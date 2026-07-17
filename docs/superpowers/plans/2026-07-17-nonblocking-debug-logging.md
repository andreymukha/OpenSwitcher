# Nonblocking Debug Logging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure that enabled diagnostics, blocked log sinks, and postmortem output cannot delay keyboard forwarding or `EVIOCGRAB` release, while creating no logging worker in the default Debian-package configuration.

**Architecture:** A new `daemon::debug_log` module owns one optional bounded `sync_channel`, a nonblocking producer, one worker, secure per-category file sinks, and best-effort teardown. Existing debug helpers become thin producers. Synchronous error output on the input control path is either converted to best-effort diagnostics or reordered after backend shutdown.

**Tech Stack:** Rust 2021, `std::sync::mpsc::sync_channel`, atomics, `OnceLock`, `libc`, inline unit tests, existing Debian/VM verification flow.

---

## File map

- Create `src/daemon/debug_log.rs`: record model, bounded producer, worker, secure sinks, global ingress, and unit tests.
- Modify `src/daemon/mod.rs`: initialize and retain the logger runtime; reorder panic reporting after input shutdown.
- Modify `src/daemon/keyboard.rs`: delegate input diagnostics and remove synchronous release-error output.
- Modify `src/daemon/runtime.rs`: delegate layout diagnostics and remove the fallback `eprintln!` reachable from runtime synchronization.
- Modify `src/daemon/capture.rs`: delegate capture diagnostics without environment or file access.
- Modify `src/daemon/selected_text/debug.rs`: delegate redacted selected-text diagnostics.
- Modify `src/system/mod.rs`: route layout/session debug output through the hub.
- Test existing shutdown ordering in `src/daemon/mod.rs`, redaction in `src/daemon/selected_text/debug.rs`, and focused logger behavior in `src/daemon/debug_log.rs`.

### Task 1: Bounded nonblocking producer

**Files:**

- Create: `src/daemon/debug_log.rs`
- Modify: `src/daemon/mod.rs`

- [ ] **Step 1: Add producer RED tests**

Create the module and tests for full, closed, disabled, and UTF-8-bounded records. The test-only constructor returns the receiver so no worker or environment is involved:

```rust
#[test]
fn full_queue_drops_newest_without_replacing_first_record() {
    let (producer, receiver) = DebugLogProducer::for_test(1, &[DebugLogKind::Input]);

    assert_eq!(
        producer.try_enqueue(DebugLogKind::Input, "first".to_string()),
        DebugEnqueueOutcome::Queued
    );
    assert_eq!(
        producer.try_enqueue(DebugLogKind::Input, "second".to_string()),
        DebugEnqueueOutcome::DroppedFull
    );
    let DebugLogCommand::Record(record) = receiver.try_recv().unwrap() else {
        panic!("expected queued record");
    };
    assert_eq!(record.line.as_ref(), "first");
    assert_eq!(producer.dropped(DebugLogKind::Input), 1);
}

#[test]
fn disconnected_queue_drops_without_panicking() {
    let (producer, receiver) = DebugLogProducer::for_test(1, &[DebugLogKind::Layout]);
    drop(receiver);

    assert_eq!(
        producer.try_enqueue(DebugLogKind::Layout, "line".to_string()),
        DebugEnqueueOutcome::DroppedClosed
    );
    assert_eq!(producer.dropped(DebugLogKind::Layout), 1);
}

#[test]
fn disabled_category_does_not_build_or_enqueue_a_record() {
    let (producer, receiver) = DebugLogProducer::for_test(1, &[]);
    let built = AtomicBool::new(false);

    assert_eq!(
        producer.try_enqueue_with(DebugLogKind::Input, || {
            built.store(true, Ordering::SeqCst);
            "must-not-be-built".to_string()
        }),
        DebugEnqueueOutcome::Disabled
    );
    assert!(!built.load(Ordering::SeqCst));
    assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
}

#[test]
fn oversized_record_is_truncated_on_utf8_boundary() {
    let (producer, receiver) = DebugLogProducer::for_test(1, &[DebugLogKind::Input]);
    producer.try_enqueue(DebugLogKind::Input, "я".repeat(MAX_DEBUG_RECORD_BYTES));

    let DebugLogCommand::Record(record) = receiver.try_recv().unwrap() else {
        panic!("expected queued record");
    };
    assert!(record.line.len() <= MAX_DEBUG_RECORD_BYTES);
    assert!(record.line.is_char_boundary(record.line.len()));
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib debug_log -- --nocapture
```

Expected: compile failure because the producer types and constructors do not exist.

- [ ] **Step 3: Implement the minimal producer**

Use a standard bounded channel and no blocking send API:

```rust
use std::array;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

pub(crate) const DEBUG_LOG_QUEUE_CAPACITY: usize = 256;
pub(crate) const MAX_DEBUG_RECORD_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugLogKind {
    Input = 0,
    Layout = 1,
    Capture = 2,
    SelectedText = 3,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DebugLogRecord {
    pub kind: DebugLogKind,
    pub line: Box<str>,
}

enum DebugLogCommand {
    Record(DebugLogRecord),
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugEnqueueOutcome {
    Disabled,
    Queued,
    DroppedFull,
    DroppedClosed,
}

struct DebugDropCounters {
    values: [AtomicU64; 4],
}

impl Default for DebugDropCounters {
    fn default() -> Self {
        Self {
            values: array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

#[derive(Clone)]
pub(crate) struct DebugLogProducer {
    enabled_mask: u8,
    sender: Option<mpsc::SyncSender<DebugLogCommand>>,
    dropped: Arc<DebugDropCounters>,
}

impl DebugLogProducer {
    pub(crate) fn enabled(&self, kind: DebugLogKind) -> bool {
        self.enabled_mask & (1 << kind as u8) != 0
    }

    pub(crate) fn try_enqueue_with(
        &self,
        kind: DebugLogKind,
        build: impl FnOnce() -> String,
    ) -> DebugEnqueueOutcome {
        if !self.enabled(kind) {
            return DebugEnqueueOutcome::Disabled;
        }
        self.try_enqueue(kind, build())
    }

    pub(crate) fn try_enqueue(
        &self,
        kind: DebugLogKind,
        mut line: String,
    ) -> DebugEnqueueOutcome {
        truncate_utf8(&mut line, MAX_DEBUG_RECORD_BYTES);
        let record = DebugLogRecord {
            kind,
            line: line.into_boxed_str(),
        };
        let Some(sender) = self.sender.as_ref() else {
            self.record_drop(kind);
            return DebugEnqueueOutcome::DroppedClosed;
        };
        match sender.try_send(DebugLogCommand::Record(record)) {
            Ok(()) => DebugEnqueueOutcome::Queued,
            Err(mpsc::TrySendError::Full(_)) => {
                self.record_drop(kind);
                DebugEnqueueOutcome::DroppedFull
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.record_drop(kind);
                DebugEnqueueOutcome::DroppedClosed
            }
        }
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}
```

Complete the producer with these methods:

```rust
impl DebugLogProducer {
    fn record_drop(&self, kind: DebugLogKind) {
        self.dropped.values[kind as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn dropped(&self, kind: DebugLogKind) -> u64 {
        self.dropped.values[kind as usize].load(Ordering::Relaxed)
    }

    fn disabled() -> Self {
        Self {
            enabled_mask: 0,
            sender: None,
            dropped: Arc::new(DebugDropCounters::default()),
        }
    }

    #[cfg(test)]
    fn for_test(
        capacity: usize,
        enabled: &[DebugLogKind],
    ) -> (Self, mpsc::Receiver<DebugLogCommand>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let enabled_mask = enabled
            .iter()
            .fold(0u8, |mask, kind| mask | (1 << *kind as u8));
        (
            Self {
                enabled_mask,
                sender: Some(sender),
                dropped: Arc::new(DebugDropCounters::default()),
            },
            receiver,
        )
    }
}
```

- [ ] **Step 4: Run GREEN**

Run:

```bash
cargo test --lib debug_log -- --nocapture
```

Expected: the four producer tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/debug_log.rs src/daemon/mod.rs
git commit -m "feat: add bounded debug log producer"
```

### Task 2: Worker lifecycle and secure sinks

**Files:**

- Modify: `src/daemon/debug_log.rs`

- [ ] **Step 1: Add worker, routing, teardown, and file-safety RED tests**

Add local fake sinks and temporary-file tests:

```rust
#[test]
fn worker_routes_all_categories_in_enqueue_order() {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let sink = RecordingSink::new(Arc::clone(&writes));
    let (sender, receiver) = mpsc::sync_channel(8);
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker = thread::spawn(move || run_debug_worker(receiver, worker_stop, sink));

    for (kind, line) in [
        (DebugLogKind::Input, "input"),
        (DebugLogKind::Layout, "layout"),
        (DebugLogKind::Capture, "capture"),
        (DebugLogKind::SelectedText, "selected"),
    ] {
        sender
            .send(DebugLogCommand::Record(DebugLogRecord {
                kind,
                line: line.into(),
            }))
            .unwrap();
    }
    sender.send(DebugLogCommand::Shutdown).unwrap();
    worker.join().unwrap();

    assert_eq!(
        *writes.lock().unwrap(),
        vec!["input", "layout", "capture", "selected"]
    );
}

#[test]
fn runtime_drop_never_joins_an_unfinished_worker() {
    let (producer, receiver) = DebugLogProducer::for_test(1, &[DebugLogKind::Input]);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        run_debug_worker(
            receiver,
            worker_stop,
            BlockingSink {
                entered: entered_tx,
                release: release_rx,
            },
        )
    });
    producer.try_enqueue(DebugLogKind::Input, "blocked".to_string());
    entered_rx.recv().unwrap();

    let runtime = DebugLogRuntime {
        producer,
        stop,
        worker: Some(worker),
    };
    let (dropped_tx, dropped_rx) = mpsc::channel();
    thread::spawn(move || {
        drop(runtime);
        dropped_tx.send(()).unwrap();
    });

    dropped_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("runtime drop must not wait for a blocked sink");
    release_tx.send(()).unwrap();
}

#[test]
fn secure_sink_rejects_symlink_and_fifo() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target.log");
    std::fs::write(&target, b"").unwrap();
    let link = temp.path().join("link.log");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(open_secure_debug_file(&link).is_err());

    let fifo = temp.path().join("fifo");
    let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    assert!(open_secure_debug_file(&fifo).is_err());
}

#[test]
fn secure_sink_forces_owner_only_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("debug.log");
    let file = open_secure_debug_file(&path).unwrap();
    let mode = file.metadata().unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
```

Add the blocked-sink test with an explicit release channel:

```rust
struct BlockingSink {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl DebugRecordSink for BlockingSink {
    fn write_record(&mut self, _record: &DebugLogRecord) {
        self.entered.send(()).unwrap();
        self.release.recv().unwrap();
    }
}

#[test]
fn blocked_sink_cannot_block_or_expand_producer_queue() {
    let (producer, receiver) = DebugLogProducer::for_test(1, &[DebugLogKind::Input]);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        run_debug_worker(
            receiver,
            worker_stop,
            BlockingSink {
                entered: entered_tx,
                release: release_rx,
            },
        )
    });

    assert_eq!(
        producer.try_enqueue(DebugLogKind::Input, "active".to_string()),
        DebugEnqueueOutcome::Queued
    );
    entered_rx.recv().unwrap();
    assert_eq!(
        producer.try_enqueue(DebugLogKind::Input, "queued".to_string()),
        DebugEnqueueOutcome::Queued
    );
    assert_eq!(
        producer.try_enqueue(DebugLogKind::Input, "dropped".to_string()),
        DebugEnqueueOutcome::DroppedFull
    );

    stop.store(true, Ordering::SeqCst);
    release_tx.send(()).unwrap();
    worker.join().unwrap();
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib debug_log -- --nocapture
```

Expected: compile failure for missing worker, lifecycle, sink, and secure-open functions.

- [ ] **Step 3: Implement worker and runtime**

Use an injected sink for deterministic tests and a non-waiting drop policy:

```rust
trait DebugRecordSink: Send + 'static {
    fn write_record(&mut self, record: &DebugLogRecord);
}

fn run_debug_worker<S: DebugRecordSink>(
    receiver: mpsc::Receiver<DebugLogCommand>,
    stop: Arc<AtomicBool>,
    mut sink: S,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            DebugLogCommand::Record(record) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                sink.write_record(&record);
                if stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            DebugLogCommand::Shutdown => break,
        }
    }
}

pub(crate) struct DebugLogRuntime {
    producer: DebugLogProducer,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for DebugLogRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(sender) = self.producer.sender.as_ref() {
            let _ = sender.try_send(DebugLogCommand::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}
```

The production sink keeps at most one lazily opened `File` per category and writes the record to the category file and `stderr`. A sink failure disables only that file handle; it never sends an error back to the producer:

```rust
struct DebugFileSink {
    path: PathBuf,
    file: Option<File>,
    disabled: bool,
}

impl DebugFileSink {
    fn write_line(&mut self, line: &str) {
        if self.disabled {
            return;
        }
        if self.file.is_none() {
            match open_secure_debug_file(&self.path) {
                Ok(file) => self.file = Some(file),
                Err(_) => {
                    self.disabled = true;
                    return;
                }
            }
        }
        if self
            .file
            .as_mut()
            .is_some_and(|file| writeln!(file, "{line}").is_err())
        {
            self.file = None;
            self.disabled = true;
        }
    }
}

struct ProductionDebugSink {
    files: [DebugFileSink; 4],
}

impl DebugRecordSink for ProductionDebugSink {
    fn write_record(&mut self, record: &DebugLogRecord) {
        let _ = writeln!(io::stderr().lock(), "{}", record.line);
        self.files[record.kind as usize].write_line(&record.line);
    }
}
```

Implement secure open with Unix flags and post-open validation:

```rust
fn open_secure_debug_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::other("debug log target is not a regular file"));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "debug log target is not owned by the effective user",
        ));
    }
    file.set_permissions(Permissions::from_mode(0o600))?;
    Ok(file)
}
```

- [ ] **Step 4: Run GREEN**

Run:

```bash
cargo test --lib debug_log -- --nocapture
```

Expected: producer, worker, blocked-sink, teardown, routing, and secure-file tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/debug_log.rs
git commit -m "feat: isolate debug sinks in a best-effort worker"
```

### Task 3: Lazy global initialization and existing logger migration

**Files:**

- Modify: `src/daemon/debug_log.rs`
- Modify: `src/daemon/mod.rs`
- Modify: `src/daemon/keyboard.rs`
- Modify: `src/daemon/runtime.rs`
- Modify: `src/daemon/capture.rs`
- Modify: `src/daemon/selected_text/debug.rs`
- Modify: `src/system/mod.rs`

- [ ] **Step 1: Add configuration and formatting RED tests**

Test configuration without mutating process-global environment:

```rust
#[test]
fn disabled_config_creates_no_worker_or_channel() {
    let config = DebugLogConfig::disabled();
    let runtime = DebugLogRuntime::from_config(config);
    assert!(runtime.worker.is_none());
    assert!(!runtime.producer.any_enabled());
}

#[test]
fn exact_existing_prefixes_are_preserved() {
    assert_eq!(format_input("writer", "ready=true"),
               "[input-debug] stage=writer ready=true");
    assert_eq!(format_layout("sync", "result=ok"),
               "[layout-debug] stage=sync result=ok");
    assert_eq!(format_selected("copy", "chars=3"),
               "[selected-text-debug] stage=copy chars=3");
    assert_eq!(format_capture("start", "note=session-started"),
               "[daemon-capture] phase=start note=session-started");
}
```

Keep the existing `summarize_text_redacts_content` test and add an assertion that the selected-text formatter receives only its summary.

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib debug_log -- --nocapture
cargo test --lib selected_text::debug -- --nocapture
```

Expected: missing configuration, runtime construction, and formatter helpers fail compilation.

- [ ] **Step 3: Implement startup configuration and global ingress**

Read the four existing environment flags and paths once:

```rust
static GLOBAL_DEBUG_PRODUCER: OnceLock<DebugLogProducer> = OnceLock::new();

pub(crate) fn debug_enabled(kind: DebugLogKind) -> bool {
    GLOBAL_DEBUG_PRODUCER
        .get()
        .is_some_and(|producer| producer.enabled(kind))
}

pub(crate) fn try_debug_line(
    kind: DebugLogKind,
    build: impl FnOnce() -> String,
) -> DebugEnqueueOutcome {
    GLOBAL_DEBUG_PRODUCER
        .get()
        .map(|producer| producer.try_enqueue_with(kind, build))
        .unwrap_or(DebugEnqueueOutcome::Disabled)
}
```

Use this immutable configuration, preserving all existing variables and defaults:

```rust
const DEBUG_ENV: [&str; 4] = [
    "OPEN_SWITCHER_INPUT_DEBUG",
    "OPEN_SWITCHER_LAYOUT_DEBUG",
    "OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG",
    "OPEN_SWITCHER_SELECTED_TEXT_DEBUG",
];
const DEBUG_FILE_ENV: [&str; 4] = [
    "OPEN_SWITCHER_INPUT_DEBUG_FILE",
    "OPEN_SWITCHER_LAYOUT_DEBUG_FILE",
    "OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG_FILE",
    "OPEN_SWITCHER_SELECTED_TEXT_DEBUG_FILE",
];
const DEFAULT_DEBUG_PATHS: [&str; 4] = [
    "/tmp/open-switcher-input-debug.log",
    "/tmp/open-switcher-layout-debug.log",
    "/tmp/open-switcher-daemon-capture.log",
    "/tmp/open-switcher-selected-text.log",
];

struct DebugLogConfig {
    enabled_mask: u8,
    paths: [PathBuf; 4],
}

impl DebugLogConfig {
    fn disabled() -> Self {
        Self {
            enabled_mask: 0,
            paths: array::from_fn(|index| PathBuf::from(DEFAULT_DEBUG_PATHS[index])),
        }
    }

    fn from_env() -> Self {
        let enabled_mask = DEBUG_ENV.iter().enumerate().fold(0, |mask, (index, name)| {
            let enabled = env::var(name)
                .ok()
                .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"));
            mask | ((enabled as u8) << index)
        });
        let paths = array::from_fn(|index| {
            env::var(DEBUG_FILE_ENV[index])
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(DEFAULT_DEBUG_PATHS[index]))
        });
        Self { enabled_mask, paths }
    }
}
```

`DebugLogRuntime::from_config` returns a runtime with `DebugLogProducer::disabled()` and no channel when `enabled_mask == 0`. Otherwise it creates exactly one capacity-256 channel, starts one `open-switcher-debug-log` thread with `ProductionDebugSink`, and returns a producer sharing that sender. Thread-start failure is converted to a disconnected/disabled best-effort producer and never fails daemon startup. `DebugLogRuntime::initialize_from_env` calls it, installs a clone in `GLOBAL_DEBUG_PRODUCER`, and returns the owning runtime.

Initialize it at the top of `daemon::run` and keep the guard in scope through finalization:

```rust
pub fn run() -> Result<(), SwitcherError> {
    let _debug_log_runtime = debug_log::DebugLogRuntime::initialize_from_env();
    let config_service = ConfigService::load(default_config_path())?;
    // Existing startup and daemon lifecycle follows unchanged.
}
```

- [ ] **Step 4: Migrate all four debug helpers and session detection**

Each wrapper performs no environment, file, `stderr`, or blocking channel operation:

```rust
pub(crate) fn log_input_debug(stage: &str, details: &str) {
    try_debug_line(DebugLogKind::Input, || {
        format!("[input-debug] stage={stage} {details}")
    });
}

pub(crate) fn log_layout_debug(stage: &str, details: &str) {
    try_debug_line(DebugLogKind::Layout, || {
        format!("[layout-debug] stage={stage} {details}")
    });
}

pub(crate) fn log_selected_text_debug(stage: &str, details: &str) {
    try_debug_line(DebugLogKind::SelectedText, || {
        format!("[selected-text-debug] stage={stage} {details}")
    });
}
```

Capture first checks `debug_enabled(DebugLogKind::Capture)` and then formats its existing pressed-key/evaluation fields inside the enqueue closure. `system::log_session_detection_summary` uses `DebugLogKind::Layout` and preserves `[session-detect]`.

Remove the old debug environment constants, `OpenOptions`, `append_*_debug_line`, repeated environment reads, and debug `eprintln!` implementations from the migrated files.

- [ ] **Step 5: Run GREEN and static boundary checks**

Run:

```bash
cargo test --lib debug_log -- --nocapture
cargo test --lib selected_text::debug -- --nocapture
cargo test --lib capture -- --nocapture
cargo test --lib system::tests -- --nocapture
rg -n "OPEN_SWITCHER_.*DEBUG|OpenOptions|writeln!|eprintln!" src/daemon/keyboard.rs src/daemon/runtime.rs src/daemon/capture.rs src/daemon/selected_text/debug.rs src/system/mod.rs
```

Expected: tests pass. Remaining `OpenOptions` in `keyboard.rs` is only `/dev/uinput` access, and every remaining `eprintln!` is classified as pre-grab, post-release, or isolated worker output.

- [ ] **Step 6: Commit**

```bash
git add src/daemon/debug_log.rs src/daemon/mod.rs src/daemon/keyboard.rs src/daemon/runtime.rs src/daemon/capture.rs src/daemon/selected_text/debug.rs src/system/mod.rs
git commit -m "fix: make debug logging nonblocking on input paths"
```

### Task 4: Reorder synchronous postmortem output after input release

**Files:**

- Modify: `src/daemon/mod.rs`
- Modify: `src/daemon/keyboard.rs`
- Modify: `src/daemon/runtime.rs`

- [ ] **Step 1: Add shutdown-order RED test**

Factor postmortem reporting behind a callback and prove it follows backend shutdown:

```rust
#[test]
fn input_loop_postmortem_is_reported_only_after_backend_shutdown() {
    let phases = RefCell::new(Vec::new());

    let result = finalize_daemon_run_with_postmortem(
        Err(SwitcherError::DaemonPanicked),
        || phases.borrow_mut().push("release-input"),
        || {
            phases.borrow_mut().push("stop-monitor");
            Ok(())
        },
        || phases.borrow_mut().push("report-panic"),
    );

    assert!(matches!(result, Err(SwitcherError::DaemonPanicked)));
    assert_eq!(
        *phases.borrow(),
        vec!["release-input", "stop-monitor", "report-panic"]
    );
}
```

- [ ] **Step 2: Run RED**

Run:

```bash
cargo test --lib input_loop_postmortem_is_reported_only_after_backend_shutdown -- --nocapture
```

Expected: compile failure because the postmortem-aware finalizer does not exist.

- [ ] **Step 3: Implement release-before-report ordering**

Use the existing finalizer and run the report only after it has invoked shutdown and monitor stop:

```rust
fn finalize_daemon_run_with_postmortem<Shutdown, StopMonitor, Postmortem>(
    result: Result<(), SwitcherError>,
    shutdown: Shutdown,
    stop_monitor: StopMonitor,
    postmortem: Postmortem,
) -> Result<(), SwitcherError>
where
    Shutdown: FnOnce(),
    StopMonitor: FnOnce() -> std::thread::Result<()>,
    Postmortem: FnOnce(),
{
    let result = finalize_daemon_run(result, shutdown, stop_monitor);
    postmortem();
    result
}
```

The panic branch stores an owned reason and defers its ordinary `eprintln!` through this callback. Remove synchronous `eprintln!` from `release_grab_best_effort` and `GrabbedKeyboardDevice::drop`; their nonblocking input-debug record remains. Replace the layout auto-detection fallback `eprintln!` with `log_layout_debug` because that function is reachable from runtime synchronization until the subsequent H01 snapshot phase lands.

Keep direct output in writer and selected-text worker threads only where the thread has already published itself dead or cannot block input forwarding. Keep the capture-monitor shutdown message because `finalize_daemon_run` has already released input before monitor stop.

- [ ] **Step 4: Run GREEN and audit remaining direct output**

Run:

```bash
cargo test --lib input_loop_postmortem_is_reported_only_after_backend_shutdown -- --nocapture
cargo test --lib daemon::tests -- --nocapture
cargo test --lib daemon::keyboard::tests -- --nocapture
cargo test --lib daemon::runtime::tests -- --nocapture
rg -n "eprintln!" src/daemon src/system/mod.rs
```

Expected: tests pass. No remaining synchronous output executes on the grabbed input/control thread before release.

- [ ] **Step 5: Commit**

```bash
git add src/daemon/mod.rs src/daemon/keyboard.rs src/daemon/runtime.rs
git commit -m "fix: release input before synchronous postmortem output"
```

### Task 5: Review and verification checkpoint

**Files:**

- Review all files changed in Tasks 1-4.

- [ ] **Step 1: Run formatting and compile verification**

Run:

```bash
rustup run stable rustfmt --edition 2021 --check src/daemon/debug_log.rs src/daemon/mod.rs src/daemon/keyboard.rs src/daemon/runtime.rs src/daemon/capture.rs src/daemon/selected_text/debug.rs src/system/mod.rs
cargo check --all-targets
git diff --check HEAD~4..HEAD
```

Expected: all commands exit zero.

- [ ] **Step 2: Run focused safe tests**

Run:

```bash
cargo test --lib debug_log -- --nocapture
cargo test --lib daemon::tests -- --nocapture
cargo test --lib daemon::keyboard::tests -- --nocapture
cargo test --lib daemon::runtime::tests -- --nocapture
cargo test --lib daemon::capture::tests -- --nocapture
cargo test --lib daemon::selected_text -- --nocapture
cargo test --lib system::tests -- --nocapture
```

Expected: all environment-independent tests pass. Any sandbox-only D-Bus or Wayland-socket `EPERM` failures are recorded separately and rerun in the VM rather than treated as logger regressions.

- [ ] **Step 3: Perform two-stage review**

First review the implementation against every success criterion in `docs/superpowers/specs/2026-07-17-nonblocking-debug-logging-design.md`. Then perform a separate code-quality review focused on queue semantics, shutdown races, file security, and accidental input-path I/O.

Expected: no Critical, High, or Medium blocker remains before moving to the runtime snapshot portion of H01.

- [ ] **Step 4: Record the checkpoint**

Run:

```bash
git status --short
git log --oneline -6
```

Expected: the worktree is clean and the logger implementation is represented by the planned focused commits.
