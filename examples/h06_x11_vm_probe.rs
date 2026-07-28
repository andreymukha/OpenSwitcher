use nix::unistd::geteuid;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;
use x11rb::connection::Connection as _;
use x11rb::protocol::xinput::{ConnectionExt as _, Device, EventMask as XiEventMask, XIEventMask};
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

const CONFIRMATION_ARGUMENT: &str = "--confirm-openswitcher-vm-lab";
const LAB_GUEST_MARKER: &str = "/etc/openswitcher-lab-guest";
const DMI_PRODUCT_NAME: &str = "/sys/class/dmi/id/product_name";
const DMI_SYS_VENDOR: &str = "/sys/class/dmi/id/sys_vendor";
const EXPECTED_TARGET_EXE: &str = "open-switcher-daemon";
const MAX_EVIDENCE_RECORDS: usize = 16_384;
const MAX_ASSERT_TIMEOUT_MS: u64 = 60_000;
const QUERY_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Error, Clone, PartialEq, Eq)]
enum ProbeError {
    #[error("explicit OpenSwitcher VM lab confirmation is required")]
    ConfirmationRequired,
    #[error("the current machine is not an authenticated OpenSwitcher QEMU/KVM guest")]
    NotOpenSwitcherVm,
    #[error("invalid command line: {0}")]
    InvalidArguments(String),
    #[error("target process is not the current user's open-switcher-daemon")]
    InvalidTarget,
    #[error("target key remained down until the assertion timeout")]
    KeyStillDown,
    #[error("bounded evidence limit was reached")]
    EvidenceLimit,
    #[error("I/O failure: {0}")]
    Io(String),
    #[error("X11 failure: {0}")]
    X11(String),
    #[error("pidfd failure: {0}")]
    PidFd(String),
}

impl From<io::Error> for ProbeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cli {
    confirmed: bool,
    mode: ProbeMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeMode {
    Observe {
        target_keycode: u8,
        output: PathBuf,
    },
    KillOnPress {
        target_keycode: u8,
        pid: u32,
        output: PathBuf,
    },
    AssertKeyUp {
        target_keycode: u8,
        timeout: Duration,
        output: PathBuf,
    },
}

fn parse_cli<I, S>(args: I) -> Result<Cli, ProbeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut arguments = args
        .into_iter()
        .skip(1)
        .map(|argument| argument.as_ref().to_owned())
        .collect::<Vec<_>>();
    if arguments.first().map(String::as_str) != Some(CONFIRMATION_ARGUMENT) {
        return Err(ProbeError::ConfirmationRequired);
    }
    arguments.remove(0);

    let mode_name = arguments
        .first()
        .cloned()
        .ok_or_else(|| ProbeError::InvalidArguments("missing mode".to_owned()))?;
    arguments.remove(0);
    let mut options = parse_options(&arguments)?;
    let target_keycode = parse_target_keycode(take_required(&mut options, "--target-keycode")?)?;
    let output = PathBuf::from(take_required(&mut options, "--output")?);
    if output.as_os_str().is_empty() {
        return Err(ProbeError::InvalidArguments(
            "--output must not be empty".to_owned(),
        ));
    }

    let mode = match mode_name.as_str() {
        "observe" => ProbeMode::Observe {
            target_keycode,
            output,
        },
        "kill-on-press" => {
            let pid = take_required(&mut options, "--pid")?
                .parse::<u32>()
                .map_err(|_| ProbeError::InvalidArguments("invalid --pid".to_owned()))?;
            if pid == 0 || pid > i32::MAX as u32 {
                return Err(ProbeError::InvalidArguments(
                    "--pid is outside the supported process range".to_owned(),
                ));
            }
            ProbeMode::KillOnPress {
                target_keycode,
                pid,
                output,
            }
        }
        "assert-key-up" => {
            let timeout_ms = take_required(&mut options, "--timeout-ms")?
                .parse::<u64>()
                .map_err(|_| ProbeError::InvalidArguments("invalid --timeout-ms".to_owned()))?;
            if timeout_ms == 0 || timeout_ms > MAX_ASSERT_TIMEOUT_MS {
                return Err(ProbeError::InvalidArguments(format!(
                    "--timeout-ms must be within 1..={MAX_ASSERT_TIMEOUT_MS}"
                )));
            }
            ProbeMode::AssertKeyUp {
                target_keycode,
                timeout: Duration::from_millis(timeout_ms),
                output,
            }
        }
        _ => {
            return Err(ProbeError::InvalidArguments(format!(
                "unknown mode: {mode_name}"
            )))
        }
    };

    if let Some(unexpected) = options.keys().next() {
        return Err(ProbeError::InvalidArguments(format!(
            "unexpected option for {mode_name}: {unexpected}"
        )));
    }

    Ok(Cli {
        confirmed: true,
        mode,
    })
}

fn parse_options(arguments: &[String]) -> Result<BTreeMap<String, String>, ProbeError> {
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < arguments.len() {
        let option = &arguments[index];
        if !option.starts_with("--") {
            return Err(ProbeError::InvalidArguments(format!(
                "unexpected positional argument: {option}"
            )));
        }
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| ProbeError::InvalidArguments(format!("missing value for {option}")))?;
        if value.starts_with("--") {
            return Err(ProbeError::InvalidArguments(format!(
                "missing value for {option}"
            )));
        }
        if options.insert(option.clone(), value.clone()).is_some() {
            return Err(ProbeError::InvalidArguments(format!(
                "duplicate option: {option}"
            )));
        }
        index += 2;
    }
    Ok(options)
}

fn take_required(
    options: &mut BTreeMap<String, String>,
    option: &str,
) -> Result<String, ProbeError> {
    options
        .remove(option)
        .ok_or_else(|| ProbeError::InvalidArguments(format!("missing {option}")))
}

fn parse_target_keycode(value: String) -> Result<u8, ProbeError> {
    let keycode = value
        .parse::<u16>()
        .map_err(|_| ProbeError::InvalidArguments("invalid --target-keycode".to_owned()))?;
    if !(8..=255).contains(&keycode) {
        return Err(ProbeError::InvalidArguments(
            "--target-keycode must be within 8..=255".to_owned(),
        ));
    }
    Ok(keycode as u8)
}

trait VmBoundaryEnvironment {
    fn confirmation_present(&self) -> bool;
    fn marker_is_root_owned_regular(&self) -> Result<bool, ProbeError>;
    fn dmi_product_name(&self) -> Result<String, ProbeError>;
    fn dmi_sys_vendor(&self) -> Result<String, ProbeError>;
}

fn validate_vm_boundary(environment: &impl VmBoundaryEnvironment) -> Result<(), ProbeError> {
    if !environment.confirmation_present() {
        return Err(ProbeError::ConfirmationRequired);
    }
    if !environment.marker_is_root_owned_regular()? {
        return Err(ProbeError::NotOpenSwitcherVm);
    }

    let dmi_identity = format!(
        "{} {}",
        environment.dmi_product_name()?,
        environment.dmi_sys_vendor()?
    )
    .to_ascii_lowercase();
    if !dmi_identity.contains("qemu") && !dmi_identity.contains("kvm") {
        return Err(ProbeError::NotOpenSwitcherVm);
    }
    Ok(())
}

struct RealEnvironment {
    confirmed: bool,
}

impl VmBoundaryEnvironment for RealEnvironment {
    fn confirmation_present(&self) -> bool {
        self.confirmed
    }

    fn marker_is_root_owned_regular(&self) -> Result<bool, ProbeError> {
        match fs::symlink_metadata(LAB_GUEST_MARKER) {
            Ok(metadata) => Ok(metadata.file_type().is_file() && metadata.uid() == 0),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn dmi_product_name(&self) -> Result<String, ProbeError> {
        fs::read_to_string(DMI_PRODUCT_NAME).map_err(ProbeError::from)
    }

    fn dmi_sys_vendor(&self) -> Result<String, ProbeError> {
        fs::read_to_string(DMI_SYS_VENDOR).map_err(ProbeError::from)
    }
}

struct TargetProcess {
    pidfd: OwnedFd,
}

impl TargetProcess {
    fn pin(pid: u32) -> Result<Self, ProbeError> {
        let raw_pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0_u32) };
        if raw_pidfd < 0 {
            return Err(ProbeError::PidFd(io::Error::last_os_error().to_string()));
        }
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw_pidfd as i32) };

        let process_path = PathBuf::from(format!("/proc/{pid}"));
        let metadata = fs::metadata(&process_path).map_err(|_| ProbeError::InvalidTarget)?;
        if metadata.uid() != geteuid().as_raw() {
            return Err(ProbeError::InvalidTarget);
        }

        let executable =
            fs::read_link(process_path.join("exe")).map_err(|_| ProbeError::InvalidTarget)?;
        if executable.file_name() != Some(OsStr::new(EXPECTED_TARGET_EXE)) {
            return Err(ProbeError::InvalidTarget);
        }

        Ok(Self { pidfd })
    }

    fn send_sigkill(self) -> Result<(), ProbeError> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            )
        };
        if result < 0 {
            return Err(ProbeError::PidFd(io::Error::last_os_error().to_string()));
        }
        Ok(())
    }
}

struct EvidenceWriter {
    file: File,
    records: usize,
}

impl EvidenceWriter {
    fn create(path: &Path) -> Result<Self, ProbeError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        Ok(Self { file, records: 0 })
    }

    fn record(&mut self, kind: &str, keycode: u32, monotonic_us: u64) -> Result<(), ProbeError> {
        if self.records >= MAX_EVIDENCE_RECORDS {
            return Err(ProbeError::EvidenceLimit);
        }
        let record = format!(
            "{{\"kind\":\"{kind}\",\"keycode\":{keycode},\"monotonic_us\":{monotonic_us}}}\n"
        );
        self.file.write_all(record.as_bytes())?;
        self.records += 1;
        Ok(())
    }

    fn sync(&self) -> Result<(), ProbeError> {
        self.file.sync_data().map_err(ProbeError::from)
    }
}

struct X11Probe {
    connection: RustConnection,
    started_at: Instant,
}

impl X11Probe {
    fn connect_for_raw_keys() -> Result<Self, ProbeError> {
        let (connection, screen_number) =
            x11rb::connect(None).map_err(|error| x11_error("connect", error))?;
        let root = connection.setup().roots[screen_number].root;
        connection
            .xinput_xi_query_version(2, 0)
            .map_err(|error| x11_error("XIQueryVersion request", error))?
            .reply()
            .map_err(|error| x11_error("XIQueryVersion reply", error))?;
        let mask = XiEventMask {
            deviceid: Device::ALL_MASTER.into(),
            mask: vec![XIEventMask::RAW_KEY_PRESS | XIEventMask::RAW_KEY_RELEASE],
        };
        connection
            .xinput_xi_select_events(root, &[mask])
            .map_err(|error| x11_error("XISelectEvents request", error))?
            .check()
            .map_err(|error| x11_error("XISelectEvents check", error))?;
        connection
            .flush()
            .map_err(|error| x11_error("XISelectEvents flush", error))?;
        Ok(Self {
            connection,
            started_at: Instant::now(),
        })
    }

    fn connect_for_query_keymap() -> Result<Self, ProbeError> {
        let (connection, _) = x11rb::connect(None).map_err(|error| x11_error("connect", error))?;
        Ok(Self {
            connection,
            started_at: Instant::now(),
        })
    }

    fn wait_for_target_event(&self, target_keycode: u8) -> Result<ObservedKeyEvent, ProbeError> {
        loop {
            let event = self
                .connection
                .wait_for_event()
                .map_err(|error| x11_error("wait_for_event", error))?;
            match event {
                Event::XinputRawKeyPress(event) if event.detail == u32::from(target_keycode) => {
                    return Ok(ObservedKeyEvent::Press(event.detail));
                }
                Event::XinputRawKeyRelease(event) if event.detail == u32::from(target_keycode) => {
                    return Ok(ObservedKeyEvent::Release(event.detail));
                }
                _ => {}
            }
        }
    }

    fn query_key_down(&self, target_keycode: u8) -> Result<bool, ProbeError> {
        let reply = self
            .connection
            .query_keymap()
            .map_err(|error| x11_error("XQueryKeymap request", error))?
            .reply()
            .map_err(|error| x11_error("XQueryKeymap reply", error))?;
        Ok(key_is_down(&reply.keys, target_keycode))
    }

    fn monotonic_us(&self) -> u64 {
        self.started_at
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    }
}

enum ObservedKeyEvent {
    Press(u32),
    Release(u32),
}

fn x11_error(context: &str, error: impl std::fmt::Display) -> ProbeError {
    ProbeError::X11(format!("{context}: {error}"))
}

fn run_observe(
    x11: X11Probe,
    target_keycode: u8,
    writer: &mut EvidenceWriter,
) -> Result<(), ProbeError> {
    loop {
        match x11.wait_for_target_event(target_keycode)? {
            ObservedKeyEvent::Press(keycode) => {
                writer.record("press", keycode, x11.monotonic_us())?
            }
            ObservedKeyEvent::Release(keycode) => {
                writer.record("release", keycode, x11.monotonic_us())?
            }
        }
    }
}

fn run_kill_on_press(
    x11: X11Probe,
    target_keycode: u8,
    target: TargetProcess,
    writer: &mut EvidenceWriter,
) -> Result<(), ProbeError> {
    loop {
        if let ObservedKeyEvent::Press(keycode) = x11.wait_for_target_event(target_keycode)? {
            writer.record("press", keycode, x11.monotonic_us())?;
            writer.sync()?;
            target.send_sigkill()?;
            writer.record("sigkill-sent", keycode, x11.monotonic_us())?;
            writer.sync()?;
            return Ok(());
        }
    }
}

fn run_assert_key_up(
    x11: X11Probe,
    target_keycode: u8,
    timeout: Duration,
    writer: &mut EvidenceWriter,
) -> Result<(), ProbeError> {
    let deadline = Instant::now() + timeout;
    loop {
        let down = x11.query_key_down(target_keycode)?;
        writer.record(
            if down { "query-down" } else { "query-up" },
            u32::from(target_keycode),
            x11.monotonic_us(),
        )?;
        if !down {
            writer.sync()?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            writer.sync()?;
            return Err(ProbeError::KeyStillDown);
        }
        thread::sleep(QUERY_INTERVAL);
    }
}

fn key_is_down(keys: &[u8; 32], keycode: u8) -> bool {
    let keycode = usize::from(keycode);
    keys[keycode / 8] & (1 << (keycode % 8)) != 0
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeSampleKind {
    Press,
    QueryDown,
    QueryUp,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProbeSample {
    kind: ProbeSampleKind,
    keycode: u8,
    at_ms: u64,
}

#[cfg(test)]
impl ProbeSample {
    fn press(keycode: u8, at_ms: u64) -> Self {
        Self {
            kind: ProbeSampleKind::Press,
            keycode,
            at_ms,
        }
    }

    fn query_down(keycode: u8, at_ms: u64) -> Self {
        Self {
            kind: ProbeSampleKind::QueryDown,
            keycode,
            at_ms,
        }
    }

    fn query_up(keycode: u8, at_ms: u64) -> Self {
        Self {
            kind: ProbeSampleKind::QueryUp,
            keycode,
            at_ms,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeOutcome {
    Released { elapsed_ms: u64 },
    StillDown,
    PressNotObserved,
}

#[cfg(test)]
fn classify_samples(samples: &[ProbeSample], target_keycode: u8) -> ProbeOutcome {
    let Some((press_index, press)) = samples.iter().enumerate().find(|(_, sample)| {
        sample.keycode == target_keycode && sample.kind == ProbeSampleKind::Press
    }) else {
        return ProbeOutcome::PressNotObserved;
    };

    for sample in &samples[press_index + 1..] {
        if sample.keycode == target_keycode && sample.kind == ProbeSampleKind::QueryUp {
            return ProbeOutcome::Released {
                elapsed_ms: sample.at_ms.saturating_sub(press.at_ms),
            };
        }
    }
    ProbeOutcome::StillDown
}

fn run() -> Result<(), ProbeError> {
    let cli = parse_cli(std::env::args())?;
    validate_vm_boundary(&RealEnvironment {
        confirmed: cli.confirmed,
    })?;

    match cli.mode {
        ProbeMode::Observe {
            target_keycode,
            output,
        } => {
            let mut writer = EvidenceWriter::create(&output)?;
            let x11 = X11Probe::connect_for_raw_keys()?;
            run_observe(x11, target_keycode, &mut writer)
        }
        ProbeMode::KillOnPress {
            target_keycode,
            pid,
            output,
        } => {
            // pidfd связывает будущий SIGKILL с уже проверенным процессом и
            // исключает убийство постороннего процесса после повторного PID.
            let target = TargetProcess::pin(pid)?;
            let mut writer = EvidenceWriter::create(&output)?;
            let x11 = X11Probe::connect_for_raw_keys()?;
            run_kill_on_press(x11, target_keycode, target, &mut writer)
        }
        ProbeMode::AssertKeyUp {
            target_keycode,
            timeout,
            output,
        } => {
            let mut writer = EvidenceWriter::create(&output)?;
            let x11 = X11Probe::connect_for_query_keymap()?;
            run_assert_key_up(x11, target_keycode, timeout, &mut writer)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("h06_x11_vm_probe: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
struct FakeEnvironment {
    confirmed: bool,
    marker_is_root_owned_regular: bool,
    product_name: String,
    sys_vendor: String,
    x11_connects: std::cell::Cell<usize>,
    signals_sent: std::cell::Cell<usize>,
}

#[cfg(test)]
impl FakeEnvironment {
    fn host() -> Self {
        Self {
            confirmed: true,
            marker_is_root_owned_regular: false,
            product_name: "QEMU Standard PC".to_owned(),
            sys_vendor: "QEMU".to_owned(),
            x11_connects: std::cell::Cell::new(0),
            signals_sent: std::cell::Cell::new(0),
        }
    }

    fn qemu_lab_with_default_product() -> Self {
        Self {
            confirmed: true,
            marker_is_root_owned_regular: true,
            product_name: "Standard PC (Q35 + ICH9, 2009)".to_owned(),
            sys_vendor: "QEMU".to_owned(),
            x11_connects: std::cell::Cell::new(0),
            signals_sent: std::cell::Cell::new(0),
        }
    }

    fn x11_connects(&self) -> usize {
        self.x11_connects.get()
    }

    fn signals_sent(&self) -> usize {
        self.signals_sent.get()
    }
}

#[cfg(test)]
impl VmBoundaryEnvironment for FakeEnvironment {
    fn confirmation_present(&self) -> bool {
        self.confirmed
    }

    fn marker_is_root_owned_regular(&self) -> Result<bool, ProbeError> {
        Ok(self.marker_is_root_owned_regular)
    }

    fn dmi_product_name(&self) -> Result<String, ProbeError> {
        Ok(self.product_name.clone())
    }

    fn dmi_sys_vendor(&self) -> Result<String, ProbeError> {
        Ok(self.sys_vendor.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_without_lab_marker_is_rejected_before_x11_or_signal() {
        let environment = FakeEnvironment::host();

        assert_eq!(
            validate_vm_boundary(&environment),
            Err(ProbeError::NotOpenSwitcherVm),
        );
        assert_eq!(environment.x11_connects(), 0);
        assert_eq!(environment.signals_sent(), 0);
    }

    #[test]
    fn default_q35_product_is_accepted_when_sys_vendor_is_qemu() {
        let environment = FakeEnvironment::qemu_lab_with_default_product();

        assert_eq!(validate_vm_boundary(&environment), Ok(()));
    }

    #[test]
    fn target_press_is_followed_until_query_keymap_reports_up() {
        let events = [
            ProbeSample::press(22, 10),
            ProbeSample::query_down(22, 11),
            ProbeSample::query_up(22, 21),
        ];

        assert_eq!(
            classify_samples(&events, 22),
            ProbeOutcome::Released { elapsed_ms: 11 },
        );
    }

    #[test]
    fn query_keymap_checks_only_the_requested_keycode_bit() {
        let mut keys = [0_u8; 32];
        keys[22 / 8] |= 1 << (22 % 8);

        assert!(key_is_down(&keys, 22));
        assert!(!key_is_down(&keys, 21));
        assert!(!key_is_down(&keys, 23));
    }

    #[test]
    fn cli_requires_exact_confirmation_and_mode_arguments() {
        assert!(parse_cli([
            "h06_x11_vm_probe",
            "observe",
            "--target-keycode",
            "22",
            "--output",
            "/tmp/trace.jsonl",
        ])
        .is_err());

        assert!(parse_cli([
            "h06_x11_vm_probe",
            "--confirm-openswitcher-vm-lab",
            "kill-on-press",
            "--target-keycode",
            "22",
            "--pid",
            "42",
            "--output",
            "/tmp/trace.jsonl",
            "extra",
        ])
        .is_err());

        assert!(matches!(
            parse_cli([
                "h06_x11_vm_probe",
                "--confirm-openswitcher-vm-lab",
                "assert-key-up",
                "--target-keycode",
                "22",
                "--timeout-ms",
                "2000",
                "--output",
                "/tmp/trace.jsonl",
            ]),
            Ok(Cli {
                mode: ProbeMode::AssertKeyUp { .. },
                ..
            })
        ));
    }
}
