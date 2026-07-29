use crate::error::SwitcherError;
use std::fs;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpectedDeviceIdentity {
    devnum: u64,
}

impl ExpectedDeviceIdentity {
    fn character(devnum: u64) -> Self {
        Self { devnum }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObservedDeviceIdentity {
    character: bool,
    devnum: u64,
}

impl ObservedDeviceIdentity {
    fn character(devnum: u64) -> Self {
        Self {
            character: true,
            devnum,
        }
    }

    #[cfg(test)]
    fn regular(devnum: u64) -> Self {
        Self {
            character: false,
            devnum,
        }
    }
}

fn identity_matches(expected: ExpectedDeviceIdentity, observed: ObservedDeviceIdentity) -> bool {
    observed.character && observed.devnum == expected.devnum
}

fn normalized_device_seat(seat: Option<&str>) -> &str {
    seat.filter(|seat| !seat.is_empty()).unwrap_or("seat0")
}

fn verify_authorized_seat(observed_seat: &str, authorized_seat: &str) -> Result<(), SwitcherError> {
    if observed_seat == authorized_seat {
        Ok(())
    } else {
        Err(SwitcherError::InputDeviceSeatMismatch)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifiedInputDevice {
    pub(crate) canonical_path: PathBuf,
    pub(crate) devnum: u64,
    pub(crate) seat: Arc<str>,
}

pub(crate) fn verify_input_device(
    path: &Path,
    authorized_seat: &str,
) -> Result<VerifiedInputDevice, SwitcherError> {
    let canonical_path = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical_path)?;
    if !metadata.file_type().is_char_device() {
        return Err(SwitcherError::InputDeviceIdentityUnverified);
    }
    let devnum = metadata.rdev();

    let context = libudev::Context::new().map_err(udev_error)?;
    let mut enumerator = libudev::Enumerator::new(&context).map_err(udev_error)?;
    enumerator.match_is_initialized().map_err(udev_error)?;
    enumerator.match_subsystem("input").map_err(udev_error)?;
    let mut devices = enumerator.scan_devices().map_err(udev_error)?;
    let device = devices
        .find(|device| device.devnum().map(|value| value as u64) == Some(devnum))
        .ok_or(SwitcherError::InputDeviceIdentityUnverified)?;

    if !device.is_initialized() {
        return Err(SwitcherError::InputDeviceIdentityUnverified);
    }
    let udev_path = device
        .devnode()
        .ok_or(SwitcherError::InputDeviceIdentityUnverified)?;
    let canonical_udev_path = fs::canonicalize(udev_path)?;
    if canonical_udev_path != canonical_path {
        return Err(SwitcherError::InputDeviceIdentityUnverified);
    }

    let seat = match device.property_value("ID_SEAT") {
        None => normalized_device_seat(None),
        Some(value) => value
            .to_str()
            .filter(|value| !value.is_empty())
            .ok_or(SwitcherError::InputDeviceIdentityUnverified)?,
    };
    verify_authorized_seat(seat, authorized_seat)?;

    Ok(VerifiedInputDevice {
        canonical_path,
        devnum,
        seat: Arc::from(seat),
    })
}

pub(crate) fn verify_open_device_identity(
    device: &impl AsRawFd,
    expected: &VerifiedInputDevice,
) -> Result<(), SwitcherError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe { libc::fstat(device.as_raw_fd(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let stat = unsafe { stat.assume_init() };
    let observed = ObservedDeviceIdentity {
        character: (stat.st_mode & libc::S_IFMT) == libc::S_IFCHR,
        devnum: stat.st_rdev as u64,
    };

    if identity_matches(ExpectedDeviceIdentity::character(expected.devnum), observed) {
        Ok(())
    } else {
        Err(SwitcherError::InputDeviceIdentityUnverified)
    }
}

fn udev_error(error: libudev::Error) -> SwitcherError {
    SwitcherError::Io(std::io::Error::other(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SwitcherError;

    #[test]
    fn missing_id_seat_defaults_to_seat_zero() {
        assert_eq!(normalized_device_seat(None), "seat0");
    }

    #[test]
    fn device_from_other_seat_is_rejected() {
        assert!(matches!(
            verify_authorized_seat("seat1", "seat0"),
            Err(SwitcherError::InputDeviceSeatMismatch)
        ));
    }

    #[test]
    fn non_character_and_changed_devnum_are_rejected() {
        assert!(!identity_matches(
            ExpectedDeviceIdentity::character(0x0d05),
            ObservedDeviceIdentity::regular(0x0d05)
        ));
        assert!(!identity_matches(
            ExpectedDeviceIdentity::character(0x0d05),
            ObservedDeviceIdentity::character(0x0d06)
        ));
    }
}
