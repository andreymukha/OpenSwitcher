# M-04 + M-05: план реализации безопасной записи конфигурации и settings patch

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Исключить частично записанный `config.toml` и перестать затирать несвязанные настройки устаревшим полным D-Bus-снимком.

**Architecture:** `AppConfig` сохраняется same-directory atomic replace с явным commit point на `rename`. Settings UI строит field mask между `loaded` и `draft`; daemon накладывает только отмеченные поля на актуальный config под существующим `settings_update_gate`, валидирует итог и возвращает фактически committed снимок.

**Tech Stack:** Rust 2021, `tempfile`, TOML/serde, zbus/zvariant, GTK4/libadwaita settings UI, Cargo tests, Debian package, две существующие QEMU VM.

---

## Граница плана

План реализует согласованную спецификацию:
`docs/superpowers/specs/2026-08-11-config-settings-transaction-safety-design.md`.

M-04 и M-05 остаются одним slice: settings patch нельзя считать безопасным,
пока его финальный config commit всё ещё способен обрезать файл. Новые input
backend, потоки, polling, тайминги коррекции, clipboard и system lifecycle в
этот план не входят.

## Карта файлов

- Create: `src/config/atomic.rs` — единственная ответственность: атомарно
  заменить один файл и сообщить durability после commit.
- Modify: `src/config.rs` — сериализация `AppConfig` и вызов atomic helper.
- Modify: `Cargo.toml`, `Cargo.lock` — перенести уже используемый `tempfile` в
  production dependencies.
- Modify: `src/model.rs` — field mask, domain/DTO patch и committed snapshot в
  результате обновления.
- Modify: `src/error/mod.rs` — typed validation error неизвестной маски.
- Modify: `src/daemon/runtime.rs` — overlay patch на последний config, no-op,
  persist-before-publish и единый порядок с tray.
- Modify: `src/dbus/mod.rs`, `tests/dbus_api.rs` — новый типизированный D-Bus
  контракт и конкурентные сценарии.
- Modify: `src/settings_ui/state.rs`, `src/settings_ui/dbus_client.rs`,
  `src/settings_ui/presenter.rs` — построение patch и принятие committed снимка.
- Modify: `debian/changelog` — следующий package revision.
- Modify: `docs/audits/2026-07-30-audit-remediation-status.md` — закрытие M-04
  и M-05 только после всех gates и VM smoke.
- Create: `docs/audits/2026-08-11-config-settings-transaction-validation.md` —
  точные результаты, DEB SHA-256 и остаточные риски.

---

### Task 1: Same-directory atomic replace для `config.toml`

**Files:**

- Create: `src/config/atomic.rs`
- Modify: `src/config.rs:1-97`
- Modify: `src/daemon/runtime.rs:115-205` (адаптация caller-ов нового outcome)
- Modify: `src/config.rs:250-740` (tests)
- Modify: `Cargo.toml:20-55`
- Modify: `Cargo.lock`

- [ ] **Step 1: Перенести `tempfile` в production dependencies**

В `Cargo.toml` переместить существующую строку без смены version requirement:

```toml
[dependencies]
tempfile = "3.15"

[dev-dependencies]
```

Затем выполнить:

```bash
cargo check --locked --lib
```

Expected: PASS; `Cargo.lock` либо не меняется, либо содержит только нормализацию
dependency edge без обновления версий.

- [ ] **Step 2: Написать failing-тесты atomic helper**

Добавить `mod atomic;` и `pub use atomic::ConfigCommitOutcome;` в
`src/config.rs`. Создать `src/config/atomic.rs` с
тестовым модулем, который требует следующий контракт:

```rust
#[derive(Debug)]
pub enum ConfigCommitOutcome {
    Durable,
    CommittedDurabilityUncertain(std::io::Error),
}

pub(crate) fn atomic_replace(
    path: &Path,
    content: &[u8],
) -> io::Result<ConfigCommitOutcome>;
```

Минимальные тесты до реализации:

```rust
#[test]
fn atomic_replace_commits_complete_new_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, b"old-complete").unwrap();

    let outcome = atomic_replace(&path, b"new-complete").unwrap();

    assert!(matches!(outcome, ConfigCommitOutcome::Durable));
    assert_eq!(fs::read(&path).unwrap(), b"new-complete");
}

#[test]
fn parent_sync_failure_is_post_commit_not_false_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, b"old").unwrap();

    let outcome = atomic_replace_with_parent_sync(&path, b"new", |_| {
        Err(io::Error::other("injected directory sync failure"))
    })
    .unwrap();

    assert!(matches!(
        outcome,
        ConfigCommitOutcome::CommittedDurabilityUncertain(_)
    ));
    assert_eq!(fs::read(&path).unwrap(), b"new");
}
```

Также добавить Unix-тесты mode `0600` и замены конечного symlink без изменения
его target. Для pre-rename error использовать безопасный rename failure
(конечный путь — непустой каталог): проверить, что каталог не изменён и
созданный рядом временный файл удалён после возврата ошибки.

- [ ] **Step 3: Запустить тест и подтвердить RED**

```bash
cargo test --locked --lib atomic_replace -- --nocapture
```

Expected: FAIL на отсутствующих `atomic_replace` и
`atomic_replace_with_parent_sync`, а не на окружении или линковке.

- [ ] **Step 4: Реализовать минимальный atomic helper**

Реализация должна оставаться локальной одному файлу:

```rust
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

#[derive(Debug)]
pub enum ConfigCommitOutcome {
    Durable,
    CommittedDurabilityUncertain(io::Error),
}

pub(crate) fn atomic_replace(
    path: &Path,
    content: &[u8],
) -> io::Result<ConfigCommitOutcome> {
    atomic_replace_with_parent_sync(path, content, |parent| {
        File::open(parent)?.sync_all()
    })
}

fn config_parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn atomic_replace_with_parent_sync<F>(
    path: &Path,
    content: &[u8],
    sync_parent: F,
) -> io::Result<ConfigCommitOutcome>
where
    F: FnOnce(&Path) -> io::Result<()>,
{
    let parent = config_parent(path);
    let mut temporary = NamedTempFile::new_in(&parent)?;
    temporary.as_file_mut().write_all(content)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;

    match sync_parent(&parent) {
        Ok(()) => Ok(ConfigCommitOutcome::Durable),
        Err(error) => Ok(ConfigCommitOutcome::CommittedDurabilityUncertain(error)),
    }
}
```

Не добавлять backup-файл, journal, общий filesystem trait или retry-loop.
`NamedTempFile` сам удаляет temp на путях до успешного `persist`.

- [ ] **Step 5: Подключить helper к `AppConfig::save_to_path`**

Сигнатура становится:

```rust
pub fn save_to_path(&self, path: &Path) -> Result<ConfigCommitOutcome, ConfigError>
```

Порядок обязателен:

```rust
self.validate()?;
let content = toml::to_string_pretty(&AppConfigFile::from(self))?;
let parent = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
fs::create_dir_all(parent)?;
Ok(atomic::atomic_replace(path, content.as_bytes())?)
```

Все caller-ы должны разобрать outcome явно:

- `load_or_create` и startup auto-detection продолжают работу после
  `CommittedDurabilityUncertain`, но один раз пишут краткое предупреждение без
  TOML и значений настроек;
- `ConfigService::save()` сохраняет прежнюю сигнатуру `Result<(), ConfigError>`
  через явный `map(|_| ())`;
- основной settings commit в Task 3 использует outcome, чтобы отличить обычный
  успех от уже committed, но не подтверждённого directory sync.

Это нужно сделать в Task 1, чтобы каждый промежуточный commit компилировался.

- [ ] **Step 6: Добавить AppConfig regression-тесты**

Добавить проверки:

```rust
#[test]
fn invalid_save_keeps_existing_config_bytes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "known-old-config").unwrap();
    let mut invalid = AppConfig::default();
    invalid.layout.delay_ms = crate::model::LAYOUT_DELAY_MAX_MS + 1;

    assert!(invalid.save_to_path(&path).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "known-old-config");
}
```

У существующих roundtrip-тестов outcome можно игнорировать после `unwrap()`.

- [ ] **Step 7: Проверить Task 1**

```bash
cargo test --locked --lib config -- --nocapture
cargo fmt --check
git diff --check
```

Expected: все config/atomic тесты PASS; прямого `fs::write(path, content)` в
`AppConfig::save_to_path` больше нет.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/config.rs src/config/atomic.rs src/daemon/runtime.rs
git commit -m "fix: save configuration atomically"
```

---

### Task 2: Типизированная field-mask модель settings patch

**Files:**

- Modify: `src/model.rs:957-1050`
- Modify: `src/model.rs:1060-1160` (tests)
- Modify: `src/error/mod.rs:20-60`
- Modify: `src/daemon/runtime.rs:162-181` (новое поле результата)
- Modify: `src/settings_ui/presenter.rs:1270-1280` (test literal)

- [ ] **Step 1: Написать failing model-тесты**

Добавить тесты с точными ожиданиями:

```rust
#[test]
fn settings_patch_between_marks_only_changed_fields() {
    let base = Settings::default();
    let desired = Settings {
        fix_two_capitals: true,
        layout_delay_ms: 77,
        ..base
    };

    let patch = SettingsPatch::between(base, desired);

    assert!(patch.changed().contains(SettingsFieldMask::FIX_TWO_CAPITALS));
    assert!(patch.changed().contains(SettingsFieldMask::LAYOUT_DELAY_MS));
    assert!(!patch.changed().contains(SettingsFieldMask::AUTO_SWITCH_ENABLED));
    assert_eq!(patch.apply_to(base).unwrap(), desired);
}

#[test]
fn unmarked_invalid_dto_value_is_ignored() {
    let current = Settings::default();
    let dto = SettingsPatchDto {
        changed: SettingsFieldMask::empty(),
        values: SettingsDto {
            layout_delay_ms: LAYOUT_DELAY_MAX_MS + 1,
            ..SettingsDto::default()
        },
    };

    let patch = SettingsPatch::try_from(dto).unwrap();
    assert_eq!(patch.apply_to(current).unwrap(), current);
}

#[test]
fn settings_patch_rejects_unknown_mask_bits() {
    let dto = SettingsPatchDto {
        changed: SettingsFieldMask::from_bits(1 << 15),
        values: SettingsDto::default(),
    };
    assert!(matches!(
        SettingsPatch::try_from(dto),
        Err(ValidationError::UnknownSettingsPatchFields { .. })
    ));
}
```

Добавить тест, где patch только одной hotkey после overlay конфликтует с
актуальной второй hotkey: ожидается `DuplicateHotkey` и отсутствие частичного
результата.

- [ ] **Step 2: Подтвердить RED**

```bash
cargo test --locked --lib settings_patch -- --nocapture
```

Expected: FAIL, потому что mask/patch types ещё не существуют.

- [ ] **Step 3: Добавить typed validation error**

В `ValidationError`:

```rust
#[error("Patch настроек содержит неизвестные поля: 0x{unknown:04x}")]
UnknownSettingsPatchFields { unknown: u16 },
```

- [ ] **Step 4: Реализовать mask и patch без revision/CAS**

В `src/model.rs` добавить D-Bus-safe newtype и DTO:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(transparent)]
pub struct SettingsFieldMask(u16);

impl SettingsFieldMask {
    pub const AUTO_SWITCH_ENABLED: Self = Self(1 << 0);
    pub const FIX_TWO_CAPITALS: Self = Self(1 << 1);
    pub const FIX_ACCIDENTAL_CAPS_LOCK: Self = Self(1 << 2);
    pub const LAYOUT_DELAY_MS: Self = Self(1 << 3);
    pub const MANUAL_CORRECTION_HOTKEY: Self = Self(1 << 4);
    pub const SELECTED_TEXT_HOTKEY: Self = Self(1 << 5);
    pub const LAYOUT_SWITCH: Self = Self(1 << 6);
    const ALL_BITS: u16 = (1 << 7) - 1;

    pub const fn empty() -> Self { Self(0) }
    pub const fn all() -> Self { Self(Self::ALL_BITS) }
    pub const fn contains(self, field: Self) -> bool { self.0 & field.0 != 0 }
    pub const fn is_empty(self) -> bool { self.0 == 0 }
    pub const fn bits(self) -> u16 { self.0 }

    pub const fn from_bits(bits: u16) -> Self { Self(bits) }

    fn validate(self) -> Result<Self, ValidationError> {
        let unknown = self.0 & !Self::ALL_BITS;
        (unknown == 0)
            .then_some(self)
            .ok_or(ValidationError::UnknownSettingsPatchFields { unknown })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SettingsPatchDto {
    pub changed: SettingsFieldMask,
    pub values: SettingsDto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsPatch {
    changed: SettingsFieldMask,
    values: Settings,
}
```

Реализовать `between`, `all`, `changed`, `apply_to`, `From<SettingsPatch> for
SettingsPatchDto` и `TryFrom<SettingsPatchDto> for SettingsPatch`. `apply_to`
явно копирует только семь отмеченных полей, затем вызывает `validate()` у
полного результата. Не использовать serde-map, reflection или macro-generated
field access.

- [ ] **Step 5: Возвращать committed settings в результате**

Расширить результат:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct UpdateSettingsResult {
    pub message: String,
    pub restart_required: bool,
    pub settings: SettingsDto,
}
```

На этом шаге текущий полный `ConfigService::update_settings` заполняет поле из
того же validated `Settings`, а presenter test literal — из ожидаемого
committed снимка. Поведение D-Bus пока не менять.

- [ ] **Step 6: Проверить Task 2**

```bash
cargo test --locked --lib settings_patch -- --nocapture
cargo test --locked --lib model::tests -- --nocapture
cargo check --locked --features settings-ui --bins
cargo fmt --check
git diff --check
```

Expected: PASS; patch применяет только mask, полный DTO API пока продолжает
компилироваться до следующего task.

- [ ] **Step 7: Commit**

```bash
git add src/model.rs src/error/mod.rs src/daemon/runtime.rs src/settings_ui/presenter.rs
git commit -m "feat: model field-level settings patches"
```

---

### Task 3: Persist-before-publish и last-write-wins в runtime

**Files:**

- Modify: `src/daemon/runtime.rs:115-240`
- Modify: `src/daemon/runtime.rs:3830-3850`
- Modify: `src/daemon/runtime.rs:4440-4485`
- Modify: `src/daemon/runtime.rs:5270-5390` (tests)

- [ ] **Step 1: Написать failing runtime-тесты**

Добавить четыре regression-теста:

```rust
#[test]
fn unrelated_patch_preserves_latest_tray_toggle() {
    let temp = TempDir::new().unwrap();
    let runtime = test_runtime_with_config_path(temp.path().join("config.toml"));
    assert!(!runtime.toggle_enabled_result().unwrap());

    let mut desired = Settings::default();
    desired.fix_two_capitals = true;
    runtime
        .update_settings_patch(SettingsPatch::between(Settings::default(), desired))
        .unwrap();

    let actual = runtime.get_settings().unwrap();
    assert!(!actual.auto_switch_enabled);
    assert!(actual.fix_two_capitals);
}

#[test]
fn same_field_patch_after_tray_toggle_wins_last() {
    // Toggle true -> false, затем explicit all/field patch false -> true.
    // Expected: true на диске, в ConfigService, IsEnabled и input snapshot.
}

#[test]
fn empty_patch_does_not_write_or_publish_generation() {
    // Сравнить config mtime/bytes и config_generation до/после.
}

#[test]
fn unrelated_patch_preserves_runtime_redetected_layout_switch() {
    // Применить apply_detected_layout_switch_runtime(), затем patch только
    // fix_two_capitals; layout_switch должен остаться detected и попасть в TOML.
}
```

Существующие `failed_settings_save_does_not_publish_new_config_generation` и
`successful_settings_save_publishes_one_complete_generation` оставить и
перевести на patch API после реализации.

- [ ] **Step 2: Подтвердить RED**

```bash
cargo test --locked --lib daemon::runtime::tests -- --nocapture
```

Expected: FAIL на отсутствующем `update_settings_patch` или на stale full-write.

- [ ] **Step 3: Ввести внутренний результат ConfigService**

Добавить рядом с `ConfigService`:

```rust
struct ConfigSettingsUpdate {
    result: UpdateSettingsResult,
    committed_snapshot: Option<RuntimeConfigSnapshot>,
}
```

`None` означает фактический no-op. Реализовать:

```rust
fn update_settings_patch(
    &self,
    patch: SettingsPatch,
) -> Result<ConfigSettingsUpdate, SettingsError>
```

Алгоритм под `inner.write()`:

```rust
let current = config.settings();
let merged = patch.apply_to(current)?;
if merged == current {
    return Ok(ConfigSettingsUpdate {
        result: UpdateSettingsResult {
            message: "Настройки уже актуальны.".to_string(),
            restart_required: false,
            settings: SettingsDto::from(current),
        },
        committed_snapshot: None,
    });
}

let mut updated = config.clone();
updated.apply_settings(merged);
let outcome = updated
    .save_to_path(&self.config_path)
    .map_err(SettingsError::SaveFailed)?;
let snapshot = RuntimeConfigSnapshot::from(&updated);
*config = updated;
```

Только после `save_to_path` обновлять lock-protected config. Для
`CommittedDurabilityUncertain` config уже committed: принять его в память,
вернуть success message с предупреждением и записать bounded debug line без
значений настроек.

- [ ] **Step 4: Публиковать только реальный commit**

Добавить `RuntimeState::update_settings_patch` и
`update_settings_patch_under_gate`. Оба используют существующий
`settings_update_gate`; `publish_committed_config` вызывается только при
`Some(snapshot)`.

Временный legacy wrapper полного `Settings` разрешён внутри Task 3 только через:

```rust
SettingsPatch::all(settings)
```

Он нужен, чтобы ветка компилировалась до перевода D-Bus/UI в Task 4, и удаляется
в том же Task 4.

- [ ] **Step 5: Перевести tray toggle на field patch**

Под уже взятым gate получить актуальный `Settings`, инвертировать только
`auto_switch_enabled` и вызвать `update_settings_patch_under_gate` с mask одного
поля. Не читать полный снимок до взятия gate.

- [ ] **Step 6: Проверить Task 3**

```bash
cargo test --locked --lib daemon::runtime::tests -- --nocapture
cargo test --locked --test dbus_api -- --test-threads=1
cargo fmt --check
git diff --check
```

Expected: runtime tests PASS; старый D-Bus roundtrip временно работает через
full-mask wrapper; no-op не меняет generation.

- [ ] **Step 7: Commit**

```bash
git add src/daemon/runtime.rs
git commit -m "fix: apply settings patches to committed state"
```

---

### Task 4: Перевести D-Bus и Settings UI на patch

**Files:**

- Modify: `src/dbus/mod.rs:250-335`
- Modify: `tests/dbus_api.rs:1-350`
- Modify: `src/settings_ui/state.rs:1-165,360-520`
- Modify: `src/settings_ui/dbus_client.rs:1-50`
- Modify: `src/settings_ui/presenter.rs:1-90,395-455,620-840,1269-1325`
- Modify: `src/daemon/runtime.rs:4440-4475` (удалить legacy full wrapper)

- [ ] **Step 1: Добавить D-Bus RED-тесты двух клиентов**

Создать test helper:

```rust
fn patch(changed: SettingsFieldMask, values: SettingsDto) -> SettingsPatchDto {
    SettingsPatchDto { changed, values }
}
```

Добавить сценарии:

```rust
#[test]
fn stale_clients_merge_different_settings_fields() -> Result<(), Box<dyn Error>> {
    // Оба клиента читают initial.
    // A отправляет mask AUTO_SWITCH_ENABLED=false.
    // B на базе старого initial отправляет только FIX_TWO_CAPITALS=true.
    // GetSettings/TOML обязаны содержать false + true.
    Ok(())
}

#[test]
fn last_patch_wins_for_the_same_field() -> Result<(), Box<dyn Error>> {
    // A: layout_delay_ms=50, B: layout_delay_ms=70.
    // Последний committed result/GetSettings/TOML == 70.
    Ok(())
}

#[test]
fn stale_full_settings_signature_fails_without_writing() -> Result<(), Box<dyn Error>> {
    // Вызвать UpdateSettings со старым SettingsDto body.
    // Ожидать InvalidArgs/signature error и неизменный config.
    Ok(())
}
```

Добавить invalid merged-hotkey test: D-Bus возвращает validation error, TOML,
`IsEnabled` и `GetSettings` не меняются.

- [ ] **Step 2: Добавить UI state RED-тесты**

Проверить:

```rust
#[test]
fn begin_save_builds_patch_only_for_changed_fields() {
    let mut state = DomainState::new();
    state.apply_loaded(Settings::default());
    state.apply_loaded_autostart(false);
    state.update_fix_two_capitals(true);

    let pending = state.begin_save().unwrap();
    assert!(pending.patch.changed().contains(SettingsFieldMask::FIX_TWO_CAPITALS));
    assert!(!pending.patch.changed().contains(SettingsFieldMask::AUTO_SWITCH_ENABLED));
}

#[test]
fn save_success_uses_committed_snapshot_not_stale_draft() {
    // Draft меняет fix_two_capitals, committed result дополнительно содержит
    // auto_switch_enabled=false. После success view показывает оба значения.
}
```

Autostart-only save должен выдавать пустой settings patch, но сохранять
`autostart_change`.

- [ ] **Step 3: Подтвердить RED**

```bash
cargo test --locked --test dbus_api stale_clients -- --nocapture
cargo test --locked --features settings-ui --lib settings_ui::state::tests -- --nocapture
```

Expected: FAIL на старой сигнатуре и отсутствии patch в `PendingSave`.

- [ ] **Step 4: Изменить D-Bus-контракт**

Proxy и API принимают `SettingsPatchDto`:

```rust
fn update_settings(&self, patch: SettingsPatchDto) -> zbus::Result<UpdateSettingsResult>;
```

В API:

```rust
let patch = SettingsPatch::try_from(patch)
    .map_err(|error| fdo::Error::from(DbusError::from(SettingsError::from(error))))?;
let result = self
    .runtime
    .update_settings_patch(patch)
    .map_err(|error| fdo::Error::from(DbusError::from(error)))?;
```

`StatusChanged` по-прежнему испускается только если committed
`auto_switch_enabled` реально отличается от `enabled_before`.

Обновить старые D-Bus tests: roundtrip использует `SettingsPatch::all`/all mask,
а новые tests используют узкие mask. После этого удалить full-settings wrapper
из public runtime API.

- [ ] **Step 5: Изменить DomainState и D-Bus client**

`PendingSave` хранит одновременно validated desired settings и patch:

```rust
pub struct PendingSave {
    pub settings: Settings,
    pub patch: SettingsPatch,
    pub autostart_change: Option<bool>,
}
```

`begin_save` получает `loaded`, строит `desired = settings_for_save()` и
`SettingsPatch::between(loaded, desired)`.

`SettingsClientBackend::save_settings` и `SettingsDbusClient::save_settings`
принимают `SettingsPatch`; реальный клиент вызывает:

```rust
self.proxy()?
    .update_settings(SettingsPatchDto::from(patch))
    .map_err(SettingsClientError::Daemon)
```

- [ ] **Step 6: Принять committed snapshot в presenter**

После успешного D-Bus ответа сначала проверить trust boundary:

```rust
let committed = match Settings::try_from(result.settings) {
    Ok(settings) => settings,
    Err(error) => {
        presenter.with_state(DomainState::save_failed);
        let _ = presenter.emit_view_state();
        let _ = presenter.send_event(PresenterEvent::SaveFailed(error.into()));
        return;
    }
};
```

Изменить методы state:

```rust
pub fn save_succeeded(&mut self, snapshot: PendingSave, committed: Settings)
pub fn save_persisted_settings_succeeded(&mut self, committed: Settings)
```

Оба устанавливают `loaded` и `draft` именно в `committed`. В ветке ошибки
autostart также использовать `committed`, а не `snapshot.settings`. Fake client
должен записывать `Vec<SettingsPatch>`, чтобы тесты проверяли mask.

- [ ] **Step 7: Проверить Task 4**

```bash
cargo test --locked --test dbus_api -- --test-threads=1
cargo test --locked --features settings-ui --lib settings_ui -- --nocapture
cargo check --locked --features settings-ui --bins
cargo fmt --check
git diff --check
```

Expected: все D-Bus/UI tests PASS; `rg 'update_settings\(SettingsDto' src tests`
не находит call site старого blind API.

- [ ] **Step 8: Commit**

```bash
git add src/dbus/mod.rs tests/dbus_api.rs src/settings_ui/state.rs \
  src/settings_ui/dbus_client.rs src/settings_ui/presenter.rs \
  src/daemon/runtime.rs
git commit -m "fix: preserve unrelated concurrent settings"
```

---

### Task 5: Полные gates и канонический DEB

**Files:**

- Modify: `debian/changelog`

- [ ] **Step 1: Добавить package revision `0.1.0-8`**

Новая запись:

```text
open-switcher (0.1.0-8) unstable; urgency=medium

  * Save configuration with a same-directory atomic replacement.
  * Preserve unrelated settings changed by tray or another client.
  * Return the actual committed settings to the settings window.
```

Использовать фактические текущие дату/время и существующего maintainer.

- [ ] **Step 2: Запустить focused и полный Rust gate последовательно**

```bash
cargo test --locked --lib config -- --nocapture
cargo test --locked --lib settings_patch -- --nocapture
cargo test --locked --test dbus_api -- --test-threads=1
cargo test --locked --features settings-ui --lib settings_ui -- --nocapture
cargo test --locked --all-targets --features settings-ui -- --test-threads=1
cargo fmt --check
git diff --check
```

Expected: 0 failed. Не запускать эти Cargo-команды одновременно: они используют
один target и параллельность не сокращает итоговое время.

- [ ] **Step 3: Запустить package shell gates последовательно**

```bash
bash tests/debian_package_scripts_test.sh
bash tests/input_access_package_test.sh
bash tests/manage_package_deb_test.sh
```

Expected: каждая команда печатает `ok`. Не параллелить
`manage_package_deb_test.sh` с другими package tests из-за общих mock artifacts
в родительском каталоге.

- [ ] **Step 4: Commit package metadata**

```bash
git add debian/changelog
git commit -m "chore: package config settings safety"
```

- [ ] **Step 5: Собрать канонический пакет**

```bash
./manage.sh package deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-8_amd64.deb
sha256sum dist/packages/open-switcher_0.1.0-8_amd64.deb
```

Expected: package version `0.1.0-8`, architecture `amd64`, один exact SHA-256.
Сохранить также optional `.ddeb`; не устанавливать пакет на host автоматически.

---

### Task 6: Минимальный package-first smoke в двух VM и закрытие аудита

**Files:**

- Create: `docs/audits/2026-08-11-config-settings-transaction-validation.md`
- Modify: `docs/audits/2026-07-30-audit-remediation-status.md`
- External evidence only: `/home/andrey/VMs/OpenSwitcherLab/runs/`

VM запускаются строго последовательно. Не удалять лабораторию, overlays, base
images, ключи или старые evidence.

- [ ] **Step 1: Запустить Mint/Cinnamon/X11**

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session mint-installed
```

Expected: fixed session с SSH port `22223`.

- [ ] **Step 2: Передать и установить exact DEB в Mint**

```bash
WT=/home/andrey/Projects/OpenSwitcher/.worktrees/config-settings-safety
KEY=/home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519
KNOWN_HOSTS=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts
DEB="$WT/dist/packages/open-switcher_0.1.0-8_amd64.deb"
sha256sum "$DEB"
scp -i "$KEY" -P 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes "$DEB" \
  openswitcher@127.0.0.1:/tmp/open-switcher-m04-m05.deb
ssh -i "$KEY" -p 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes openswitcher@127.0.0.1 \
  'sha256sum /tmp/open-switcher-m04-m05.deb && sudo apt-get install --yes /tmp/open-switcher-m04-m05.deb'
```

Expected: guest SHA совпадает с host; daemon/tray active, `NRestarts=0`.

- [ ] **Step 3: Выполнить Mint settings matrix**

В guest зафиксировать baseline:

```bash
systemctl --user show open-switcher-daemon.service \
  -p MainPID -p NRestarts -p ActiveState -p SubState
gdbus call --session --dest org.oswitch.core --object-path /org/oswitch/core \
  --method org.oswitch.core.GetSettings
stat -c '%a %U %s' ~/.config/open-switcher/config.toml
```

Через установленное окно настроек выполнить один точный сценарий:

1. открыть окно и изменить только `Исправлять две заглавные буквы`, но пока не
   нажимать Save;
2. через tray выключить автопереключение;
3. нажать Save в уже открытом окне;
4. переоткрыть окно.

Expected: автопереключение остаётся выключенным, исправление двух заглавных —
включённым, `config.toml` parseable и mode `600`. Затем явно включить
автопереключение в окне и Save: последнее действие побеждает. Проверить
`GetSettings`, tray и TOML. Обычный F12 smoke выполняется только внутри guest,
без fault injection.

После проверки штатно выключить Mint; overlay сохранить.

- [ ] **Step 4: Повторить exact SHA в Ubuntu/GNOME/Wayland**

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session ubuntu-installed
```

Передать тот же `$DEB` через port `22222`:

```bash
WT=/home/andrey/Projects/OpenSwitcher/.worktrees/config-settings-safety
KEY=/home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519
KNOWN_HOSTS=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts
DEB="$WT/dist/packages/open-switcher_0.1.0-8_amd64.deb"
sha256sum "$DEB"
scp -i "$KEY" -P 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes "$DEB" \
  openswitcher@127.0.0.1:/tmp/open-switcher-m04-m05.deb
ssh -i "$KEY" -p 22222 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes openswitcher@127.0.0.1 \
  'sha256sum /tmp/open-switcher-m04-m05.deb && sudo apt-get install --yes /tmp/open-switcher-m04-m05.deb'
```

Повторить ту же settings matrix и baseline-команды. Expected: одинаковая
field-level семантика, parseable mode-600 TOML, совпадающие result/GetSettings,
daemon `NRestarts=0`. Штатно выключить Ubuntu; overlay сохранить.

- [ ] **Step 5: Написать validation report и обновить статус**

Отчёт обязан содержать:

- commit и DEB SHA-256/размер;
- точные counts Rust/package gates;
- M-04 evidence old-or-new/permissions/symlink/durability outcome;
- M-05 unit, D-Bus, UI и обе VM matrix;
- подтверждение, что host input/clipboard/layout/systemd не менялись;
- ограничения: ручное редактирование TOML во время работы не наблюдается;
- M-04 и M-05 — **закрыто** только если все перечисленные проверки прошли.

В status document обновить общий счётчик и оставить M-06 открытым. Не объявлять
отложенную общую финальную кампанию выполненной.

- [ ] **Step 6: Проверить и commit evidence**

```bash
rg -n "TBD|TODO|FIXME" \
  docs/audits/2026-08-11-config-settings-transaction-validation.md \
  docs/audits/2026-07-30-audit-remediation-status.md
git diff --check
git add -f docs/audits/2026-08-11-config-settings-transaction-validation.md \
  docs/audits/2026-07-30-audit-remediation-status.md
git commit -m "docs: validate config settings transactions"
```

Expected: placeholder search пуст, commit содержит только два audit documents.

---

### Task 7: Финальная проверка ветки и пользовательский DEB checkpoint

**Files:** только проверка уже закоммиченного состояния.

- [ ] **Step 1: Проверить scope и историю**

```bash
git status --short --branch
git log --oneline master..HEAD
git diff --stat master...HEAD
git diff --check master...HEAD
```

Expected: worktree clean; изменения ограничены M-04/M-05, tests, changelog и
документацией.

- [ ] **Step 2: Выполнить независимый self-review**

Проверить по diff:

- ни один pre-rename error не меняет in-memory config;
- post-rename durability warning не выдаётся как rollback;
- unmasked DTO fields никогда не копируются;
- final merged settings валидируются до commit;
- tray и D-Bus проходят один gate;
- no-op не пишет файл и не публикует generation;
- UI принимает returned committed settings;
- input/clipboard/timing code не изменён.

При найденном дефекте: сначала regression test, затем минимальный fix и повтор
релевантных gates. Не расширять scope архитектурными улучшениями.

- [ ] **Step 3: Дать пользователю пакет для проверки**

Команда установки на host выполняется только пользователем:

```bash
sudo apt install \
  /home/andrey/Projects/OpenSwitcher/.worktrees/config-settings-safety/dist/packages/open-switcher_0.1.0-8_amd64.deb
```

Попросить проверить обычное сохранение настроек, tray toggle и базовый F12.
Слияние в `master` выполнять только после явного подтверждения пользователя.
