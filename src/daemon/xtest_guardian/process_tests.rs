use super::client::{
    EmergencyRelease, GuardianClient, GuardianMutationDeadline, GUARDIAN_EMERGENCY_DEADLINE,
};
use super::protocol::{
    decode_frame, encode_frame, Message, MutationDeadlineNs, ProtocolSession, ReleaseDeadline,
    Request, Response, Sequence, ServerEpoch, SessionId,
};
use super::seqpacket::Seqpacket;
use super::service::{monotonic_now_ns, run_connection, X11ServerIdentity, XtestExecutor};
use crate::daemon::synthetic_input::{InputGeneration, OperationId, TerminalProof};
use crate::error::{InputSafetyError, SwitcherError};
use evdev::Key;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const ROLE_ENV: &str = "OPEN_SWITCHER_H06_TEST_ROLE";
const FD_ENV: &str = "OPEN_SWITCHER_H06_TEST_FD";
const TRACE_ENV: &str = "OPEN_SWITCHER_H06_TEST_TRACE";
const SCENARIO_ENV: &str = "OPEN_SWITCHER_H06_TEST_SCENARIO";
const FIXTURE_TEST_NAME: &str = "daemon::xtest_guardian::process_tests::fixture_process_entry";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(3);

fn test_identity() -> X11ServerIdentity {
    X11ServerIdentity {
        epoch: ServerEpoch([0x52; 16]),
        root: 1,
        epoch_window: 2,
        epoch_nonce: [0x53; 16],
    }
}

fn test_session() -> ProtocolSession {
    ProtocolSession {
        session: SessionId([0x51; 16]),
        epoch: test_identity().epoch,
    }
}

fn append_trace(path: &Path, event: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    let record = format!("{event}\n");
    file.write_all(record.as_bytes()).unwrap();
}

struct ProcessFakeExecutor {
    identity: X11ServerIdentity,
    trace: PathBuf,
    fail_first_release: bool,
    release_attempts: usize,
}

impl XtestExecutor for ProcessFakeExecutor {
    fn server_identity(&self) -> &X11ServerIdentity {
        &self.identity
    }

    fn prepare_key(&mut self, evdev_code: u16) -> Result<(u8, ServerEpoch), InputSafetyError> {
        Ok(((evdev_code + 8) as u8, self.identity.epoch))
    }

    fn key_down(&mut self, keycode: u8) -> Result<(), InputSafetyError> {
        append_trace(&self.trace, &format!("down:{keycode}"));
        Ok(())
    }

    fn key_up(&mut self, keycode: u8) -> Result<(), InputSafetyError> {
        self.release_attempts += 1;
        append_trace(&self.trace, &format!("release-attempt:{keycode}"));
        if self.fail_first_release && self.release_attempts == 1 {
            append_trace(&self.trace, &format!("release-failed:{keycode}"));
            return Err(InputSafetyError::Invariant {
                context: "process fixture release failure",
            });
        }
        append_trace(&self.trace, &format!("release:{keycode}"));
        Ok(())
    }

    fn synchronize(&mut self) -> Result<(), InputSafetyError> {
        append_trace(&self.trace, "sync");
        Ok(())
    }
}

fn inherited_connection() -> Seqpacket {
    let raw_fd = env::var(FD_ENV).unwrap().parse().unwrap();
    unsafe { Seqpacket::from_inherited_test_fd(raw_fd).unwrap() }
}

fn run_guardian_role(scenario: &str, trace: &Path) {
    let connection = inherited_connection();
    let mut executor = ProcessFakeExecutor {
        identity: test_identity(),
        trace: trace.to_path_buf(),
        fail_first_release: scenario == "cleanup-failure",
        release_attempts: 0,
    };
    let record = run_connection(&connection, test_session(), &mut executor).unwrap();
    match record.proof {
        TerminalProof::Reconciled => {
            append_trace(
                trace,
                &format!("guardian-terminal:{:?}:reconciled", record.reason),
            );
        }
        TerminalProof::Unreconciled { remaining } => append_trace(
            trace,
            &format!(
                "guardian-terminal:{:?}:unreconciled:{remaining}",
                record.reason
            ),
        ),
        TerminalProof::OwnerGenerationDestroyed { generation } => append_trace(
            trace,
            &format!(
                "guardian-terminal:{:?}:destroyed:{generation}",
                record.reason
            ),
        ),
    }
}

fn send_request(connection: &Seqpacket, sequence: u64, request: Request) {
    let frame = encode_frame(Sequence(sequence), &Message::Request(request)).unwrap();
    connection.send_frame(&frame).unwrap();
}

fn receive_response(connection: &Seqpacket) -> Response {
    let frame = connection.recv_frame().unwrap();
    let decoded = decode_frame(&frame).unwrap();
    let Message::Response(response) = decoded.message else {
        panic!("fixture expected guardian response");
    };
    response
}

fn wait_forever() -> ! {
    loop {
        std::thread::park();
    }
}

struct ProcessFakeEmergency {
    epoch: ServerEpoch,
    trace: PathBuf,
}

impl EmergencyRelease for ProcessFakeEmergency {
    fn server_epoch(&self) -> ServerEpoch {
        self.epoch
    }

    fn release_token(
        &mut self,
        token: super::protocol::PreparedToken,
    ) -> Result<(), SwitcherError> {
        append_trace(&self.trace, &format!("emergency-up:{}", token.x11_keycode));
        Ok(())
    }

    fn synchronize(&mut self) -> Result<(), SwitcherError> {
        append_trace(&self.trace, "emergency-sync");
        Ok(())
    }
}

fn run_daemon_guardian_sigkill_role(trace: &Path) {
    let connection = inherited_connection();
    let mut client =
        GuardianClient::from_test_connection(connection, Instant::now() + Duration::from_secs(1))
            .unwrap();
    let epoch = client.ready().epoch;
    client
        .arm_emergency(ProcessFakeEmergency {
            epoch,
            trace: trace.to_path_buf(),
        })
        .unwrap();
    let deadline =
        GuardianMutationDeadline::from_instant(Instant::now() + Duration::from_secs(2)).unwrap();
    let token = client
        .prepare_key(OperationId(1), Key::KEY_A, deadline)
        .unwrap();
    if client.execute_down(OperationId(1), token, deadline).is_ok() {
        let _ = client.synchronize(OperationId(1), deadline);
    }

    let health = client.health();
    let health_deadline = Instant::now() + PROCESS_TIMEOUT;
    while !health.is_failed() && Instant::now() < health_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        health.is_failed(),
        "daemon surrogate did not observe guardian loss"
    );

    // This trace point stands for KeyboardController releasing EVIOCGRAB.
    // EmergencyCoordinator itself cannot start the worker before this call.
    append_trace(trace, "grab-released");
    let proof = client
        .emergency_coordinator()
        .start_pending_release()
        .unwrap()
        .wait(GUARDIAN_EMERGENCY_DEADLINE);
    append_trace(trace, &format!("daemon-emergency-terminal:{proof:?}"));
    panic!("intentional daemon fail-stop after guardian loss");
}

fn run_daemon_role(scenario: &str, trace: &Path) {
    if scenario == "guardian-sigkill" {
        run_daemon_guardian_sigkill_role(trace);
        return;
    }
    let connection = inherited_connection();
    let deadline = MutationDeadlineNs(monotonic_now_ns().unwrap() + 4_000_000_000);
    send_request(
        &connection,
        1,
        Request::Hello {
            daemon_nonce: [0x54; 16],
            deadline,
        },
    );
    assert!(matches!(
        receive_response(&connection),
        Response::Ready { .. }
    ));
    send_request(
        &connection,
        2,
        Request::PrepareKey {
            operation: OperationId(1),
            evdev_code: 30,
            deadline,
        },
    );
    let Response::Prepared { token, .. } = receive_response(&connection) else {
        panic!("fixture expected prepared response");
    };
    send_request(
        &connection,
        3,
        Request::ExecuteDown {
            operation: OperationId(1),
            token,
            deadline,
        },
    );

    if scenario == "lost-down-ack" {
        append_trace(trace, "down-sent");
        wait_forever();
    }

    assert!(matches!(
        receive_response(&connection),
        Response::DownAck { .. }
    ));
    append_trace(trace, "down-ack");

    if scenario == "panic-after-ack" {
        panic!("intentional daemon surrogate panic");
    }

    if scenario == "physical-debt" {
        send_request(
            &connection,
            4,
            Request::Synchronize {
                operation: OperationId(1),
                token_id: token.token_id,
                deadline: ReleaseDeadline::Mutation(deadline),
            },
        );
        assert!(matches!(
            receive_response(&connection),
            Response::SyncAck { .. }
        ));
        send_request(
            &connection,
            5,
            Request::TransferToPhysicalDebt {
                operation: OperationId(1),
                token,
                input_generation: InputGeneration(7),
                deadline,
            },
        );
        assert!(matches!(
            receive_response(&connection),
            Response::TransferAck { .. }
        ));
        append_trace(trace, "transfer-ack");
    }

    wait_forever();
}

#[test]
fn fixture_process_entry() {
    let Ok(role) = env::var(ROLE_ENV) else {
        return;
    };
    let scenario = env::var(SCENARIO_ENV).unwrap();
    let trace = PathBuf::from(env::var_os(TRACE_ENV).unwrap());
    match role.as_str() {
        "guardian" => run_guardian_role(&scenario, &trace),
        "daemon" => run_daemon_role(&scenario, &trace),
        other => panic!("unknown process fixture role: {other}"),
    }
}

fn clear_close_on_exec(fd: i32) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
        0
    );
}

fn spawn_role(
    role: &'static str,
    scenario: &str,
    trace: &Path,
    inherited_fd: i32,
    closed_fd: i32,
) -> std::io::Result<Child> {
    let mut command = Command::new(env::current_exe()?);
    command
        .arg("--exact")
        .arg(FIXTURE_TEST_NAME)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(ROLE_ENV, role)
        .env(FD_ENV, inherited_fd.to_string())
        .env(TRACE_ENV, trace)
        .env(SCENARIO_ENV, scenario)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if libc::close(closed_fd) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

struct ProcessFixture {
    _directory: tempfile::TempDir,
    trace: PathBuf,
    guardian: Option<Child>,
    daemon: Option<Child>,
}

impl ProcessFixture {
    fn spawn(scenario: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let trace = directory.path().join("trace.log");
        fs::write(&trace, []).unwrap();
        let (guardian_socket, daemon_socket) = Seqpacket::pair().unwrap();
        let guardian_fd = guardian_socket.as_raw_fd();
        let daemon_fd = daemon_socket.as_raw_fd();
        clear_close_on_exec(guardian_fd);
        clear_close_on_exec(daemon_fd);

        let mut guardian =
            spawn_role("guardian", scenario, &trace, guardian_fd, daemon_fd).unwrap();
        let daemon = match spawn_role("daemon", scenario, &trace, daemon_fd, guardian_fd) {
            Ok(daemon) => daemon,
            Err(error) => {
                let _ = guardian.kill();
                let _ = guardian.wait();
                panic!("failed to spawn daemon fixture: {error}");
            }
        };
        drop(guardian_socket);
        drop(daemon_socket);

        Self {
            _directory: directory,
            trace,
            guardian: Some(guardian),
            daemon: Some(daemon),
        }
    }

    fn trace_text(&self) -> String {
        fs::read_to_string(&self.trace).unwrap_or_default()
    }

    fn wait_for_trace(&self, needle: &str) {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while Instant::now() < deadline {
            if self.trace_text().lines().any(|line| line == needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "timed out waiting for trace {needle:?}; trace:\n{}",
            self.trace_text()
        );
    }

    fn wait_for_trace_prefix(&self, prefix: &str) {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        while Instant::now() < deadline {
            if self
                .trace_text()
                .lines()
                .any(|line| line.starts_with(prefix))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "timed out waiting for trace prefix {prefix:?}; trace:\n{}",
            self.trace_text()
        );
    }

    fn kill_daemon(&mut self) {
        let daemon = self.daemon.as_mut().unwrap();
        if daemon.try_wait().unwrap().is_none() {
            daemon.kill().unwrap();
        }
        let _ = daemon.wait().unwrap();
    }

    fn kill_guardian(&mut self) {
        let guardian = self.guardian.as_mut().unwrap();
        if guardian.try_wait().unwrap().is_none() {
            guardian.kill().unwrap();
        }
        let _ = guardian.wait().unwrap();
    }

    fn wait_daemon(&mut self) -> ExitStatus {
        self.daemon.as_mut().unwrap().wait().unwrap()
    }

    fn wait_guardian(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Some(status) = self.guardian.as_mut().unwrap().try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "guardian did not exit; trace:\n{}",
                self.trace_text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_daemon_bounded(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Some(status) = self.daemon.as_mut().unwrap().try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "daemon surrogate did not exit; trace:\n{}",
                self.trace_text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn trace_before(&self, first: &str, second: &str) -> bool {
        let text = self.trace_text();
        let first = text.lines().position(|line| line == first);
        let second = text.lines().position(|line| line == second);
        matches!((first, second), (Some(first), Some(second)) if first < second)
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        for child in [&mut self.daemon, &mut self.guardian] {
            if let Some(child) = child.as_mut() {
                if child.try_wait().ok().flatten().is_none() {
                    let _ = child.kill();
                }
                let _ = child.wait();
            }
        }
    }
}

#[test]
fn sigkill_daemon_surrogate_after_down_ack_makes_guardian_release_and_exit() {
    let mut fixture = ProcessFixture::spawn("down-ack");
    fixture.wait_for_trace("down-ack");
    fixture.kill_daemon();
    fixture.wait_for_trace("release:38");
    assert!(fixture.wait_guardian().success());
    assert!(fixture
        .trace_text()
        .contains("guardian-terminal:PeerEof:reconciled"));
}

#[test]
fn lost_down_ack_keeps_one_down_attempt_and_guardian_releases_it() {
    let mut fixture = ProcessFixture::spawn("lost-down-ack");
    fixture.wait_for_trace("down-sent");
    fixture.wait_for_trace("down:38");
    fixture.kill_daemon();
    fixture.wait_for_trace("release:38");
    assert!(fixture.wait_guardian().success());
    assert_eq!(
        fixture
            .trace_text()
            .lines()
            .filter(|line| *line == "down:38")
            .count(),
        1
    );
}

#[test]
fn daemon_surrogate_panic_closes_channel_and_guardian_exits() {
    let mut fixture = ProcessFixture::spawn("panic-after-ack");
    fixture.wait_for_trace("down-ack");
    assert!(!fixture.wait_daemon().success());
    fixture.wait_for_trace("release:38");
    assert!(fixture.wait_guardian().success());
}

#[test]
fn daemon_death_after_transfer_releases_session_scoped_modifier_debt() {
    let mut fixture = ProcessFixture::spawn("physical-debt");
    fixture.wait_for_trace("transfer-ack");
    fixture.kill_daemon();
    fixture.wait_for_trace("release:38");
    assert!(fixture.wait_guardian().success());
    assert!(fixture
        .trace_text()
        .contains("guardian-terminal:PeerEof:reconciled"));
}

#[test]
fn cleanup_failure_reports_unreconciled_and_does_not_claim_stopped() {
    let mut fixture = ProcessFixture::spawn("cleanup-failure");
    fixture.wait_for_trace("down-ack");
    fixture.kill_daemon();
    fixture.wait_for_trace("release-failed:38");
    fixture.wait_for_trace_prefix("guardian-terminal:PeerEof:unreconciled:1");
    assert!(fixture.wait_guardian().success());
    assert!(!fixture.trace_text().contains("Stopped"));
}

#[test]
fn sigkill_guardian_after_down_starts_emergency_only_after_ungrab_signal() {
    let mut fixture = ProcessFixture::spawn("guardian-sigkill");
    fixture.wait_for_trace("down:38");
    fixture.kill_guardian();
    fixture.wait_for_trace("grab-released");
    fixture.wait_for_trace("emergency-up:38");

    assert!(fixture.trace_before("grab-released", "emergency-up:38"));
    assert!(!fixture.wait_daemon_bounded().success());
    assert_eq!(
        fixture
            .trace_text()
            .lines()
            .filter(|line| *line == "down:38")
            .count(),
        1
    );
    assert!(fixture
        .trace_text()
        .contains("daemon-emergency-terminal:Reconciled"));
}
