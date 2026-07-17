use std::array;
use std::env;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread::{self, JoinHandle};

pub(crate) const DEBUG_LOG_QUEUE_CAPACITY: usize = 256;
pub(crate) const MAX_DEBUG_RECORD_BYTES: usize = 4096;
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

static GLOBAL_DEBUG_PRODUCER: OnceLock<DebugLogProducer> = OnceLock::new();

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

    pub(crate) fn try_enqueue(&self, kind: DebugLogKind, mut line: String) -> DebugEnqueueOutcome {
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

    fn record_drop(&self, kind: DebugLogKind) {
        self.dropped.values[kind as usize].fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn dropped(&self, kind: DebugLogKind) -> u64 {
        self.dropped.values[kind as usize].load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn any_enabled(&self) -> bool {
        self.enabled_mask != 0
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

struct DebugLogConfig {
    enabled_mask: u8,
    paths: [PathBuf; 4],
}

impl DebugLogConfig {
    #[cfg(test)]
    fn disabled() -> Self {
        Self {
            enabled_mask: 0,
            paths: array::from_fn(|index| PathBuf::from(DEFAULT_DEBUG_PATHS[index])),
        }
    }

    fn from_env() -> Self {
        Self::from_lookup(|name| env::var(name).ok())
    }

    fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        let enabled_mask = DEBUG_ENV
            .iter()
            .enumerate()
            .fold(0u8, |mask, (index, name)| {
                let enabled = lookup(name).is_some_and(|value| is_truthy(&value));
                mask | ((enabled as u8) << index)
            });
        let paths = array::from_fn(|index| {
            lookup(DEBUG_FILE_ENV[index])
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_DEBUG_PATHS[index]))
        });
        Self {
            enabled_mask,
            paths,
        }
    }

    #[cfg(test)]
    fn enabled(&self, kind: DebugLogKind) -> bool {
        self.enabled_mask & (1 << kind as u8) != 0
    }
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

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

pub(crate) fn format_input(stage: &str, details: &str) -> String {
    format!("[input-debug] stage={stage} {details}")
}

pub(crate) fn format_layout(stage: &str, details: &str) -> String {
    format!("[layout-debug] stage={stage} {details}")
}

pub(crate) fn format_capture(phase: &str, details: &str) -> String {
    format!("[daemon-capture] phase={phase} {details}")
}

pub(crate) fn format_selected(stage: &str, details: &str) -> String {
    format!("[selected-text-debug] stage={stage} {details}")
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

impl DebugLogRuntime {
    pub(crate) fn initialize_from_env() -> Self {
        let runtime = Self::from_config(DebugLogConfig::from_env());
        let _ = GLOBAL_DEBUG_PRODUCER.set(runtime.producer.clone());
        runtime
    }

    fn from_config(config: DebugLogConfig) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        if config.enabled_mask == 0 {
            return Self {
                producer: DebugLogProducer::disabled(),
                stop,
                worker: None,
            };
        }

        let (sender, receiver) = mpsc::sync_channel(DEBUG_LOG_QUEUE_CAPACITY);
        let producer = DebugLogProducer {
            enabled_mask: config.enabled_mask,
            sender: Some(sender),
            dropped: Arc::new(DebugDropCounters::default()),
        };
        let worker_stop = Arc::clone(&stop);
        let sink = ProductionDebugSink::new(config.paths);
        let worker = thread::Builder::new()
            .name("open-switcher-debug-log".to_string())
            .spawn(move || run_debug_worker(receiver, worker_stop, sink));

        match worker {
            Ok(worker) => Self {
                producer,
                stop,
                worker: Some(worker),
            },
            Err(_) => Self {
                producer: DebugLogProducer::disabled(),
                stop,
                worker: None,
            },
        }
    }
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

struct DebugFileSink {
    path: PathBuf,
    file: Option<File>,
    disabled: bool,
}

impl DebugFileSink {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            disabled: false,
        }
    }

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

impl ProductionDebugSink {
    fn new(paths: [PathBuf; 4]) -> Self {
        Self {
            files: paths.map(DebugFileSink::new),
        }
    }
}

impl DebugRecordSink for ProductionDebugSink {
    fn write_record(&mut self, record: &DebugLogRecord) {
        let _ = writeln!(io::stderr().lock(), "{}", record.line);
        self.files[record.kind as usize].write_line(&record.line);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::TryRecvError;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    struct RecordingSink {
        writes: Arc<Mutex<Vec<String>>>,
    }

    impl DebugRecordSink for RecordingSink {
        fn write_record(&mut self, record: &DebugLogRecord) {
            self.writes.lock().unwrap().push(record.line.to_string());
        }
    }

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
        let (producer, receiver) =
            DebugLogProducer::for_test(1, &[DebugLogKind::Capture, DebugLogKind::SelectedText]);
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

    #[test]
    fn worker_routes_all_categories_in_enqueue_order() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let sink = RecordingSink {
            writes: Arc::clone(&writes),
        };
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
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker must enter the blocking sink");
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
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker must enter the blocking sink");

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
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();

        let file = open_secure_debug_file(&path).unwrap();

        let mode = file.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn production_sink_opens_only_the_matching_category_file() {
        let temp = tempfile::tempdir().unwrap();
        let paths = array::from_fn(|index| temp.path().join(format!("{index}.log")));
        let mut sink = ProductionDebugSink::new(paths.clone());

        sink.write_record(&DebugLogRecord {
            kind: DebugLogKind::Input,
            line: "input-line".into(),
        });

        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "input-line\n");
        assert!(paths[1..].iter().all(|path| !path.exists()));
    }

    #[test]
    fn unsafe_file_failure_permanently_disables_only_that_sink() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.log");
        std::fs::write(&target, b"").unwrap();
        let unsafe_path = temp.path().join("unsafe.log");
        std::os::unix::fs::symlink(&target, &unsafe_path).unwrap();
        let paths = [
            unsafe_path.clone(),
            temp.path().join("layout.log"),
            temp.path().join("capture.log"),
            temp.path().join("selected.log"),
        ];
        let mut sink = ProductionDebugSink::new(paths);

        sink.write_record(&DebugLogRecord {
            kind: DebugLogKind::Input,
            line: "first".into(),
        });
        std::fs::remove_file(&unsafe_path).unwrap();
        sink.write_record(&DebugLogRecord {
            kind: DebugLogKind::Input,
            line: "second".into(),
        });

        assert!(!unsafe_path.exists());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "");
    }

    #[test]
    fn disabled_config_creates_no_worker_or_channel() {
        let runtime = DebugLogRuntime::from_config(DebugLogConfig::disabled());

        assert!(runtime.worker.is_none());
        assert!(runtime.producer.sender.is_none());
        assert!(!runtime.producer.any_enabled());
    }

    #[test]
    fn enabled_config_creates_one_lazy_worker_without_opening_files() {
        let temp = tempfile::tempdir().unwrap();
        let paths = array::from_fn(|index| temp.path().join(format!("{index}.log")));
        let runtime = DebugLogRuntime::from_config(DebugLogConfig {
            enabled_mask: 1 << DebugLogKind::Input as u8,
            paths: paths.clone(),
        });

        assert!(runtime.worker.is_some());
        assert!(runtime.producer.sender.is_some());
        assert!(runtime.producer.enabled(DebugLogKind::Input));
        assert!(paths.iter().all(|path| !path.exists()));
        drop(runtime);
    }

    #[test]
    fn startup_lookup_preserves_existing_flags_paths_and_defaults() {
        let config = DebugLogConfig::from_lookup(|name| match name {
            "OPEN_SWITCHER_INPUT_DEBUG" => Some("YES".to_string()),
            "OPEN_SWITCHER_LAYOUT_DEBUG" => Some("0".to_string()),
            "OPEN_SWITCHER_DAEMON_CAPTURE_DEBUG" => Some("on".to_string()),
            "OPEN_SWITCHER_SELECTED_TEXT_DEBUG" => Some("true".to_string()),
            "OPEN_SWITCHER_INPUT_DEBUG_FILE" => Some("/custom/input.log".to_string()),
            _ => None,
        });

        assert!(config.enabled(DebugLogKind::Input));
        assert!(!config.enabled(DebugLogKind::Layout));
        assert!(config.enabled(DebugLogKind::Capture));
        assert!(config.enabled(DebugLogKind::SelectedText));
        assert_eq!(config.paths[0], PathBuf::from("/custom/input.log"));
        assert_eq!(
            config.paths[1],
            PathBuf::from("/tmp/open-switcher-layout-debug.log")
        );
    }

    #[test]
    fn exact_existing_prefixes_are_preserved() {
        assert_eq!(
            format_input("writer", "ready=true"),
            "[input-debug] stage=writer ready=true"
        );
        assert_eq!(
            format_layout("sync", "result=ok"),
            "[layout-debug] stage=sync result=ok"
        );
        assert_eq!(
            format_selected("copy", "chars=3"),
            "[selected-text-debug] stage=copy chars=3"
        );
        assert_eq!(
            format_capture("start", "note=session-started"),
            "[daemon-capture] phase=start note=session-started"
        );
    }

    #[test]
    fn grab_reachable_layout_detection_has_no_direct_output() {
        let source = include_str!("../layout_switch/mod.rs");

        assert!(!source.contains("eprintln!"));
        assert!(!source.contains("stderr()"));
    }
}
