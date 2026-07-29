use crate::daemon::xtest_guardian::protocol::{PreparedToken, ServerEpoch};
use crate::daemon::xtest_guardian::service::{X11ServerIdentity, XtestExecutor};
use crate::error::{InputSafetyError, SwitcherError};
use std::fs::File;
use std::io::{self, Read};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, PropMode, WindowClass, KEY_PRESS_EVENT,
    KEY_RELEASE_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub(crate) const EPOCH_NONCE_BYTES: usize = 16;
pub(crate) const EPOCH_RANDOM_BYTES: usize = 32;
pub(crate) const X11_EVDEV_KEYCODE_OFFSET: u16 = 8;

const EPOCH_PROPERTY_NAME: &[u8] = b"_OPEN_SWITCHER_XTEST_GUARDIAN_EPOCH_V1";
const EPOCH_PROPERTY_LONG_LENGTH: u32 = (EPOCH_NONCE_BYTES / 4) as u32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct X11SetupIdentity {
    pub(crate) root: u32,
    pub(crate) min_keycode: u8,
    pub(crate) max_keycode: u8,
    pub(crate) fingerprint: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CheckedX11Mutation(());

pub(crate) trait X11ConnectionBoundary {
    fn setup_identity(&self) -> &X11SetupIdentity;
    fn create_epoch_marker(&mut self, nonce: [u8; EPOCH_NONCE_BYTES])
        -> Result<u32, SwitcherError>;
    fn read_epoch_marker(&mut self, window: u32) -> Result<[u8; EPOCH_NONCE_BYTES], SwitcherError>;
    fn keyboard_mapping(&mut self, keycode: u8) -> Result<Vec<u32>, SwitcherError>;
    fn checked_fake_key(
        &mut self,
        keycode: u8,
        pressed: bool,
    ) -> Result<CheckedX11Mutation, SwitcherError>;
    fn round_trip(&mut self) -> Result<(), SwitcherError>;
}

pub(crate) struct RustX11Connection {
    connection: RustConnection,
    setup: X11SetupIdentity,
}

impl RustX11Connection {
    fn connect_with_xtest() -> Result<Self, SwitcherError> {
        let (connection, screen_number) =
            x11rb::connect(None).map_err(|error| x11_error("connect", error))?;
        connection
            .xtest_get_version(2, 2)
            .map_err(|error| x11_error("query XTEST version", error))?
            .reply()
            .map_err(|error| x11_error("read XTEST version", error))?;

        let setup = connection.setup();
        let screen = setup.roots.get(screen_number).ok_or_else(|| {
            SwitcherError::input_safety("X11 setup does not contain the selected screen")
        })?;
        let mut fingerprint = Vec::with_capacity(64 + setup.vendor.len());
        fingerprint.extend_from_slice(&setup.protocol_major_version.to_be_bytes());
        fingerprint.extend_from_slice(&setup.protocol_minor_version.to_be_bytes());
        fingerprint.extend_from_slice(&setup.release_number.to_be_bytes());
        fingerprint.extend_from_slice(&setup.motion_buffer_size.to_be_bytes());
        fingerprint.extend_from_slice(&setup.maximum_request_length.to_be_bytes());
        fingerprint.push(u8::from(setup.image_byte_order));
        fingerprint.push(u8::from(setup.bitmap_format_bit_order));
        fingerprint.push(setup.bitmap_format_scanline_unit);
        fingerprint.push(setup.bitmap_format_scanline_pad);
        fingerprint.push(setup.min_keycode);
        fingerprint.push(setup.max_keycode);
        fingerprint.extend_from_slice(&(setup.vendor.len() as u64).to_be_bytes());
        fingerprint.extend_from_slice(&setup.vendor);
        fingerprint.extend_from_slice(&screen.root.to_be_bytes());
        fingerprint.extend_from_slice(&screen.root_visual.to_be_bytes());
        fingerprint.extend_from_slice(&screen.width_in_pixels.to_be_bytes());
        fingerprint.extend_from_slice(&screen.height_in_pixels.to_be_bytes());
        fingerprint.extend_from_slice(&screen.width_in_millimeters.to_be_bytes());
        fingerprint.extend_from_slice(&screen.height_in_millimeters.to_be_bytes());
        fingerprint.push(screen.root_depth);
        let root = screen.root;
        let min_keycode = setup.min_keycode;
        let max_keycode = setup.max_keycode;

        Ok(Self {
            connection,
            setup: X11SetupIdentity {
                root,
                min_keycode,
                max_keycode,
                fingerprint,
            },
        })
    }
}

fn epoch_atom(connection: &RustConnection) -> Result<u32, SwitcherError> {
    Ok(connection
        .intern_atom(false, EPOCH_PROPERTY_NAME)
        .map_err(|error| x11_error("intern guardian epoch atom", error))?
        .reply()
        .map_err(|error| x11_error("read guardian epoch atom", error))?
        .atom)
}

fn read_epoch_marker_from(
    connection: &RustConnection,
    window: u32,
) -> Result<[u8; EPOCH_NONCE_BYTES], SwitcherError> {
    let atom = epoch_atom(connection)?;
    let property = connection
        .get_property(
            false,
            window,
            atom,
            AtomEnum::ANY,
            0,
            EPOCH_PROPERTY_LONG_LENGTH,
        )
        .map_err(|error| x11_error("query guardian epoch property", error))?
        .reply()
        .map_err(|error| x11_error("read guardian epoch property", error))?;
    if property.type_ != u32::from(AtomEnum::STRING)
        || property.format != 8
        || property.bytes_after != 0
        || property.value.len() != EPOCH_NONCE_BYTES
    {
        return Err(SwitcherError::input_safety(
            "X11 guardian epoch property has an invalid type or length",
        ));
    }
    let mut nonce = [0; EPOCH_NONCE_BYTES];
    nonce.copy_from_slice(&property.value);
    Ok(nonce)
}

pub(crate) fn verify_external_x11_connection(
    connection: &RustConnection,
    screen_number: usize,
    identity: &X11ServerIdentity,
) -> Result<(), SwitcherError> {
    let root = connection
        .setup()
        .roots
        .get(screen_number)
        .ok_or_else(|| {
            SwitcherError::input_safety("X11 setup does not contain the selected screen")
        })?
        .root;
    if root != identity.root {
        return Err(SwitcherError::input_safety(
            "X11 connection root does not match guardian server identity",
        ));
    }
    if read_epoch_marker_from(connection, identity.epoch_window)? != identity.epoch_nonce {
        return Err(SwitcherError::input_safety(
            "X11 guardian epoch property does not match the expected identity",
        ));
    }
    Ok(())
}

impl X11ConnectionBoundary for RustX11Connection {
    fn setup_identity(&self) -> &X11SetupIdentity {
        &self.setup
    }

    fn create_epoch_marker(
        &mut self,
        nonce: [u8; EPOCH_NONCE_BYTES],
    ) -> Result<u32, SwitcherError> {
        let window = self
            .connection
            .generate_id()
            .map_err(|error| x11_error("allocate guardian epoch window", error))?;
        let atom = epoch_atom(&self.connection)?;
        self.connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                self.setup.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )
            .map_err(|error| x11_error("create guardian epoch window", error))?
            .check()
            .map_err(|error| x11_error("confirm guardian epoch window", error))?;
        self.connection
            .change_property8(PropMode::REPLACE, window, atom, AtomEnum::STRING, &nonce)
            .map_err(|error| x11_error("write guardian epoch property", error))?
            .check()
            .map_err(|error| x11_error("confirm guardian epoch property", error))?;
        self.connection
            .flush()
            .map_err(|error| x11_error("flush guardian epoch property", error))?;
        Ok(window)
    }

    fn read_epoch_marker(&mut self, window: u32) -> Result<[u8; EPOCH_NONCE_BYTES], SwitcherError> {
        read_epoch_marker_from(&self.connection, window)
    }

    fn keyboard_mapping(&mut self, keycode: u8) -> Result<Vec<u32>, SwitcherError> {
        Ok(self
            .connection
            .get_keyboard_mapping(keycode, 1)
            .map_err(|error| x11_error("query X11 keyboard mapping", error))?
            .reply()
            .map_err(|error| x11_error("read X11 keyboard mapping", error))?
            .keysyms)
    }

    fn checked_fake_key(
        &mut self,
        keycode: u8,
        pressed: bool,
    ) -> Result<CheckedX11Mutation, SwitcherError> {
        self.connection
            .xtest_fake_input(
                if pressed {
                    KEY_PRESS_EVENT
                } else {
                    KEY_RELEASE_EVENT
                },
                keycode,
                x11rb::CURRENT_TIME,
                self.setup.root,
                0,
                0,
                0,
            )
            .map_err(|error| x11_error("send XTEST key mutation", error))?
            .check()
            .map_err(|error| x11_error("confirm XTEST key mutation", error))?;
        self.connection
            .flush()
            .map_err(|error| x11_error("flush XTEST key mutation", error))?;
        Ok(CheckedX11Mutation(()))
    }

    fn round_trip(&mut self) -> Result<(), SwitcherError> {
        self.connection
            .get_input_focus()
            .map_err(|error| x11_error("start X11 synchronization", error))?
            .reply()
            .map_err(|error| x11_error("complete X11 synchronization", error))?;
        self.connection
            .flush()
            .map_err(|error| x11_error("flush X11 synchronization", error))
    }
}

fn x11_error(context: &'static str, error: impl std::fmt::Display) -> SwitcherError {
    SwitcherError::Io(io::Error::other(format!("{context}: {error}")))
}

fn executor_error(context: &'static str) -> InputSafetyError {
    InputSafetyError::Invariant { context }
}

fn derive_server_epoch(seed: [u8; 16], fingerprint: &[u8]) -> ServerEpoch {
    let mut epoch = seed;
    for (offset, byte) in fingerprint.iter().copied().enumerate() {
        let first = offset % epoch.len();
        let second = (offset.wrapping_mul(7).wrapping_add(5)) % epoch.len();
        epoch[first] ^= byte.rotate_left((offset % 8) as u32);
        epoch[second] = epoch[second].wrapping_add(byte ^ (offset as u8).wrapping_mul(29));
    }
    ServerEpoch(epoch)
}

fn verify_server_identity<C: X11ConnectionBoundary>(
    connection: &mut C,
    identity: &X11ServerIdentity,
) -> Result<(), SwitcherError> {
    if identity.epoch.0.iter().all(|byte| *byte == 0)
        || identity.root == 0
        || identity.epoch_window == 0
        || identity.epoch_nonce.iter().all(|byte| *byte == 0)
    {
        return Err(SwitcherError::input_safety(
            "X11 guardian server identity contains a zero field",
        ));
    }
    if connection.setup_identity().root != identity.root {
        return Err(SwitcherError::input_safety(
            "X11 connection root does not match guardian server identity",
        ));
    }
    if connection.read_epoch_marker(identity.epoch_window)? != identity.epoch_nonce {
        return Err(SwitcherError::input_safety(
            "X11 guardian epoch property does not match the expected identity",
        ));
    }
    Ok(())
}

pub(crate) fn establish_guardian_identity<C: X11ConnectionBoundary>(
    connection: &mut C,
    random: [u8; EPOCH_RANDOM_BYTES],
) -> Result<X11ServerIdentity, SwitcherError> {
    let mut seed = [0; 16];
    seed.copy_from_slice(&random[..16]);
    let mut nonce = [0; EPOCH_NONCE_BYTES];
    nonce.copy_from_slice(&random[16..]);
    if seed.iter().all(|byte| *byte == 0) || nonce.iter().all(|byte| *byte == 0) {
        return Err(SwitcherError::input_safety(
            "X11 guardian random identity material must be nonzero",
        ));
    }
    let setup = connection.setup_identity().clone();
    if setup.root == 0
        || setup.min_keycode == 0
        || setup.min_keycode > setup.max_keycode
        || setup.fingerprint.is_empty()
    {
        return Err(SwitcherError::input_safety(
            "X11 guardian setup identity is invalid",
        ));
    }
    let epoch = derive_server_epoch(seed, &setup.fingerprint);
    if epoch.0.iter().all(|byte| *byte == 0) {
        return Err(SwitcherError::input_safety(
            "X11 guardian derived a zero server epoch",
        ));
    }
    let epoch_window = connection.create_epoch_marker(nonce)?;
    let identity = X11ServerIdentity {
        epoch,
        root: setup.root,
        epoch_window,
        epoch_nonce: nonce,
    };
    verify_server_identity(connection, &identity)?;
    Ok(identity)
}

fn read_epoch_random() -> Result<[u8; EPOCH_RANDOM_BYTES], SwitcherError> {
    let mut random = [0; EPOCH_RANDOM_BYTES];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    Ok(random)
}

pub(crate) struct GuardianX11Executor<C: X11ConnectionBoundary = RustX11Connection> {
    connection: C,
    identity: X11ServerIdentity,
    pending_confirmation: Option<CheckedX11Mutation>,
}

impl GuardianX11Executor<RustX11Connection> {
    pub(crate) fn connect_and_establish() -> Result<Self, SwitcherError> {
        let mut connection = RustX11Connection::connect_with_xtest()?;
        let identity = establish_guardian_identity(&mut connection, read_epoch_random()?)?;
        Self::from_connection(connection, identity)
    }
}

impl<C: X11ConnectionBoundary> GuardianX11Executor<C> {
    pub(crate) fn from_connection(
        mut connection: C,
        identity: X11ServerIdentity,
    ) -> Result<Self, SwitcherError> {
        verify_server_identity(&mut connection, &identity)?;
        Ok(Self {
            connection,
            identity,
            pending_confirmation: None,
        })
    }

    fn checked_mutation(
        &mut self,
        keycode: u8,
        pressed: bool,
        failure: &'static str,
    ) -> Result<(), InputSafetyError> {
        self.pending_confirmation = None;
        let confirmation = self
            .connection
            .checked_fake_key(keycode, pressed)
            .map_err(|_| executor_error(failure))?;
        self.pending_confirmation = Some(confirmation);
        Ok(())
    }

    pub(crate) fn connection_ref(&self) -> &C {
        &self.connection
    }
}

impl<C: X11ConnectionBoundary> XtestExecutor for GuardianX11Executor<C> {
    fn server_identity(&self) -> &X11ServerIdentity {
        &self.identity
    }

    fn prepare_key(&mut self, evdev_code: u16) -> Result<(u8, ServerEpoch), InputSafetyError> {
        if evdev_code == 0 {
            return Err(executor_error(
                "XTEST guardian evdev key code must be nonzero",
            ));
        }
        let raw = evdev_code
            .checked_add(X11_EVDEV_KEYCODE_OFFSET)
            .ok_or_else(|| executor_error("XTEST guardian key code conversion overflowed"))?;
        let keycode = u8::try_from(raw)
            .map_err(|_| executor_error("XTEST guardian key code is outside the X11 range"))?;
        let setup = self.connection.setup_identity();
        if keycode < setup.min_keycode || keycode > setup.max_keycode {
            return Err(executor_error(
                "XTEST guardian key code is outside the server mapping range",
            ));
        }
        let mapping = self
            .connection
            .keyboard_mapping(keycode)
            .map_err(|_| executor_error("XTEST guardian could not read the keyboard mapping"))?;
        if mapping.is_empty() || mapping.iter().all(|keysym| *keysym == 0) {
            return Err(executor_error(
                "XTEST guardian key code has no current keyboard mapping",
            ));
        }
        Ok((keycode, self.identity.epoch))
    }

    fn key_down(&mut self, keycode: u8) -> Result<(), InputSafetyError> {
        self.checked_mutation(keycode, true, "XTEST guardian key-down failed")
    }

    fn key_up(&mut self, keycode: u8) -> Result<(), InputSafetyError> {
        self.checked_mutation(keycode, false, "XTEST guardian key-up failed")
    }

    fn synchronize(&mut self) -> Result<(), InputSafetyError> {
        self.pending_confirmation
            .take()
            .map(|_| ())
            .ok_or_else(|| executor_error("XTEST guardian synchronization has no checked mutation"))
    }
}

pub(crate) struct EmergencyX11Releaser<C: X11ConnectionBoundary = RustX11Connection> {
    connection: C,
    identity: X11ServerIdentity,
}

impl EmergencyX11Releaser<RustX11Connection> {
    pub(crate) fn connect_and_verify(identity: X11ServerIdentity) -> Result<Self, SwitcherError> {
        Self::verify_connection(RustX11Connection::connect_with_xtest()?, identity)
    }
}

impl<C: X11ConnectionBoundary> EmergencyX11Releaser<C> {
    pub(crate) fn verify_connection(
        mut connection: C,
        identity: X11ServerIdentity,
    ) -> Result<Self, SwitcherError> {
        verify_server_identity(&mut connection, &identity)?;
        Ok(Self {
            connection,
            identity,
        })
    }

    pub(crate) fn release_token(&mut self, token: PreparedToken) -> Result<(), SwitcherError> {
        if token.epoch != self.identity.epoch
            || token.session.0.iter().all(|byte| *byte == 0)
            || token.token_id == 0
            || token.evdev_code == 0
        {
            return Err(SwitcherError::input_safety(
                "emergency XTEST token does not belong to the verified server epoch",
            ));
        }
        let expected_keycode = token
            .evdev_code
            .checked_add(X11_EVDEV_KEYCODE_OFFSET)
            .and_then(|raw| u8::try_from(raw).ok())
            .ok_or_else(|| {
                SwitcherError::input_safety("emergency XTEST token key code is invalid")
            })?;
        if token.x11_keycode != expected_keycode {
            return Err(SwitcherError::input_safety(
                "emergency XTEST token key code does not match its evdev code",
            ));
        }
        self.connection
            .checked_fake_key(token.x11_keycode, false)
            .map(|_| ())
    }

    pub(crate) fn synchronize(&mut self) -> Result<(), SwitcherError> {
        self.connection.round_trip()
    }

    pub(crate) fn server_identity(&self) -> &X11ServerIdentity {
        &self.identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::xtest_guardian::protocol::{PreparedToken, ServerEpoch, SessionId};
    use crate::daemon::xtest_guardian::service::{X11ServerIdentity, XtestExecutor};
    use crate::error::SwitcherError;
    use std::collections::BTreeMap;

    struct FakeX11Connection {
        setup: X11SetupIdentity,
        mappings: BTreeMap<u8, Vec<u32>>,
        marker: Option<(u32, [u8; EPOCH_NONCE_BYTES])>,
        next_window: u32,
        fake_events: Vec<(u8, bool)>,
        fail_fake_event_number: Option<usize>,
        fail_round_trip: bool,
        round_trips: usize,
    }

    impl Default for FakeX11Connection {
        fn default() -> Self {
            Self {
                setup: X11SetupIdentity {
                    root: 1,
                    min_keycode: 8,
                    max_keycode: 255,
                    fingerprint: b"fake-x11-server".to_vec(),
                },
                mappings: BTreeMap::new(),
                marker: None,
                next_window: 2,
                fake_events: Vec::new(),
                fail_fake_event_number: None,
                fail_round_trip: false,
                round_trips: 0,
            }
        }
    }

    impl FakeX11Connection {
        fn with_mapping(evdev_code: u16, keycode: u8) -> Self {
            let mut connection = Self::default();
            assert_eq!(evdev_code + X11_EVDEV_KEYCODE_OFFSET, u16::from(keycode));
            connection.mappings.insert(keycode, vec![0x61]);
            connection
        }

        fn with_identity(mut self, identity: &X11ServerIdentity) -> Self {
            self.setup.root = identity.root;
            self.marker = Some((identity.epoch_window, identity.epoch_nonce));
            self
        }
    }

    impl X11ConnectionBoundary for FakeX11Connection {
        fn setup_identity(&self) -> &X11SetupIdentity {
            &self.setup
        }

        fn create_epoch_marker(
            &mut self,
            nonce: [u8; EPOCH_NONCE_BYTES],
        ) -> Result<u32, SwitcherError> {
            let window = self.next_window;
            self.next_window += 1;
            self.marker = Some((window, nonce));
            Ok(window)
        }

        fn read_epoch_marker(
            &mut self,
            window: u32,
        ) -> Result<[u8; EPOCH_NONCE_BYTES], SwitcherError> {
            match self.marker {
                Some((marker_window, nonce)) if marker_window == window => Ok(nonce),
                _ => Err(SwitcherError::input_safety(
                    "fake X11 epoch marker is unavailable",
                )),
            }
        }

        fn keyboard_mapping(&mut self, keycode: u8) -> Result<Vec<u32>, SwitcherError> {
            Ok(self.mappings.get(&keycode).cloned().unwrap_or_default())
        }

        fn checked_fake_key(
            &mut self,
            keycode: u8,
            pressed: bool,
        ) -> Result<CheckedX11Mutation, SwitcherError> {
            self.fake_events.push((keycode, pressed));
            if self.fail_fake_event_number == Some(self.fake_events.len()) {
                return Err(SwitcherError::input_safety(
                    "fake X11 checked-mutation failure",
                ));
            }
            Ok(CheckedX11Mutation(()))
        }

        fn round_trip(&mut self) -> Result<(), SwitcherError> {
            self.round_trips += 1;
            if self.fail_round_trip {
                Err(SwitcherError::input_safety("fake X11 round-trip failure"))
            } else {
                Ok(())
            }
        }
    }

    fn test_server_identity() -> X11ServerIdentity {
        X11ServerIdentity {
            epoch: ServerEpoch([0x31; 16]),
            root: 1,
            epoch_window: 2,
            epoch_nonce: [0x32; EPOCH_NONCE_BYTES],
        }
    }

    fn test_token(identity: &X11ServerIdentity) -> PreparedToken {
        PreparedToken {
            session: SessionId([0x33; 16]),
            epoch: identity.epoch,
            token_id: 1,
            evdev_code: 30,
            x11_keycode: 38,
        }
    }

    #[test]
    fn prepared_token_contains_validated_keycode_and_current_epoch() {
        let identity = test_server_identity();
        let connection = FakeX11Connection::with_mapping(30, 38).with_identity(&identity);
        let mut executor =
            GuardianX11Executor::from_connection(connection, identity.clone()).unwrap();

        let prepared = executor.prepare_key(30).unwrap();

        assert_eq!(prepared, (38, identity.epoch));
    }

    #[test]
    fn emergency_connection_rejects_mismatched_epoch_property() {
        let expected = test_server_identity();
        let mut connection = FakeX11Connection::default().with_identity(&expected);
        connection.marker = Some((expected.epoch_window, [0xEE; EPOCH_NONCE_BYTES]));

        assert!(EmergencyX11Releaser::verify_connection(connection, expected).is_err());
    }

    #[test]
    fn checked_mutation_satisfies_sync_without_second_round_trip() {
        let identity = test_server_identity();
        let connection = FakeX11Connection::default().with_identity(&identity);
        let mut executor = GuardianX11Executor::from_connection(connection, identity).unwrap();

        executor.key_up(38).unwrap();
        executor.synchronize().unwrap();

        assert_eq!(executor.connection_ref().fake_events, [(38, false)]);
        assert_eq!(executor.connection_ref().round_trips, 0);
    }

    #[test]
    fn checked_mutation_confirmation_is_consumed_once() {
        let identity = test_server_identity();
        let connection = FakeX11Connection::default().with_identity(&identity);
        let mut executor = GuardianX11Executor::from_connection(connection, identity).unwrap();

        executor.key_up(38).unwrap();
        executor.synchronize().unwrap();

        assert!(executor.synchronize().is_err());
        assert_eq!(executor.connection_ref().round_trips, 0);
    }

    #[test]
    fn failed_new_mutation_cannot_reuse_stale_confirmation() {
        let identity = test_server_identity();
        let connection = FakeX11Connection {
            fail_fake_event_number: Some(2),
            ..FakeX11Connection::default().with_identity(&identity)
        };
        let mut executor = GuardianX11Executor::from_connection(connection, identity).unwrap();

        executor.key_up(38).unwrap();
        assert!(executor.key_up(39).is_err());

        assert!(executor.synchronize().is_err());
        assert_eq!(
            executor.connection_ref().fake_events,
            [(38, false), (39, false)]
        );
    }

    #[test]
    fn emergency_releaser_uses_only_preverified_keycode_and_explicit_round_trip() {
        let identity = test_server_identity();
        let connection = FakeX11Connection::default().with_identity(&identity);
        let mut releaser =
            EmergencyX11Releaser::verify_connection(connection, identity.clone()).unwrap();

        releaser.release_token(test_token(&identity)).unwrap();
        releaser.synchronize().unwrap();

        assert_eq!(releaser.server_identity(), &identity);
        assert_eq!(releaser.connection.fake_events, [(38, false)]);
        assert_eq!(releaser.connection.round_trips, 1);
    }

    #[test]
    fn emergency_releaser_rejects_stale_epoch_and_forged_keycode() {
        let identity = test_server_identity();
        let connection = FakeX11Connection::default().with_identity(&identity);
        let mut releaser =
            EmergencyX11Releaser::verify_connection(connection, identity.clone()).unwrap();
        let mut token = test_token(&identity);
        token.epoch = ServerEpoch([0xEE; 16]);
        assert!(releaser.release_token(token).is_err());

        let mut token = test_token(&identity);
        token.x11_keycode = 39;
        assert!(releaser.release_token(token).is_err());
        assert!(releaser.connection.fake_events.is_empty());
    }

    #[test]
    fn guardian_epoch_marker_is_verified_and_mixes_setup_fingerprint() {
        let mut connection = FakeX11Connection::default();
        let random = [0x41; EPOCH_RANDOM_BYTES];
        let identity = establish_guardian_identity(&mut connection, random).unwrap();

        assert_eq!(identity.root, 1);
        assert_eq!(identity.epoch_window, 2);
        assert_eq!(identity.epoch_nonce, [0x41; EPOCH_NONCE_BYTES]);
        assert_ne!(identity.epoch, ServerEpoch([0x41; 16]));
        assert_eq!(
            connection.read_epoch_marker(identity.epoch_window).unwrap(),
            identity.epoch_nonce
        );
    }

    #[test]
    fn empty_or_out_of_range_mapping_is_rejected_before_fake_input() {
        let identity = test_server_identity();
        let connection = FakeX11Connection::default().with_identity(&identity);
        let mut executor =
            GuardianX11Executor::from_connection(connection, identity.clone()).unwrap();
        assert!(executor.prepare_key(30).is_err());

        let mut connection = FakeX11Connection::with_mapping(30, 38).with_identity(&identity);
        connection.setup.min_keycode = 40;
        let mut executor = GuardianX11Executor::from_connection(connection, identity).unwrap();
        assert!(executor.prepare_key(30).is_err());
        assert!(executor.connection_ref().fake_events.is_empty());
    }
}
