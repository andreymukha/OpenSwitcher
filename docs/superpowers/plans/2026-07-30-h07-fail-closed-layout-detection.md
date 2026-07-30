# H-07 Fail-Closed Layout Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** перестать выдумывать US/RU при ошибке определения, сохранить обычную `us/ru` функциональность на Cinnamon/X11 и GNOME/Wayland и автоматически восстановиться после слишком раннего запуска.

**Architecture:** отдельный `layout_backend::detection` классифицирует setup из подтверждённого desktop-specific источника и возвращает явный `Confirmed`, `TemporarilyUnavailable` или `Unsupported`. Runtime хранит согласованный setup/features, подтверждает текущую группу через XKB либо GNOME MRU и выполняет setup retry только пока подтверждения нет; grabbed input path использует только уже существующий snapshot.

**Tech Stack:** Rust 2021, `x11rb` XKB, GSettings/`setxkbmap` через существующий `DesktopSettingsReader`, Cargo unit/fault-injection tests, Debian packaging, package-first QEMU VM profiles.

---

## Граница и структура файлов

Создаваемые файлы:

- `src/layout_backend/detection.rs` — чистая классификация X11/GNOME setup,
  явный outcome и сопоставление подтверждённой группы;
- `src/daemon/layout_setup_retry.rs` — маленький детерминированный backoff без
  внешнего I/O;
- `tests/fixtures/h07/layout-source-wrapper` — guest-only transient source
  wrapper для VM fault injection;
- `docs/audits/2026-07-30-h07-layout-detection-validation.md` — итоговые
  команды, exact DEB/SHA и результаты двух VM.

Изменяемые файлы:

- `src/layout_backend/mod.rs` — exports нового detection API;
- `src/layout_backend/backend.rs` — context-aware `detect_setup`;
- `src/layout_backend/backends/legacy.rs` — убрать default US/RU и хранить
  последний классифицированный setup;
- `src/layout_backend/registry.rs` — адаптировать test backend к новому
  контракту;
- `src/daemon/mod.rs` — подключить retry module;
- `src/daemon/runtime.rs` — ранний `SystemContext`, trusted X11/GNOME
  observation, retry и атомарная публикация features;
- `src/daemon/service.rs` — только regression tests, подтверждающие
  fail-closed и сохранение case-only поведения;
- `debian/control`, `debian/changelog` — runtime dependencies и версия
  `0.1.0-5`;
- `tests/debian_package_scripts_test.sh` — package dependency gate;
- `README.md` — команды, необходимые source/dev запуску;
- `docs/superpowers/specs/2026-07-30-h07-fail-closed-layout-detection-design.md`
  — статус `согласовано`.

Не создавать новый layout polling thread, D-Bus API, настройку пользователя,
input hook или универсальный desktop abstraction. Полный общий VM campaign
остаётся отдельным финальным этапом аудита.

## Правило gates

Каждая задача запускает только свой focused test. Полный Rust suite, shell
package gates и DEB build выполняются один раз в задаче 5. При найденном
blocker применяется узкий red-green-fix без перезапуска уже зелёных
несвязанных матриц.

---

### Task 1: Чистая context-aware классификация setup

**Files:**

- Create: `src/layout_backend/detection.rs`
- Modify: `src/layout_backend/mod.rs`

- [ ] **Step 1: написать failing tests для outcome и X11 parser**

В `src/layout_backend/detection.rs` сначала добавить test module с табличными
проверками:

```rust
#[test]
fn x11_exact_us_ru_and_gb_ru_are_strict_pairs() {
    for (query, english) in [
        ("layout: us,ru\nvariant: ,\n", LayoutCode::Us),
        ("layout: ru,gb\nvariant: ,\n", LayoutCode::Gb),
    ] {
        let LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { en, ru }) =
            detect_x11_setup_from_query(query)
        else {
            panic!("expected confirmed strict pair for {query:?}");
        };
        assert_eq!(en.normalized_code, english);
        assert_eq!(ru.normalized_code, LayoutCode::Ru);
        assert_ne!(en.index, ru.index);
    }
}

#[test]
fn x11_missing_pair_malformed_and_variant_fail_closed() {
    for query in [
        "",
        "rules: evdev\n",
        "layout: us\n",
        "layout: ru\n",
        "layout: us,us,ru\n",
        "layout: us,ru\nvariant: dvorak,\n",
        "layout: us,ru\nvariant: ,phonetic\n",
        "layout: us,ru\nvariant: \n",
    ] {
        assert!(matches!(
            detect_x11_setup_from_query(query),
            LayoutSetupDetection::Unsupported { .. }
        ));
    }
}

#[test]
fn x11_extra_plain_layout_is_pair_plus_other() {
    assert!(matches!(
        detect_x11_setup_from_query("layout: us,de,ru\nvariant: ,,"),
        LayoutSetupDetection::Confirmed(LayoutSetup::PairPlusOther {
            ref others,
            ..
        }) if others.len() == 1
    ));
}
```

- [ ] **Step 2: написать failing tests для GNOME sources и current mapping**

Добавить:

```rust
#[test]
fn gnome_pair_does_not_require_mru_to_confirm_setup() {
    let sources = flat_sources(&[("xkb", "us"), ("xkb", "ru")]);
    assert!(matches!(
        detect_gnome_setup_from_sources(&sources),
        LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { .. })
    ));
}

#[test]
fn gnome_ibus_variant_and_missing_pair_are_unsupported() {
    for sources in [
        flat_sources(&[("ibus", "typing-booster"), ("xkb", "ru")]),
        flat_sources(&[("xkb", "us+dvorak"), ("xkb", "ru")]),
        flat_sources(&[("xkb", "us")]),
    ] {
        assert!(matches!(
            detect_gnome_setup_from_sources(&sources),
            LayoutSetupDetection::Unsupported { .. }
        ));
    }
}

#[test]
fn group_mapping_rejects_a_different_group_count() {
    let setup = test_strict_pair(LayoutCode::Us);
    assert!(matches!(
        current_layout_from_group(&setup, 0, 3),
        CurrentLayoutState::Unknown { .. }
    ));
}

#[test]
fn pair_plus_other_maps_only_the_confirmed_index() {
    let setup = test_pair_plus_german();
    assert_eq!(
        current_layout_kind(&current_layout_from_group(&setup, 0, 3)),
        AppLayoutKind::English
    );
    assert_eq!(
        current_layout_kind(&current_layout_from_group(&setup, 1, 3)),
        AppLayoutKind::Other
    );
}
```

Определить test helpers в том же module, чтобы тесты были самодостаточны:

```rust
fn test_layout(code: LayoutCode, kind: AppLayoutKind, index: u32) -> SystemLayout {
    SystemLayout {
        backend_key: format!("test:{index}"),
        normalized_code: code,
        display_name: format!("{kind:?}"),
        kind,
        index: Some(index),
    }
}

fn test_strict_pair(english: LayoutCode) -> LayoutSetup {
    LayoutSetup::StrictPair {
        en: test_layout(english, AppLayoutKind::English, 0),
        ru: test_layout(LayoutCode::Ru, AppLayoutKind::Russian, 1),
    }
}

fn test_pair_plus_german() -> LayoutSetup {
    LayoutSetup::PairPlusOther {
        en: test_layout(LayoutCode::Us, AppLayoutKind::English, 0),
        ru: test_layout(LayoutCode::Ru, AppLayoutKind::Russian, 2),
        others: vec![test_layout(
            LayoutCode::from_normalized("de").unwrap(),
            AppLayoutKind::Other,
            1,
        )],
    }
}

fn flat_sources(values: &[(&str, &str)]) -> Vec<String> {
    values
        .iter()
        .flat_map(|(kind, id)| [(*kind).to_string(), (*id).to_string()])
        .collect()
}

fn current_layout_kind(state: &CurrentLayoutState) -> AppLayoutKind {
    match state {
        CurrentLayoutState::Known { layout, .. } => layout.kind,
        CurrentLayoutState::Unknown { .. } => AppLayoutKind::Unknown,
    }
}
```

- [ ] **Step 3: запустить tests и увидеть ожидаемый RED**

Run:

```bash
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib layout_backend::detection::tests -- --nocapture
```

Expected: compile/test failure, потому что detection API ещё не реализован.

- [ ] **Step 4: реализовать outcome и выбор источника**

Основной публичный контракт:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutSetupDetection {
    Confirmed(LayoutSetup),
    TemporarilyUnavailable { reason: String },
    Unsupported { reason: String },
}

impl LayoutSetupDetection {
    pub fn effective_setup(&self) -> LayoutSetup {
        match self {
            Self::Confirmed(setup) => setup.clone(),
            Self::TemporarilyUnavailable { reason } => LayoutSetup::Unsupported {
                reason: format!("temporarily-unavailable:{reason}"),
            },
            Self::Unsupported { reason } => LayoutSetup::Unsupported {
                reason: format!("unsupported:{reason}"),
            },
        }
    }

    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed(_))
    }
}

pub fn detect_layout_setup<R: DesktopSettingsReader>(
    context: SystemContext,
    reader: &R,
) -> LayoutSetupDetection {
    match (context.session_type, context.desktop_environment) {
        (SessionType::X11, _) => match reader.setxkbmap_query() {
            Ok(query) => detect_x11_setup_from_query(&query),
            Err(error) => LayoutSetupDetection::TemporarilyUnavailable {
                reason: format!("setxkbmap-query:{error}"),
            },
        },
        (SessionType::Wayland, DesktopEnvironment::Gnome) => {
            match reader.gsettings_string_list(GNOME_INPUT_SOURCES_SCHEMA, GNOME_SOURCES_KEY) {
                Ok(sources) => detect_gnome_setup_from_sources(&sources),
                Err(error) => LayoutSetupDetection::TemporarilyUnavailable {
                    reason: format!("gnome-sources:{error}"),
                },
            }
        }
        _ => LayoutSetupDetection::Unsupported {
            reason: "unsupported-session-context".to_string(),
        },
    }
}
```

`detect_x11_setup_from_query` обязан:

1. найти ровно одну непустую `layout:`;
2. разбить CSV с сохранением индексов;
3. считать отсутствующую `variant:` списком пустых variants, а присутствующую
   строку требовать согласованной по числу групп;
4. отклонить любой непустой variant;
5. классифицировать ровно одну English (`us|gb`), ровно одну `ru` и остальные
   валидные normalized codes;
6. вернуть `StrictPair` только при двух группах, иначе `PairPlusOther`.

GNOME parser принимает только чётные пары `source_type/source_id`, только
plain `xkb` ids и использует ту же общую функцию классификации.

`current_layout_from_group(setup, current_group, actual_num_groups)` сначала
сравнивает `actual_num_groups` с максимальным setup index + 1, затем возвращает
`Known { trustworthy: true }` только для точного index.

- [ ] **Step 5: экспортировать API и получить GREEN**

В `src/layout_backend/mod.rs`:

```rust
mod detection;

pub use detection::{
    current_layout_from_gnome_sources, current_layout_from_group, detect_layout_setup,
    LayoutSetupDetection,
};
```

Run:

```bash
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib layout_backend::detection::tests -- --nocapture
```

Expected: все detection tests PASS.

- [ ] **Step 6: commit**

```bash
git add src/layout_backend/detection.rs src/layout_backend/mod.rs
git commit -m "fix: classify layout setup from trusted sources"
```

---

### Task 2: Подключить trusted setup и current layout к backend/runtime

**Files:**

- Modify: `src/layout_backend/backend.rs`
- Modify: `src/layout_backend/backends/legacy.rs`
- Modify: `src/layout_backend/registry.rs`
- Modify: `src/daemon/runtime.rs`
- Modify: `src/daemon/service.rs`

- [ ] **Step 1: написать failing backend contract tests**

Заменить позитивные legacy tests и добавить отрицательные:

```rust
#[test]
fn legacy_backend_never_installs_default_pair_after_detection_failure() {
    let backend = LegacyLayoutBackend::new();
    let detection = backend.detect_setup_with_reader(
        x11_context(),
        &SetupReaderStub::failing(),
    );

    assert!(matches!(
        detection,
        LayoutSetupDetection::TemporarilyUnavailable { .. }
    ));
    assert!(matches!(
        backend.cached_setup(),
        LayoutSetup::Unsupported { .. }
    ));
}

#[test]
fn gnome_wayland_backend_uses_gsettings_not_setxkbmap() {
    let reader = SetupReaderStub::gnome_us_ru();
    let backend = LegacyLayoutBackend::new();

    assert!(matches!(
        backend.detect_setup_with_reader(gnome_wayland_context(), &reader),
        LayoutSetupDetection::Confirmed(LayoutSetup::StrictPair { .. })
    ));
    assert_eq!(reader.gsettings_calls(), 1);
    assert_eq!(reader.setxkbmap_calls(), 0);
}
```

Локальный stub в `legacy.rs` tests:

```rust
struct SetupReaderStub {
    gnome_sources: Option<Vec<String>>,
    x11_query: Option<String>,
    gsettings_calls: AtomicUsize,
    setxkbmap_calls: AtomicUsize,
}

impl SetupReaderStub {
    fn failing() -> Self {
        Self {
            gnome_sources: None,
            x11_query: None,
            gsettings_calls: AtomicUsize::new(0),
            setxkbmap_calls: AtomicUsize::new(0),
        }
    }

    fn gnome_us_ru() -> Self {
        Self {
            gnome_sources: Some(vec![
                "xkb".into(),
                "us".into(),
                "xkb".into(),
                "ru".into(),
            ]),
            ..Self::failing()
        }
    }

    fn gsettings_calls(&self) -> usize {
        self.gsettings_calls.load(Ordering::SeqCst)
    }

    fn setxkbmap_calls(&self) -> usize {
        self.setxkbmap_calls.load(Ordering::SeqCst)
    }
}

impl DesktopSettingsReader for SetupReaderStub {
    fn gsettings_string_list(
        &self,
        _schema: &str,
        _key: &str,
    ) -> Result<Vec<String>, LayoutAutoDetectError> {
        self.gsettings_calls.fetch_add(1, Ordering::SeqCst);
        self.gnome_sources.clone().ok_or_else(|| {
            LayoutAutoDetectError::GSettingsFailed {
                stderr: "injected unavailable".to_string(),
            }
        })
    }

    fn xfconf_string(
        &self,
        _channel: &str,
        _property: &str,
    ) -> Result<String, LayoutAutoDetectError> {
        Err(LayoutAutoDetectError::XfconfFailed {
            stderr: "unused".to_string(),
        })
    }

    fn xfconf_bool(
        &self,
        _channel: &str,
        _property: &str,
    ) -> Result<bool, LayoutAutoDetectError> {
        Err(LayoutAutoDetectError::XfconfFailed {
            stderr: "unused".to_string(),
        })
    }

    fn setxkbmap_query(&self) -> Result<String, LayoutAutoDetectError> {
        self.setxkbmap_calls.fetch_add(1, Ordering::SeqCst);
        self.x11_query.clone().ok_or_else(|| {
            LayoutAutoDetectError::SetXkbMapFailed {
                stderr: "injected unavailable".to_string(),
            }
        })
    }
}
```

В runtime tests добавить:

```rust
#[test]
fn generic_x11_rejects_untrusted_legacy_led_state() {
    let runtime = runtime_with_confirmed_setup(x11_context(), strict_pair_setup());
    runtime.refresh_current_layout_observation_with_readers(
        &unused_gsettings_reader(),
        &X11GroupStateReaderStub::error("xkb unavailable"),
    );

    assert!(matches!(
        runtime.current_layout_state(),
        CurrentLayoutState::Unknown { .. }
    ));
}

#[test]
fn x11_group_state_confirms_us_and_ru_for_exact_pair() {
    let setup = strict_pair_setup();
    assert_known_kind(
        x11_current_layout_state_from_group(X11GroupState::new(0, 2), &setup),
        AppLayoutKind::English,
    );
    assert_known_kind(
        x11_current_layout_state_from_group(X11GroupState::new(1, 2), &setup),
        AppLayoutKind::Russian,
    );
}
```

- [ ] **Step 2: запустить focused tests и увидеть RED**

```bash
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib legacy_backend -- --nocapture
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib x11_group_state -- --nocapture
```

Expected: FAIL/compile failure до изменения trait/runtime.

- [ ] **Step 3: сделать `detect_setup` context-aware**

В `LayoutBackend`:

```rust
fn detect_setup(&self, context: SystemContext) -> LayoutSetupDetection;
```

Обновить test backends в `registry.rs` и `runtime.rs`, возвращая:

```rust
LayoutSetupDetection::Unsupported {
    reason: "test-backend-does-not-detect-setup".to_string(),
}
```

`LegacyLayoutBackend` хранит:

```rust
struct LegacyLayoutBackend {
    setup: RwLock<LayoutSetup>,
}
```

Его production и injected пути:

```rust
fn detect_setup(&self, context: SystemContext) -> LayoutSetupDetection {
    self.detect_setup_with_reader(context, &CommandDesktopSettingsReader)
}

fn detect_setup_with_reader<R: DesktopSettingsReader>(
    &self,
    context: SystemContext,
    reader: &R,
) -> LayoutSetupDetection {
    let detection = detect_layout_setup(context, reader);
    *self.setup.write().unwrap_or_else(|error| error.into_inner()) =
        detection.effective_setup();
    detection
}
```

Удалить `default_legacy_layout_pair()` целиком. Старый raw LED snapshot
сохраняется только как `trustworthy:false`; runtime больше не принимает его
как доказательство ни в одном X11 context.

- [ ] **Step 4: определить `SystemContext` до backend initialization**

В `RuntimeState::new` порядок становится:

```rust
let system_context = SystemContextDetector::detect_current().unwrap_or_default();
let (backend, layout_state, setup_state, setup_detection) =
    Self::initialize_layout_backend(system_context);
```

`initialize_layout_backend(context)` вызывает `backend.detect_setup(context)`,
строит compatibility/features из `detection.effective_setup()` и возвращает
сам outcome для retry state задачи 3.

- [ ] **Step 5: обобщить Cinnamon reader до X11 group state**

Заменить `CinnamonCurrentGroupReader` на:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct X11GroupState {
    current_group: u8,
    num_groups: u8,
}

trait X11GroupStateReader {
    fn x11_group_state(&self) -> Result<X11GroupState, String>;
}
```

Production reader выполняет `xkb_use_extension`, затем:

```rust
let controls = connection
    .xkb_get_controls(xkb::ID::USE_CORE_KBD.into())?
    .reply()?;
let state = connection
    .xkb_get_state(xkb::ID::USE_CORE_KBD.into())?
    .reply()?;
Ok(X11GroupState {
    current_group: u8::from(state.group),
    num_groups: controls.num_groups,
})
```

Любой `SessionType::X11` использует
`current_layout_from_group(&setup, current_group, num_groups)`. GNOME Wayland
использует `current_layout_from_gnome_sources`; остальные contexts дают
`Unknown`.

`effective_current_layout_state` обязан отвергать raw
`trustworthy:false` для всех X11, не только Cinnamon.

- [ ] **Step 6: сохранить case-only и запретить layout switch при extra layout**

Добавить service policy regression:

```rust
#[test]
fn pair_plus_other_disables_layout_switch_but_keeps_confirmed_case_fix() {
    let now = Instant::now();
    let mut snapshot = fresh_service_snapshot(AppLayoutKind::English, now);
    snapshot.features = feature_availability_for(
        LayoutCompatibility::PairPlusOther,
        BackendCapabilities {
            can_read_current_layout: true,
            can_switch_next: true,
            can_map_layouts_to_app_kinds: true,
            ..Default::default()
        },
    );
    snapshot.config.fix_two_capitals = true;
    assert!(!snapshot.features.auto_switch);
    assert!(!snapshot.features.manual_word_fix);
    assert!(same_layout_fixes_allowed(
        &snapshot,
        now,
        snapshot.confirmed_layout_epoch,
    ));
}
```

Никаких новых веток desktop environment в `DaemonService` не добавлять.

- [ ] **Step 7: запустить focused GREEN**

```bash
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib layout_backend:: -- --nocapture
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib daemon::runtime::tests::x11_ -- --nocapture
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib pair_plus_other_disables_layout_switch -- --nocapture
```

Expected: PASS; ни один test не обращается к host display.

- [ ] **Step 8: commit**

```bash
git add src/layout_backend/backend.rs \
  src/layout_backend/backends/legacy.rs \
  src/layout_backend/registry.rs \
  src/daemon/runtime.rs src/daemon/service.rs
git commit -m "fix: require trusted layout setup and current group"
```

---

### Task 3: Восстановление setup и snapshot publication

**Files:**

- Create: `src/daemon/layout_setup_retry.rs`
- Modify: `src/daemon/mod.rs`
- Modify: `src/daemon/runtime.rs`

- [ ] **Step 1: написать failing retry state tests**

```rust
#[test]
fn retry_uses_bounded_backoff_and_stops_after_confirmation() {
    let start = Instant::now();
    let mut retry = LayoutSetupRetry::pending_at(start);
    let expected = [1, 2, 5, 10, 30, 30];
    let mut previous = start;

    for seconds in expected {
        let due = retry.next_due().unwrap();
        assert_eq!(
            due.duration_since(previous),
            Duration::from_secs(seconds),
        );
        retry.record_failure(due);
        previous = due;
    }

    retry.record_confirmed();
    assert_eq!(retry.next_due(), None);
}

#[test]
fn successful_mode_never_becomes_due_with_time() {
    let start = Instant::now();
    let retry = LayoutSetupRetry::confirmed();
    assert!(!retry.is_due(start + Duration::from_secs(86_400)));
}
```

- [ ] **Step 2: реализовать маленький backoff**

```rust
const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

pub(crate) struct LayoutSetupRetry {
    failures: usize,
    next_due: Option<Instant>,
}

impl LayoutSetupRetry {
    pub(crate) fn confirmed() -> Self {
        Self {
            failures: 0,
            next_due: None,
        }
    }

    pub(crate) fn pending_at(now: Instant) -> Self {
        Self {
            failures: 0,
            next_due: now.checked_add(RETRY_DELAYS[0]),
        }
    }

    pub(crate) fn from_detection(
        detection: &LayoutSetupDetection,
        now: Instant,
    ) -> Self {
        if detection.is_confirmed() {
            Self::confirmed()
        } else {
            Self {
                failures: 0,
                next_due: now.checked_add(RETRY_DELAYS[0]),
            }
        }
    }

    pub(crate) fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        let index = self.failures.min(RETRY_DELAYS.len() - 1);
        self.next_due = now.checked_add(RETRY_DELAYS[index]);
    }

    pub(crate) fn record_confirmed(&mut self) {
        self.failures = 0;
        self.next_due = None;
    }

    pub(crate) fn force_due(&mut self, now: Instant) {
        self.next_due = Some(now);
    }

    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.next_due
    }

    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.next_due.is_some_and(|due| now >= due)
    }
}
```

Run:

```bash
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib layout_setup_retry -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: объединить setup/compatibility/features в один runtime state**

В `runtime.rs` заменить три независимых `RwLock`:

```rust
#[derive(Clone)]
struct LayoutSetupRuntimeState {
    setup: LayoutSetup,
    compatibility: LayoutCompatibility,
    features: FeatureAvailability,
}

impl LayoutSetupRuntimeState {
    fn from_detection(
        detection: &LayoutSetupDetection,
        capabilities: BackendCapabilities,
    ) -> Self {
        let setup = detection.effective_setup();
        let compatibility = compatibility_from_setup(&setup);
        let features = feature_availability_for(compatibility, capabilities);
        Self {
            setup,
            compatibility,
            features,
        }
    }
}
```

`RuntimeState` получает:

```rust
layout_setup_state: RwLock<LayoutSetupRuntimeState>,
layout_setup_retry: Mutex<LayoutSetupRetry>,
```

Публичные getters читают один lock и возвращают соответствующее поле.

- [ ] **Step 4: написать failing runtime recovery tests**

Test backend возвращает очередь outcomes:

```rust
#[test]
fn unavailable_startup_recovers_features_without_daemon_restart() {
    let now = Instant::now();
    let runtime = runtime_with_pending_setup_outcomes(
        [LayoutSetupDetection::Confirmed(strict_pair_setup())],
        now,
    );

    assert!(!runtime.feature_availability().manual_word_fix);
    assert!(!runtime.maybe_redetect_layout_setup_at(now + Duration::from_millis(999)));
    assert!(runtime.maybe_redetect_layout_setup_at(now + Duration::from_secs(1)));
    assert!(runtime.feature_availability().manual_word_fix);
    assert_eq!(runtime.layout_setup_retry_due(), None);
}

#[test]
fn setup_change_invalidates_pending_snapshot_authorization() {
    let now = Instant::now();
    let runtime = runtime_with_confirmed_setup(x11_context(), strict_pair_setup());
    let before = runtime.input_snapshot_before_grab();
    let authorization = before.authorization_at(now, runtime.input_layout_epoch()).unwrap();

    runtime.apply_layout_setup_detection(
        LayoutSetupDetection::Unsupported {
            reason: "extra-layout".to_string(),
        },
        now,
    );

    let after = runtime.input_snapshot_before_grab();
    assert!(!after.authorizes_at(
        authorization,
        now,
        runtime.input_layout_epoch(),
    ));
    assert!(!after.features.manual_word_fix);
}
```

Добавить рядом с существующим `SnapshotBackend`:

```rust
struct SetupOutcomeBackend {
    outcomes: Mutex<VecDeque<LayoutSetupDetection>>,
}

impl LayoutBackend for SetupOutcomeBackend {
    fn id(&self) -> &'static str {
        "setup-outcome-test"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            can_list_layouts: true,
            can_read_current_layout: true,
            can_switch_next: true,
            can_map_layouts_to_app_kinds: true,
            ..Default::default()
        }
    }

    fn detect_setup(&self, _context: SystemContext) -> LayoutSetupDetection {
        self.outcomes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
            .expect("injected setup outcome")
    }

    fn current_layout_snapshot(&self) -> Result<CurrentLayoutState, LayoutBackendError> {
        Ok(CurrentLayoutState::Unknown {
            reason: "awaiting injected observation".to_string(),
        })
    }

    fn switch_to(&mut self, _target: &SystemLayout) -> Result<(), LayoutBackendError> {
        Err(LayoutBackendError::unsupported(
            self.id(),
            LayoutBackendOperation::SwitchTo,
        ))
    }

    fn switch_next(&mut self) -> Result<(), LayoutBackendError> {
        Err(LayoutBackendError::unsupported(
            self.id(),
            LayoutBackendOperation::SwitchNext,
        ))
    }

    fn start_monitoring(&mut self, _sink: LayoutStateSink) -> Result<(), LayoutBackendError> {
        Err(LayoutBackendError::unsupported(
            self.id(),
            LayoutBackendOperation::StartMonitoring,
        ))
    }
}

fn runtime_with_pending_setup_outcomes(
    outcomes: impl IntoIterator<Item = LayoutSetupDetection>,
    now: Instant,
) -> RuntimeState {
    let runtime = test_runtime_with_backend_and_context(
        CurrentLayoutState::Unknown {
            reason: "setup-pending".to_string(),
        },
        Box::new(SetupOutcomeBackend {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }),
        x11_context(),
    );
    *runtime
        .layout_setup_retry
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = LayoutSetupRetry::pending_at(now);
    runtime
}
```

- [ ] **Step 5: реализовать retry только в background coordinator**

`maybe_redetect_layout_setup_at`:

1. проверяет deadline под коротким retry lock;
2. отпускает retry lock;
3. вызывает `backend.detect_setup(current_context)` под backend lock;
4. на failure только переносит deadline;
5. на confirmed вызывает `apply_layout_setup_detection`;
6. после confirmed выключает retry.

Перед изменением setup:

```rust
self.layout_invalidation_epoch.fetch_add(1, Ordering::AcqRel);
```

Затем одной записью заменяется `LayoutSetupRuntimeState`, после чего
`input_snapshot.update`:

```rust
published.features = next.features.clone();
published.layout_state = CurrentLayoutState::Unknown {
    reason: "layout-setup-changed-awaiting-confirmation".to_string(),
};
published.layout_generation = published.layout_generation.saturating_add(1);
published.confirmed_at = None;
```

Это гарантирует, что input path либо видит старый snapshot с уже изменившимся
epoch, либо новый fail-closed snapshot.

Coordinator на каждом уже существующем timeout делает только дешёвую проверку
`is_due`. Внешний setup запрос запускается лишь при due; после success
`next_due=None`.

Late context upgrade немедленно ставит retry due и не сохраняет X11 setup как
доверенный GNOME Wayland setup.

GNOME current observation уже читает `sources`; классифицировать эти же
полученные значения повторно чистой функцией, не запускать второй `gsettings`.
Если sources изменились на `PairPlusOther`/`Unsupported`, применить новый setup
и инвалидировать snapshot. Если X11 сообщает другое `num_groups`, сделать
current layout `Unknown` и вызвать `force_due(now)`: это запускает один
background `setxkbmap`, а не steady-state polling.

- [ ] **Step 6: добавить bounded diagnostic**

Логировать только transitions:

```text
layout-setup-detection strategy=<...> result=<confirmed|temporary|unsupported>
compatibility=<...> generation=<...> reason=<redacted-technical-reason>
```

Не логировать typed text, selection, clipboard или полные environment values.
Одинаковый failure без изменения state не создаёт новую строку чаще retry
attempt.

- [ ] **Step 7: focused GREEN**

```bash
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib layout_setup_retry -- --nocapture
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib setup_change_invalidates -- --nocapture
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib unavailable_startup_recovers -- --nocapture
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib pending_commit_is_cancelled_after_layout_generation_change \
  -- --nocapture
```

Expected: PASS.

- [ ] **Step 8: commit**

```bash
git add src/daemon/layout_setup_retry.rs src/daemon/mod.rs src/daemon/runtime.rs
git commit -m "fix: recover trusted layout setup off the input path"
```

---

### Task 4: Зафиксировать runtime dependencies в основном DEB

**Files:**

- Modify: `tests/debian_package_scripts_test.sh`
- Modify: `debian/control`
- Modify: `debian/changelog`
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-07-30-h07-fail-closed-layout-detection-design.md`
- Create: `tests/fixtures/h07/layout-source-wrapper`

- [ ] **Step 1: написать failing package test**

Добавить отдельную функцию
`test_package_declares_layout_detection_runtime_dependencies` и вызвать её в
нижнем списке test functions:

```bash
assert_contains "$control" "x11-xkb-utils,"
assert_contains "$control" "libglib2.0-bin,"
assert_contains "$control" "gsettings-desktop-schemas,"
```

Run:

```bash
bash tests/debian_package_scripts_test.sh
```

Expected: FAIL на первой отсутствующей зависимости.

- [ ] **Step 2: добавить зависимости**

`debian/control`:

```debcontrol
Depends:
 ${shlibs:Depends},
 ${misc:Depends},
 systemd,
 udev (>= 247),
 acl,
 dbus-user-session,
 x11-xkb-utils,
 libglib2.0-bin,
 gsettings-desktop-schemas,
```

- [ ] **Step 3: поднять package revision**

В начало `debian/changelog`:

```text
open-switcher (0.1.0-5) unstable; urgency=high

  * Close H-07 by requiring a confirmed EN/RU layout setup before any
    layout-dependent text mutation.
  * Detect Cinnamon/X11 through setxkbmap plus XKB group state and
    GNOME/Wayland through configured GSettings input sources.
  * Recover layout features after an early unavailable desktop session without
    adding setup commands to the grabbed input path.

 -- Andrey Mukha <6871314+andreymukha@users.noreply.github.com>  Thu, 30 Jul 2026 20:57:39 +0300
```

- [ ] **Step 4: обновить README и статус spec**

В README перенести `libglib2.0-bin` из optional-only формулировки и кратко
указать runtime tools `x11-xkb-utils`, `libglib2.0-bin`,
`gsettings-desktop-schemas`. Для установки DEB пояснить, что APT устанавливает
их автоматически.

Статус design doc оставить `согласовано`.

- [ ] **Step 5: добавить фиксированный VM wrapper**

`tests/fixtures/h07/layout-source-wrapper`:

```sh
#!/bin/sh
set -eu

if [ ! -e /tmp/h07-layout-source-ready ]; then
    exit 42
fi

tool="$(basename -- "$0")"
case "$tool" in
    gsettings|setxkbmap)
        exec "/usr/bin/$tool" "$@"
        ;;
    *)
        exit 64
        ;;
esac
```

Проверить:

```bash
sh -n tests/fixtures/h07/layout-source-wrapper
```

- [ ] **Step 6: package GREEN и commit**

```bash
bash tests/debian_package_scripts_test.sh
git diff --check
git add debian/control debian/changelog README.md \
  tests/debian_package_scripts_test.sh \
  tests/fixtures/h07/layout-source-wrapper \
  docs/superpowers/specs/2026-07-30-h07-fail-closed-layout-detection-design.md
git commit -m "packaging: require layout detection tools"
```

Expected: shell gate PASS; commit содержит только перечисленные файлы.

---

### Task 5: Один объединённый local gate и exact DEB

**Files:**

- No source edits expected
- Generate, do not commit:
  `dist/packages/open-switcher_0.1.0-5_amd64.deb`

- [ ] **Step 1: формат и focused gates**

```bash
cargo fmt --all -- --check
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib layout_backend:: -- --test-threads=1
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib daemon::runtime::tests:: -- --test-threads=1
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --lib daemon::service::service_snapshot_ \
  -- --test-threads=1
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
git diff --check
```

Expected: все команды PASS. Если `cargo fmt --check` сообщает только
изменённые H-07 файлы, выполнить `cargo fmt` и повторить этот шаг; не
форматировать несвязанные пользовательские файлы.

- [ ] **Step 2: один полный safe suite**

```bash
CARGO_TARGET_DIR=/home/andrey/Projects/OpenSwitcher/target \
  cargo test --locked --all-targets --features settings-ui -- --test-threads=1
```

Expected: не меньше baseline `973 passed`, `0 failed`, один допустимый ignored.
Тесты с реальными `/dev/input`, `/dev/uinput`, clipboard или host session здесь
не запускаются.

- [ ] **Step 3: собрать основной DEB один раз**

```bash
DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb
package="$(dpkg-parsechangelog -S Source)"
version="$(dpkg-parsechangelog -S Version)"
arch="$(dpkg --print-architecture)"
CANDIDATE_DEB="$(realpath "dist/packages/${package}_${version}_${arch}.deb")"
test "$version" = "0.1.0-5"
test -f "$CANDIDATE_DEB"
sha256sum "$CANDIDATE_DEB"
dpkg-deb --field "$CANDIDATE_DEB" Package Version Architecture Depends
```

Expected: exact `open-switcher_0.1.0-5_amd64.deb`; Depends содержит все три
layout tools. SHA-256 сохранить для Task 6, новую сборку между VM не делать.

- [ ] **Step 4: read-only diff review**

Проверить диапазон от `7e5046e` до нового HEAD:

```bash
git diff --stat 7e5046e..HEAD
git diff --check 7e5046e..HEAD
git status --short
```

Review questions:

- может ли любой error снова создать `StrictPair`;
- может ли input path вызвать detector;
- может ли старый snapshot разрешить pending correction после setup change;
- прекращается ли retry после success;
- остаётся ли обычная `us/ru` функциональность включённой;
- не попали ли в commits старые пользовательские untracked docs.

Critical/High review findings исправить до VM узким TDD-cycle.

---

### Task 6: Целевая package-first проверка в двух VM

**Files:**

- Create:
  `docs/audits/2026-07-30-h07-layout-detection-validation.md`
- External evidence only:
  `/home/andrey/VMs/OpenSwitcherLab/runs/.../h07-layout-detection/`

VM запускаются строго последовательно. Лабораторию, base images, overlays,
ключи и старые evidence не удалять.

- [ ] **Step 1: запустить Mint/Cinnamon/X11**

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session mint-installed
```

Expected: JSON с port `22223`.

- [ ] **Step 2: передать и установить exact DEB**

```bash
H07_WT=/home/andrey/Projects/OpenSwitcher/.worktrees/h07-fail-closed-layout-detection
KEY=/home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519
KNOWN_HOSTS=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts
package="$(cd "$H07_WT" && dpkg-parsechangelog -S Source)"
version="$(cd "$H07_WT" && dpkg-parsechangelog -S Version)"
arch="$(dpkg --print-architecture)"
CANDIDATE_DEB="$H07_WT/dist/packages/${package}_${version}_${arch}.deb"
LAYOUT_SOURCE_WRAPPER="$H07_WT/tests/fixtures/h07/layout-source-wrapper"
test -f "$CANDIDATE_DEB"
test -f "$LAYOUT_SOURCE_WRAPPER"
ssh -i "$KEY" -p 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes openswitcher@127.0.0.1 \
  'install -d -m 0700 /home/openswitcher/h07'
scp -i "$KEY" -P 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes "$LAYOUT_SOURCE_WRAPPER" \
  openswitcher@127.0.0.1:/home/openswitcher/h07/layout-source-wrapper
scp -i "$KEY" -P 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes "$CANDIDATE_DEB" \
  openswitcher@127.0.0.1:/tmp/open-switcher-h07.deb
ssh -i "$KEY" -p 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes openswitcher@127.0.0.1 \
  'sha256sum /tmp/open-switcher-h07.deb && sudo apt-get install --yes /tmp/open-switcher-h07.deb'
```

Сверить guest SHA с Task 5.

- [ ] **Step 3: проверить Mint normal и fail-closed**

В guest сохранить:

```bash
setxkbmap -query
systemctl --user restart open-switcher-daemon.service
systemctl --user is-active open-switcher-daemon.service
journalctl --user -u open-switcher-daemon.service -b --no-pager -n 200
```

Через обычный QEMU physical keyboard smoke проверить `us/ru`, F12,
автокоррекцию, Caps Lock и две заглавные.

Для extra-layout fail-closed внутри disposable Mint guest:

```bash
setxkbmap -layout us,de,ru -option grp:alt_shift_toggle
sleep 2
```

Убедиться через `setxkbmap -query`, что видны три layout. Набрать слово и
нажать F12: слово и раскладка не должны измениться. Это меняет только guest
X11 session; host layout/input не затрагиваются.

Для transient startup создать guest-only wrapper:

```bash
install -d -m 0700 /home/openswitcher/h07-bin
install -m 0700 /home/openswitcher/h07/layout-source-wrapper \
  /home/openswitcher/h07-bin/setxkbmap
rm -f /tmp/h07-layout-source-ready
systemctl --user set-environment \
  PATH=/home/openswitcher/h07-bin:/usr/local/bin:/usr/bin:/bin
systemctl --user restart open-switcher-daemon.service
sleep 2
touch /tmp/h07-layout-source-ready
sleep 4
```

До marker коррекция не должна изменять текст; после marker обычная `us/ru`
коррекция должна восстановиться без restart daemon. Wrapper делегирует exact
production `/usr/bin/setxkbmap` после marker.

Штатно выключить Mint VM, overlay сохранить.

- [ ] **Step 4: повторить тем же SHA в Ubuntu/GNOME/Wayland**

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session ubuntu-installed
```

Expected: JSON с port `22222`.

Передать тот же `$CANDIDATE_DEB`:

```bash
ssh -i "$KEY" -p 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes openswitcher@127.0.0.1 \
  'install -d -m 0700 /home/openswitcher/h07'
scp -i "$KEY" -P 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes "$LAYOUT_SOURCE_WRAPPER" \
  openswitcher@127.0.0.1:/home/openswitcher/h07/layout-source-wrapper
scp -i "$KEY" -P 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes "$CANDIDATE_DEB" \
  openswitcher@127.0.0.1:/tmp/open-switcher-h07.deb
ssh -i "$KEY" -p 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes openswitcher@127.0.0.1 \
  'sha256sum /tmp/open-switcher-h07.deb && sudo apt-get install --yes /tmp/open-switcher-h07.deb'
```

В графической user session сохранить:

```bash
gsettings get org.gnome.desktop.input-sources sources
gsettings get org.gnome.desktop.input-sources mru-sources
systemctl --user restart open-switcher-daemon.service
systemctl --user is-active open-switcher-daemon.service
journalctl --user -u open-switcher-daemon.service -b --no-pager -n 200
```

Проверить ту же обычную `us/ru` функциональность. Отдельный ранний-start
scenario выполняется guest-only wrapper:

```bash
install -d -m 0700 /home/openswitcher/h07-bin
install -m 0700 /home/openswitcher/h07/layout-source-wrapper \
  /home/openswitcher/h07-bin/gsettings
rm -f /tmp/h07-layout-source-ready
systemctl --user set-environment \
  PATH=/home/openswitcher/h07-bin:/usr/local/bin:/usr/bin:/bin
systemctl --user restart open-switcher-daemon.service
sleep 2
touch /tmp/h07-layout-source-ready
sleep 4
```

До marker — fail-closed, после marker — функции восстанавливаются без restart.
После confirmed setup журнал не должен показывать новые setup transition,
хотя существующее чтение current GNOME source продолжает работать.

VM-проверка должна также подтвердить, что ранний отказ `gsettings` не оставил
рабочую коррекцию со стартовым `AutoFallback`: после marker ожидается
однократный `layout-switch-setup-recovery` с `SuperSpace`, новый config
generation и успешный F12 без restart daemon. Ручную комбинацию этот путь не
меняет и в steady state повторно не опрашивает.

Для extra-layout:

```bash
gsettings set org.gnome.desktop.input-sources sources \
  "[('xkb', 'us'), ('xkb', 'de'), ('xkb', 'ru')]"
sleep 2
```

F12/автокоррекция не должны изменять исходное слово; selected-text conversion
остаётся доступным. Все изменения находятся только внутри disposable guest.

- [ ] **Step 5: сохранить отчёт и commit**

`docs/audits/2026-07-30-h07-layout-detection-validation.md` содержит:

- branch/commit и clean/dirty identity;
- exact DEB path, version и SHA-256;
- результаты local gates;
- source values без пользовательского текста;
- Mint/Ubuntu package SHA agreement;
- normal, transient recovery и unsupported outcomes;
- подтверждение отсутствия нового steady-state setup polling;
- ограничения и остаточные риски;
- состояние VM после остановки и явное указание, что лаборатория сохранена.

```bash
git add docs/audits/2026-07-30-h07-layout-detection-validation.md
git commit -m "docs: validate fail-closed layout detection"
```

Expected: один отчётный commit; VM выключены, но не удалены.

---

## Финальная точка остановки

После Task 6 не начинать H-02/M-07 или clipboard задачи. Сообщить:

- какие commits созданы;
- сколько тестов прошло;
- exact DEB и SHA;
- результаты обеих VM;
- можно ли считать H-07 закрытым;
- что требуется перед merge в `master`.

Merge/push выполняются только отдельным завершающим решением после применения
`superpowers:requesting-code-review`,
`superpowers:verification-before-completion` и
`superpowers:finishing-a-development-branch`.
