use crate::error::SwitcherError;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

const SESSION_LEASE_TTL_MS: u64 = 3_000;

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
    fn new_for_test(lease_ttl_ms: u64) -> Self {
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
    pub(crate) fn ensure_current(&self, now_ms: u64) -> Result<(), SwitcherError> {
        self.publication.ensure_available(now_ms)?;
        if self.publication.inner.generation.load(Ordering::Acquire) != self.generation {
            return Err(SwitcherError::InputSessionInactive);
        }
        Ok(())
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn session_id(&self) -> &str {
        self.session.session_id()
    }

    pub(crate) fn seat(&self) -> &str {
        self.session.seat()
    }
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
