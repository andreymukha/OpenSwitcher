mod logind;

use crate::error::SwitcherError;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SESSION_LEASE_TTL_MS: u64 = 3_000;
const SESSION_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const SESSION_WAIT_SLICE: Duration = Duration::from_millis(50);
const SESSION_RECONNECT_INITIAL: Duration = Duration::from_millis(100);
const SESSION_RECONNECT_MAX: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionRecord {
    pub(crate) id: String,
    pub(crate) uid: u32,
    pub(crate) seat: String,
    pub(crate) session_type: String,
    pub(crate) class: String,
    pub(crate) active: bool,
    pub(crate) remote: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AuthorizedSession {
    session_id: Arc<str>,
    seat: Arc<str>,
}

impl AuthorizedSession {
    pub(crate) fn new(session_id: &str, seat: &str) -> Self {
        Self {
            session_id: Arc::from(session_id),
            seat: Arc::from(seat),
        }
    }

    #[cfg(test)]
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn seat(&self) -> &str {
        &self.seat
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionUnavailableReason {
    NoCandidate,
    Ambiguous,
    SourceUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SessionDecision {
    Authorized(AuthorizedSession),
    Unauthorized(SessionUnavailableReason),
}

fn decide_session(uid: u32, records: &[SessionRecord]) -> SessionDecision {
    let mut candidates = records.iter().filter(|record| {
        record.uid == uid
            && record.active
            && !record.remote
            && record.class == "user"
            && matches!(record.session_type.as_str(), "x11" | "wayland")
            && !record.seat.is_empty()
    });

    let Some(first) = candidates.next() else {
        return SessionDecision::Unauthorized(SessionUnavailableReason::NoCandidate);
    };

    if candidates.next().is_some() {
        return SessionDecision::Unauthorized(SessionUnavailableReason::Ambiguous);
    }

    SessionDecision::Authorized(AuthorizedSession::new(&first.id, &first.seat))
}

struct SessionAccessInner {
    authorized: AtomicBool,
    generation: AtomicU64,
    confirmed_at_ms: AtomicU64,
    worker_alive: AtomicBool,
    lease_ttl_ms: u64,
    session: RwLock<Option<AuthorizedSession>>,
}

#[derive(Clone)]
pub(crate) struct SessionAccessPublication {
    inner: Arc<SessionAccessInner>,
}

impl SessionAccessPublication {
    pub(crate) fn new() -> Self {
        Self::with_lease_ttl(SESSION_LEASE_TTL_MS)
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(lease_ttl_ms: u64) -> Self {
        Self::with_lease_ttl(lease_ttl_ms)
    }

    fn with_lease_ttl(lease_ttl_ms: u64) -> Self {
        Self {
            inner: Arc::new(SessionAccessInner {
                authorized: AtomicBool::new(false),
                generation: AtomicU64::new(1),
                confirmed_at_ms: AtomicU64::new(0),
                worker_alive: AtomicBool::new(true),
                lease_ttl_ms,
                session: RwLock::new(None),
            }),
        }
    }

    pub(crate) fn publish(&self, decision: SessionDecision, confirmed_at_ms: u64) {
        let mut current = self
            .inner
            .session
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if !self.inner.worker_alive.load(Ordering::Acquire) {
            return;
        }

        if let SessionDecision::Authorized(session) = &decision {
            if self.inner.authorized.load(Ordering::Acquire) && current.as_ref() == Some(session) {
                self.inner
                    .confirmed_at_ms
                    .store(confirmed_at_ms, Ordering::Release);
                return;
            }
        }

        self.inner.authorized.store(false, Ordering::Release);
        self.inner.generation.fetch_add(1, Ordering::AcqRel);

        match decision {
            SessionDecision::Authorized(session) => {
                *current = Some(session);
                self.inner
                    .confirmed_at_ms
                    .store(confirmed_at_ms, Ordering::Release);
                self.inner.authorized.store(true, Ordering::Release);
            }
            SessionDecision::Unauthorized(_) => {
                *current = None;
                self.inner
                    .confirmed_at_ms
                    .store(confirmed_at_ms, Ordering::Release);
            }
        }
    }

    pub(crate) fn backend_lease(&self, now_ms: u64) -> Result<SessionLease, SwitcherError> {
        self.ensure_available(now_ms)?;

        let generation = self.inner.generation.load(Ordering::Acquire);
        let session = self
            .inner
            .session
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or(SwitcherError::InputSessionInactive)?;

        self.ensure_available(now_ms)?;
        if self.inner.generation.load(Ordering::Acquire) != generation {
            return Err(SwitcherError::InputSessionInactive);
        }

        Ok(SessionLease {
            publication: self.clone(),
            generation,
            session,
        })
    }

    pub(crate) fn health_error(&self, now_ms: u64) -> Option<SwitcherError> {
        self.ensure_available(now_ms).err()
    }

    pub(crate) fn mark_worker_stopped(&self) {
        let mut current = self
            .inner
            .session
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        self.inner.authorized.store(false, Ordering::Release);
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        *current = None;
        self.inner.worker_alive.store(false, Ordering::Release);
    }

    fn ensure_available(&self, now_ms: u64) -> Result<(), SwitcherError> {
        if !self.inner.worker_alive.load(Ordering::Acquire) {
            return Err(SwitcherError::SessionMonitorStopped);
        }
        if !self.inner.authorized.load(Ordering::Acquire) || !self.is_fresh(now_ms) {
            return Err(SwitcherError::InputSessionInactive);
        }
        Ok(())
    }

    fn is_fresh(&self, now_ms: u64) -> bool {
        let confirmed_at_ms = self.inner.confirmed_at_ms.load(Ordering::Acquire);
        now_ms.saturating_sub(confirmed_at_ms) <= self.inner.lease_ttl_ms
    }
}

#[derive(Clone)]
pub(crate) struct SessionLease {
    publication: SessionAccessPublication,
    generation: u64,
    session: AuthorizedSession,
}

impl SessionLease {
    #[cfg(test)]
    pub(crate) fn always_authorized_for_test() -> Self {
        let publication = SessionAccessPublication::new_for_test(u64::MAX);
        publication.publish(
            SessionDecision::Authorized(AuthorizedSession::new(
                "openswitcher-test-session",
                "seat0",
            )),
            monotonic_ms(),
        );
        publication
            .backend_lease(monotonic_ms())
            .expect("test session lease must be authorized")
    }

    pub(crate) fn ensure_current(&self, now_ms: u64) -> Result<(), SwitcherError> {
        self.publication.ensure_available(now_ms)?;
        if self.publication.inner.generation.load(Ordering::Acquire) != self.generation {
            return Err(SwitcherError::InputSessionInactive);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    #[cfg(test)]
    pub(crate) fn session_id(&self) -> &str {
        self.session.session_id()
    }

    pub(crate) fn seat(&self) -> &str {
        self.session.seat()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionSourceEvent {
    Changed,
    Timeout,
}

trait SessionSource: Send {
    fn subscribe(&mut self) -> Result<(), SwitcherError>;
    fn snapshot(&mut self, uid: u32) -> Result<Vec<SessionRecord>, SwitcherError>;
    fn wait_for_change(&mut self, timeout: Duration) -> Result<SessionSourceEvent, SwitcherError>;
}

#[derive(Clone, Copy)]
struct MonitorTiming {
    refresh_interval: Duration,
    wait_slice: Duration,
    reconnect_initial: Duration,
    reconnect_max: Duration,
}

impl Default for MonitorTiming {
    fn default() -> Self {
        Self {
            refresh_interval: SESSION_REFRESH_INTERVAL,
            wait_slice: SESSION_WAIT_SLICE,
            reconnect_initial: SESSION_RECONNECT_INITIAL,
            reconnect_max: SESSION_RECONNECT_MAX,
        }
    }
}

pub(crate) struct SessionActivityMonitor {
    publication: SessionAccessPublication,
    stop_tx: async_channel::Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl SessionActivityMonitor {
    pub(crate) fn start() -> Result<Self, SwitcherError> {
        let source = Box::new(logind::LogindSessionSource::new());
        let uid = nix::unistd::Uid::effective().as_raw();
        Self::spawn(source, uid, MonitorTiming::default())
    }

    #[cfg(test)]
    fn start_with_source_for_test(
        source: Box<dyn SessionSource>,
        uid: u32,
        timing: MonitorTiming,
    ) -> Result<Self, SwitcherError> {
        Self::spawn(source, uid, timing)
    }

    fn spawn(
        source: Box<dyn SessionSource>,
        uid: u32,
        timing: MonitorTiming,
    ) -> Result<Self, SwitcherError> {
        let publication = SessionAccessPublication::new();
        let worker_publication = publication.clone();
        let (stop_tx, stop_rx) = async_channel::bounded(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("openswitcher-session-activity".to_owned())
            .spawn(move || {
                run_session_monitor(source, uid, timing, worker_publication, stop_rx, ready_tx)
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                publication,
                stop_tx,
                join: Some(join),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(error) => {
                let _ = join.join();
                Err(std::io::Error::other(format!(
                    "session activity monitor stopped before startup completed: {error}"
                ))
                .into())
            }
        }
    }

    pub(crate) fn publication(&self) -> SessionAccessPublication {
        self.publication.clone()
    }

    pub(crate) fn stop(&mut self) -> thread::Result<()> {
        let _ = self.stop_tx.try_send(());
        match self.join.take() {
            Some(join) => join.join(),
            None => Ok(()),
        }
    }

    pub(crate) fn detach(&mut self) {
        let _ = self.stop_tx.try_send(());
        drop(self.join.take());
    }
}

impl Drop for SessionActivityMonitor {
    fn drop(&mut self) {
        if self.stop().is_err() {
            eprintln!("[input] Session activity monitor worker panicked");
        }
    }
}

struct WorkerDeathGuard {
    publication: SessionAccessPublication,
}

impl WorkerDeathGuard {
    fn new(publication: SessionAccessPublication) -> Self {
        Self { publication }
    }
}

impl Drop for WorkerDeathGuard {
    fn drop(&mut self) {
        self.publication.mark_worker_stopped();
    }
}

fn run_session_monitor(
    mut source: Box<dyn SessionSource>,
    uid: u32,
    timing: MonitorTiming,
    publication: SessionAccessPublication,
    stop_rx: async_channel::Receiver<()>,
    ready_tx: mpsc::SyncSender<Result<(), SwitcherError>>,
) {
    let _death_guard = WorkerDeathGuard::new(publication.clone());

    let initially_connected = match subscribe_and_refresh(source.as_mut(), uid, &publication) {
        Ok(()) => true,
        Err(_) => {
            publication.publish(
                SessionDecision::Unauthorized(SessionUnavailableReason::SourceUnavailable),
                monotonic_ms(),
            );
            false
        }
    };
    if ready_tx.send(Ok(())).is_err() {
        return;
    }
    if !initially_connected
        && !reconnect_source(source.as_mut(), uid, &publication, &stop_rx, timing)
    {
        return;
    }

    let mut last_refresh = Instant::now();
    loop {
        if stop_requested(&stop_rx) {
            return;
        }

        let elapsed = last_refresh.elapsed();
        if elapsed >= timing.refresh_interval {
            match refresh_decision(source.as_mut(), uid, &publication) {
                Ok(()) => {
                    last_refresh = Instant::now();
                    continue;
                }
                Err(_) => {
                    publication.publish(
                        SessionDecision::Unauthorized(SessionUnavailableReason::SourceUnavailable),
                        monotonic_ms(),
                    );
                    if !reconnect_source(source.as_mut(), uid, &publication, &stop_rx, timing) {
                        return;
                    }
                    last_refresh = Instant::now();
                    continue;
                }
            }
        }

        let until_refresh = timing.refresh_interval.saturating_sub(elapsed);
        let wait = timing.wait_slice.min(until_refresh);
        match source.wait_for_change(wait) {
            Ok(SessionSourceEvent::Changed) => {
                if refresh_decision(source.as_mut(), uid, &publication).is_err() {
                    publication.publish(
                        SessionDecision::Unauthorized(SessionUnavailableReason::SourceUnavailable),
                        monotonic_ms(),
                    );
                    if !reconnect_source(source.as_mut(), uid, &publication, &stop_rx, timing) {
                        return;
                    }
                }
                last_refresh = Instant::now();
            }
            Ok(SessionSourceEvent::Timeout) => {}
            Err(_) => {
                publication.publish(
                    SessionDecision::Unauthorized(SessionUnavailableReason::SourceUnavailable),
                    monotonic_ms(),
                );
                if !reconnect_source(source.as_mut(), uid, &publication, &stop_rx, timing) {
                    return;
                }
                last_refresh = Instant::now();
            }
        }
    }
}

fn subscribe_and_refresh(
    source: &mut dyn SessionSource,
    uid: u32,
    publication: &SessionAccessPublication,
) -> Result<(), SwitcherError> {
    source.subscribe()?;
    refresh_decision(source, uid, publication)
}

fn refresh_decision(
    source: &mut dyn SessionSource,
    uid: u32,
    publication: &SessionAccessPublication,
) -> Result<(), SwitcherError> {
    let records = source.snapshot(uid)?;
    publication.publish(decide_session(uid, &records), monotonic_ms());
    Ok(())
}

fn reconnect_source(
    source: &mut dyn SessionSource,
    uid: u32,
    publication: &SessionAccessPublication,
    stop_rx: &async_channel::Receiver<()>,
    timing: MonitorTiming,
) -> bool {
    let mut backoff = timing.reconnect_initial;
    loop {
        if wait_for_stop(stop_rx, backoff, timing.wait_slice) {
            return false;
        }

        match subscribe_and_refresh(source, uid, publication) {
            Ok(()) => return true,
            Err(_) => publication.publish(
                SessionDecision::Unauthorized(SessionUnavailableReason::SourceUnavailable),
                monotonic_ms(),
            ),
        }
        backoff = backoff.saturating_mul(2).min(timing.reconnect_max);
    }
}

fn wait_for_stop(
    stop_rx: &async_channel::Receiver<()>,
    duration: Duration,
    wait_slice: Duration,
) -> bool {
    let started = Instant::now();
    loop {
        if stop_requested(stop_rx) {
            return true;
        }
        let remaining = duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return false;
        }
        thread::sleep(wait_slice.min(remaining));
    }
}

fn stop_requested(stop_rx: &async_channel::Receiver<()>) -> bool {
    match stop_rx.try_recv() {
        Ok(()) | Err(async_channel::TryRecvError::Closed) => true,
        Err(async_channel::TryRecvError::Empty) => false,
    }
}

pub(crate) fn monotonic_ms() -> u64 {
    static MONOTONIC_EPOCH: OnceLock<Instant> = OnceLock::new();
    let elapsed = MONOTONIC_EPOCH
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis();
    elapsed.min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SwitcherError;

    fn record(
        id: &str,
        uid: u32,
        seat: &str,
        session_type: &str,
        class: &str,
        active: bool,
        remote: bool,
    ) -> SessionRecord {
        SessionRecord {
            id: id.to_owned(),
            uid,
            seat: seat.to_owned(),
            session_type: session_type.to_owned(),
            class: class.to_owned(),
            active,
            remote,
        }
    }

    fn authorized(session_id: &str, seat: &str) -> SessionDecision {
        SessionDecision::Authorized(AuthorizedSession::new(session_id, seat))
    }

    #[test]
    fn exactly_one_active_local_graphical_session_is_authorized() {
        let decision = decide_session(
            1000,
            &[record("c2", 1000, "seat0", "x11", "user", true, false)],
        );

        assert_eq!(
            decision,
            SessionDecision::Authorized(AuthorizedSession::new("c2", "seat0"))
        );
    }

    #[test]
    fn remote_tty_inactive_and_other_uid_are_not_candidates() {
        let records = [
            record("remote", 1000, "seat0", "x11", "user", true, true),
            record("tty", 1000, "seat0", "tty", "user", true, false),
            record("inactive", 1000, "seat0", "wayland", "user", false, false),
            record("other", 1001, "seat0", "wayland", "user", true, false),
        ];

        assert_eq!(
            decide_session(1000, &records),
            SessionDecision::Unauthorized(SessionUnavailableReason::NoCandidate)
        );
    }

    #[test]
    fn two_active_graphical_sessions_for_uid_fail_closed() {
        let records = [
            record("c2", 1000, "seat0", "x11", "user", true, false),
            record("c8", 1000, "seat1", "wayland", "user", true, false),
        ];

        assert_eq!(
            decide_session(1000, &records),
            SessionDecision::Unauthorized(SessionUnavailableReason::Ambiguous)
        );
    }

    #[test]
    fn same_session_refresh_renews_without_changing_generation() {
        let publication = SessionAccessPublication::new_for_test(3_000);
        publication.publish(authorized("c2", "seat0"), 10);
        let first = publication.backend_lease(11).unwrap();

        publication.publish(authorized("c2", "seat0"), 900);
        let second = publication.backend_lease(901).unwrap();

        assert_eq!(first.generation(), second.generation());
    }

    #[test]
    fn session_change_invalidates_old_token_before_new_token_is_visible() {
        let publication = SessionAccessPublication::new_for_test(3_000);
        publication.publish(authorized("c2", "seat0"), 10);
        let old = publication.backend_lease(11).unwrap();

        publication.publish(authorized("c8", "seat0"), 20);

        assert!(matches!(
            old.ensure_current(21),
            Err(SwitcherError::InputSessionInactive)
        ));
    }

    #[test]
    fn stale_lease_and_dead_worker_fail_closed() {
        let publication = SessionAccessPublication::new_for_test(3_000);
        publication.publish(authorized("c2", "seat0"), 10);

        assert!(publication.backend_lease(3_011).is_err());

        publication.mark_worker_stopped();
        assert!(matches!(
            publication.health_error(3_011),
            Some(SwitcherError::SessionMonitorStopped)
        ));
    }
}

#[cfg(test)]
mod monitor_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Clone, Copy)]
    enum FakeEvent {
        Changed,
        Disconnect,
        Panic,
    }

    struct FakeSessionSource {
        trace: Arc<Mutex<Vec<&'static str>>>,
        snapshots: VecDeque<Vec<SessionRecord>>,
        current_snapshot: Vec<SessionRecord>,
        events: mpsc::Receiver<FakeEvent>,
        subscribe_calls: Arc<AtomicUsize>,
        subscribe_failures_remaining: usize,
        reconnect_gate: Option<mpsc::Receiver<()>>,
    }

    impl SessionSource for FakeSessionSource {
        fn subscribe(&mut self) -> Result<(), SwitcherError> {
            self.trace.lock().unwrap().push("subscribe");
            let call = self.subscribe_calls.fetch_add(1, Ordering::SeqCst);
            if self.subscribe_failures_remaining > 0 {
                self.subscribe_failures_remaining -= 1;
                return Err(std::io::Error::other("injected initial subscribe failure").into());
            }
            if call > 0 {
                if let Some(gate) = self.reconnect_gate.take() {
                    gate.recv().expect("test must release reconnect");
                }
            }
            Ok(())
        }

        fn snapshot(&mut self, _uid: u32) -> Result<Vec<SessionRecord>, SwitcherError> {
            self.trace.lock().unwrap().push("snapshot");
            if let Some(snapshot) = self.snapshots.pop_front() {
                self.current_snapshot = snapshot;
            }
            Ok(self.current_snapshot.clone())
        }

        fn wait_for_change(
            &mut self,
            timeout: Duration,
        ) -> Result<SessionSourceEvent, SwitcherError> {
            self.trace.lock().unwrap().push("wait");
            match self.events.recv_timeout(timeout) {
                Ok(FakeEvent::Changed) => Ok(SessionSourceEvent::Changed),
                Ok(FakeEvent::Disconnect) => {
                    Err(std::io::Error::other("injected session source disconnect").into())
                }
                Ok(FakeEvent::Panic) => panic!("injected session source panic"),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(SessionSourceEvent::Timeout),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(std::io::Error::other("fake event sender disconnected").into())
                }
            }
        }
    }

    struct FakeControl {
        trace: Arc<Mutex<Vec<&'static str>>>,
        events: mpsc::Sender<FakeEvent>,
        subscribe_calls: Arc<AtomicUsize>,
        reconnect_gate: Option<mpsc::Sender<()>>,
    }

    fn fake_source(
        snapshots: Vec<Vec<SessionRecord>>,
        gate_reconnect: bool,
    ) -> (Box<dyn SessionSource>, FakeControl) {
        fake_source_with_subscribe_failures(snapshots, gate_reconnect, 0)
    }

    fn fake_source_with_subscribe_failures(
        snapshots: Vec<Vec<SessionRecord>>,
        gate_reconnect: bool,
        subscribe_failures_remaining: usize,
    ) -> (Box<dyn SessionSource>, FakeControl) {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let subscribe_calls = Arc::new(AtomicUsize::new(0));
        let (events_tx, events_rx) = mpsc::channel();
        let (reconnect_gate, reconnect_gate_rx) = if gate_reconnect {
            let (tx, rx) = mpsc::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let source = FakeSessionSource {
            trace: Arc::clone(&trace),
            snapshots: snapshots.into(),
            current_snapshot: Vec::new(),
            events: events_rx,
            subscribe_calls: Arc::clone(&subscribe_calls),
            subscribe_failures_remaining,
            reconnect_gate: reconnect_gate_rx,
        };
        let control = FakeControl {
            trace,
            events: events_tx,
            subscribe_calls,
            reconnect_gate,
        };
        (Box::new(source), control)
    }

    fn graphical_session(id: &str) -> Vec<SessionRecord> {
        vec![SessionRecord {
            id: id.to_owned(),
            uid: 1000,
            seat: "seat0".to_owned(),
            session_type: "x11".to_owned(),
            class: "user".to_owned(),
            active: true,
            remote: false,
        }]
    }

    fn test_timing(refresh: Duration) -> MonitorTiming {
        MonitorTiming {
            refresh_interval: refresh,
            wait_slice: Duration::from_millis(2),
            reconnect_initial: Duration::from_millis(2),
            reconnect_max: Duration::from_millis(8),
        }
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !predicate() {
            assert!(
                Instant::now() < deadline,
                "condition was not reached before test deadline"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn published_session(publication: &SessionAccessPublication) -> Option<String> {
        publication
            .backend_lease(monotonic_ms())
            .ok()
            .map(|lease| lease.session_id().to_owned())
    }

    #[test]
    fn subscribe_precedes_first_snapshot() {
        let (source, control) = fake_source(vec![graphical_session("c2")], false);
        let mut monitor = SessionActivityMonitor::start_with_source_for_test(
            source,
            1000,
            test_timing(Duration::from_secs(5)),
        )
        .unwrap();

        assert_eq!(
            &control.trace.lock().unwrap()[..2],
            &["subscribe", "snapshot"]
        );
        assert!(monitor.stop().is_ok());
    }

    #[test]
    fn source_signal_triggers_immediate_authoritative_snapshot() {
        let (source, control) = fake_source(
            vec![graphical_session("c2"), graphical_session("c8")],
            false,
        );
        let mut monitor = SessionActivityMonitor::start_with_source_for_test(
            source,
            1000,
            test_timing(Duration::from_secs(5)),
        )
        .unwrap();
        let publication = monitor.publication();
        assert_eq!(published_session(&publication).as_deref(), Some("c2"));

        control.events.send(FakeEvent::Changed).unwrap();
        wait_until(|| published_session(&publication).as_deref() == Some("c8"));

        assert!(monitor.stop().is_ok());
    }

    #[test]
    fn timeout_triggers_periodic_authoritative_snapshot() {
        let (source, _control) = fake_source(
            vec![graphical_session("c2"), graphical_session("c8")],
            false,
        );
        let mut monitor = SessionActivityMonitor::start_with_source_for_test(
            source,
            1000,
            test_timing(Duration::from_millis(20)),
        )
        .unwrap();
        let publication = monitor.publication();

        wait_until(|| published_session(&publication).as_deref() == Some("c8"));

        assert!(monitor.stop().is_ok());
    }

    #[test]
    fn disconnect_invalidates_then_reconnects_and_refreshes() {
        let (source, mut control) =
            fake_source(vec![graphical_session("c2"), graphical_session("c8")], true);
        let mut monitor = SessionActivityMonitor::start_with_source_for_test(
            source,
            1000,
            test_timing(Duration::from_secs(5)),
        )
        .unwrap();
        let publication = monitor.publication();

        control.events.send(FakeEvent::Disconnect).unwrap();
        wait_until(|| {
            matches!(
                publication.health_error(monotonic_ms()),
                Some(SwitcherError::InputSessionInactive)
            )
        });
        wait_until(|| control.subscribe_calls.load(Ordering::SeqCst) >= 2);

        control.reconnect_gate.take().unwrap().send(()).unwrap();
        wait_until(|| published_session(&publication).as_deref() == Some("c8"));

        assert!(monitor.stop().is_ok());
    }

    #[test]
    fn initial_source_failure_starts_fail_closed_then_reconnects() {
        let (source, mut control) =
            fake_source_with_subscribe_failures(vec![graphical_session("c2")], true, 1);
        let mut monitor = SessionActivityMonitor::start_with_source_for_test(
            source,
            1000,
            test_timing(Duration::from_secs(5)),
        )
        .expect("source outage must not hide the diagnostic daemon endpoint");
        let publication = monitor.publication();

        assert!(matches!(
            publication.health_error(monotonic_ms()),
            Some(SwitcherError::InputSessionInactive)
        ));
        wait_until(|| control.subscribe_calls.load(Ordering::SeqCst) >= 2);

        control.reconnect_gate.take().unwrap().send(()).unwrap();
        wait_until(|| published_session(&publication).as_deref() == Some("c2"));

        assert!(monitor.stop().is_ok());
    }

    #[test]
    fn stop_and_join_is_idempotent_and_marks_worker_dead() {
        let (source, _control) = fake_source(vec![graphical_session("c2")], false);
        let mut monitor = SessionActivityMonitor::start_with_source_for_test(
            source,
            1000,
            test_timing(Duration::from_secs(5)),
        )
        .unwrap();
        let publication = monitor.publication();

        assert!(monitor.stop().is_ok());
        assert!(monitor.stop().is_ok());
        assert!(matches!(
            publication.health_error(monotonic_ms()),
            Some(SwitcherError::SessionMonitorStopped)
        ));
    }

    #[test]
    fn worker_panic_marks_publication_dead() {
        let (source, control) = fake_source(vec![graphical_session("c2")], false);
        let mut monitor = SessionActivityMonitor::start_with_source_for_test(
            source,
            1000,
            test_timing(Duration::from_secs(5)),
        )
        .unwrap();
        let publication = monitor.publication();

        control.events.send(FakeEvent::Panic).unwrap();
        wait_until(|| {
            matches!(
                publication.health_error(monotonic_ms()),
                Some(SwitcherError::SessionMonitorStopped)
            )
        });

        assert!(monitor.stop().is_err());
    }
}
