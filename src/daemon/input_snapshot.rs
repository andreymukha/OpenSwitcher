use crate::daemon::runtime::RuntimeConfigSnapshot;
use crate::layout_backend::{AppLayoutKind, CurrentLayoutState, FeatureAvailability};
use crate::model::SessionType;
use std::sync::{RwLock, TryLockError};
use std::time::{Duration, Instant};

pub(crate) const INPUT_LAYOUT_POLL_INTERVAL: Duration = Duration::from_millis(300);
pub(crate) const INPUT_LAYOUT_FRESHNESS: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputLayoutStatus {
    Fresh,
    AwaitingConfirmation,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotAuthorization {
    pub config_generation: u64,
    pub layout_generation: u64,
    pub layout_epoch: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct InputRuntimeSnapshot {
    pub config: RuntimeConfigSnapshot,
    pub enabled: bool,
    pub features: FeatureAvailability,
    pub session_type: SessionType,
    pub layout_state: CurrentLayoutState,
    pub config_generation: u64,
    pub layout_generation: u64,
    pub confirmed_layout_epoch: u64,
    pub confirmed_at: Option<Instant>,
}

impl InputRuntimeSnapshot {
    pub(crate) fn layout_status_at(
        &self,
        now: Instant,
        current_layout_epoch: u64,
    ) -> InputLayoutStatus {
        if self.confirmed_layout_epoch != current_layout_epoch {
            return InputLayoutStatus::AwaitingConfirmation;
        }
        if matches!(self.layout_state, CurrentLayoutState::Unknown { .. }) {
            return InputLayoutStatus::Unknown;
        }
        let Some(confirmed_at) = self.confirmed_at else {
            return InputLayoutStatus::Unknown;
        };
        if now.checked_duration_since(confirmed_at).unwrap_or_default() >= INPUT_LAYOUT_FRESHNESS {
            return InputLayoutStatus::Stale;
        }
        InputLayoutStatus::Fresh
    }

    pub(crate) fn layout_kind_for_decision_at(
        &self,
        now: Instant,
        current_layout_epoch: u64,
    ) -> Option<AppLayoutKind> {
        (self.layout_status_at(now, current_layout_epoch) == InputLayoutStatus::Fresh)
            .then(|| current_layout_kind(&self.layout_state))
    }

    pub(crate) fn authorization_at(
        &self,
        now: Instant,
        current_layout_epoch: u64,
    ) -> Option<SnapshotAuthorization> {
        self.layout_kind_for_decision_at(now, current_layout_epoch)?;
        Some(SnapshotAuthorization {
            config_generation: self.config_generation,
            layout_generation: self.layout_generation,
            layout_epoch: current_layout_epoch,
        })
    }

    pub(crate) fn authorizes_at(
        &self,
        authorization: SnapshotAuthorization,
        now: Instant,
        current_layout_epoch: u64,
    ) -> bool {
        self.authorization_at(now, current_layout_epoch) == Some(authorization)
    }
}

fn current_layout_kind(state: &CurrentLayoutState) -> AppLayoutKind {
    match state {
        CurrentLayoutState::Known { layout, .. } => layout.kind,
        CurrentLayoutState::Unknown { .. } => AppLayoutKind::Unknown,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum SnapshotTryLoad {
    Loaded(InputRuntimeSnapshot),
    Contended,
    Poisoned,
}

pub(crate) struct InputSnapshotPublication {
    inner: RwLock<InputRuntimeSnapshot>,
}

impl InputSnapshotPublication {
    pub(crate) fn new(initial: InputRuntimeSnapshot) -> Self {
        Self {
            inner: RwLock::new(initial),
        }
    }

    pub(crate) fn load_before_grab(&self) -> InputRuntimeSnapshot {
        self.inner
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(crate) fn load_for_non_input_consumer(&self) -> InputRuntimeSnapshot {
        self.load_before_grab()
    }

    pub(crate) fn try_load(&self) -> SnapshotTryLoad {
        match self.inner.try_read() {
            Ok(snapshot) => SnapshotTryLoad::Loaded(snapshot.clone()),
            Err(TryLockError::WouldBlock) => SnapshotTryLoad::Contended,
            Err(TryLockError::Poisoned(_)) => SnapshotTryLoad::Poisoned,
        }
    }

    pub(crate) fn update(&self, update: impl FnOnce(&mut InputRuntimeSnapshot)) {
        let mut snapshot = self
            .inner
            .write()
            .unwrap_or_else(|error| error.into_inner());
        update(&mut snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::daemon::runtime::RuntimeConfigSnapshot;
    use crate::layout_backend::{
        AppLayoutKind, CurrentLayoutState, FeatureAvailability, LayoutCode, SystemLayout,
    };
    use crate::model::SessionType;
    use std::sync::Arc;
    use std::thread;

    fn known_layout(kind: AppLayoutKind) -> CurrentLayoutState {
        let normalized_code = match kind {
            AppLayoutKind::English => LayoutCode::Us,
            AppLayoutKind::Russian => LayoutCode::Ru,
            AppLayoutKind::Other | AppLayoutKind::Unknown => LayoutCode::Unknown,
        };
        CurrentLayoutState::Known {
            layout: SystemLayout {
                backend_key: "test".to_string(),
                normalized_code,
                display_name: "Test".to_string(),
                kind,
                index: Some(0),
            },
            trustworthy: true,
        }
    }

    fn test_snapshot(
        layout_state: CurrentLayoutState,
        confirmed_at: Option<Instant>,
        confirmed_layout_epoch: u64,
    ) -> InputRuntimeSnapshot {
        InputRuntimeSnapshot {
            config: RuntimeConfigSnapshot::from(&AppConfig::default()),
            enabled: true,
            features: FeatureAvailability {
                auto_switch: true,
                manual_word_fix: true,
                selected_text_switch: true,
                reason: None,
            },
            session_type: SessionType::X11,
            layout_state,
            config_generation: 1,
            layout_generation: 2,
            confirmed_layout_epoch,
            confirmed_at,
        }
    }

    fn default_test_snapshot() -> InputRuntimeSnapshot {
        let now = Instant::now();
        test_snapshot(known_layout(AppLayoutKind::English), Some(now), 0)
    }

    #[test]
    fn confirmed_snapshot_remains_fresh_between_poll_ticks() {
        let confirmed_at = Instant::now();
        let snapshot = test_snapshot(known_layout(AppLayoutKind::English), Some(confirmed_at), 7);

        assert_eq!(
            snapshot.layout_status_at(confirmed_at + Duration::from_millis(900), 7),
            InputLayoutStatus::Fresh
        );
    }

    #[test]
    fn invalidation_epoch_disables_layout_actions_immediately() {
        let now = Instant::now();
        let snapshot = test_snapshot(known_layout(AppLayoutKind::English), Some(now), 7);

        assert_eq!(
            snapshot.layout_status_at(now + Duration::from_millis(1), 8),
            InputLayoutStatus::AwaitingConfirmation
        );
        assert_eq!(snapshot.layout_kind_for_decision_at(now, 8), None);
    }

    #[test]
    fn confirmation_expires_after_freshness_bound() {
        let confirmed_at = Instant::now();
        let snapshot = test_snapshot(known_layout(AppLayoutKind::Russian), Some(confirmed_at), 3);

        assert_eq!(
            snapshot.layout_status_at(
                confirmed_at + INPUT_LAYOUT_FRESHNESS + Duration::from_nanos(1),
                3,
            ),
            InputLayoutStatus::Stale
        );
    }

    #[test]
    fn pending_authorization_survives_same_state_reconfirmation() {
        let now = Instant::now();
        let snapshot = test_snapshot(known_layout(AppLayoutKind::English), Some(now), 4);
        let authorization = snapshot.authorization_at(now, 4).unwrap();
        let reconfirmed = InputRuntimeSnapshot {
            confirmed_at: Some(now + Duration::from_millis(300)),
            ..snapshot.clone()
        };

        assert!(reconfirmed.authorizes_at(authorization, now + Duration::from_millis(301), 4,));
    }

    #[test]
    fn contended_publication_returns_without_waiting() {
        let publication = InputSnapshotPublication::new(default_test_snapshot());
        let _guard = publication.inner.write().unwrap();

        assert!(matches!(publication.try_load(), SnapshotTryLoad::Contended));
    }

    #[test]
    fn poisoned_publication_is_explicit_and_non_panicking() {
        let publication = Arc::new(InputSnapshotPublication::new(default_test_snapshot()));
        let poison_target = Arc::clone(&publication);
        let _ = thread::spawn(move || {
            let _guard = poison_target.inner.write().unwrap();
            panic!("poison publication");
        })
        .join();

        assert!(matches!(publication.try_load(), SnapshotTryLoad::Poisoned));
    }

    #[test]
    fn publication_update_is_visible_to_blocking_consumers() {
        let publication = InputSnapshotPublication::new(default_test_snapshot());

        publication.update(|snapshot| snapshot.config_generation += 1);

        assert_eq!(publication.load_before_grab().config_generation, 2);
        assert_eq!(
            publication.load_for_non_input_consumer().config_generation,
            2
        );
    }
}
