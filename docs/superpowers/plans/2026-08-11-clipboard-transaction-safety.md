# Безопасная clipboard-транзакция — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Закрыть M-01 и практическую часть M-03, а для M-02 реализовать согласованное поведение: преобразование не блокируется нетекстовым clipboard, а после успеха остаётся преобразованный текст.

**Architecture:** Основной сценарий в `clipboard.rs` будет работать через отдельный `ClipboardTransaction`, который фиксирует намерение до внешней мутации, условно восстанавливает исходный текст и страхует ранние ошибки через `Drop`. Идентичность clipboard предоставит изолированный X11 owner-probe; отсутствие надёжного owner token запрещает восстановительную запись, но не само преобразование.

**Tech Stack:** Rust 2021, `arboard 3.6.1`, `x11rb 0.13.2`, существующий selected-text worker, unit tests с fake clipboard/transport, Debian package и QEMU VM-лаборатория.

---

## Карта файлов

- Create: `src/daemon/selected_text/clipboard_transaction.rs` — снимок, owner/value observation, транзакционный guard, rollback/finalization и unit tests.
- Create: `src/daemon/selected_text/clipboard_owner.rs` — узкая Linux/X11 граница получения текущего `CLIPBOARD` selection owner.
- Modify: `src/daemon/selected_text/mod.rs` — объявления модулей и типизированный `ClipboardDisposition`.
- Modify: `src/daemon/selected_text/clipboard.rs` — orchestration через транзакцию без прямого финального `clear()`.
- Modify: `src/daemon/selected_text/runner.rs` — корректная диагностика разных финальных состояний.
- Modify: `debian/changelog` — версия следующего DEB и пользовательское описание исправления.
- Modify: `docs/audits/2026-07-30-audit-remediation-status.md` — M-01/M-03 и принятый продуктовый статус M-02.
- Create: `docs/audits/2026-08-11-clipboard-transaction-validation.md` — фактические автоматические и VM-доказательства.

## Task 1: Зафиксировать пользовательский контракт типами и первым RED/GREEN

**Files:**

- Create: `src/daemon/selected_text/clipboard_transaction.rs`
- Modify: `src/daemon/selected_text/mod.rs:1-40`
- Modify: `src/daemon/selected_text/clipboard.rs:17-158, 389-673`

- [ ] **Step 1: Перед изменением production-кода добавить failing test для нетекстового исходного clipboard**

Заменить старый тест `clears_clipboard_when_previous_text_was_unavailable` на контракт, который требует выполнить преобразование, не вызывать `clear()` и оставить converted text:

```rust
#[test]
fn unrestorable_clipboard_keeps_converted_text_without_clear() {
    let converter = LayoutConversionEngine;
    let operation = SelectedTextOperation;
    let mut clipboard = TestClipboard::without_readable_text();
    clipboard.queue_read(Ok("Ghbdtn".into()));
    let mut transport = TestTransport::default();

    let result = operation
        .execute(&mut clipboard, &mut transport, &converter)
        .unwrap();

    assert_eq!(
        result,
        SelectedTextSwitchResult::Replaced {
            direction: ConversionDirection::EnToRu,
            clipboard_disposition: ClipboardDisposition::ConvertedTextKept,
        }
    );
    assert_eq!(clipboard.clear_calls, 0);
    assert_eq!(clipboard.current_text.as_deref(), Some("Привет"));
}
```

- [ ] **Step 2: Запустить тест и подтвердить правильный RED**

Run:

```bash
cargo test --locked --lib unrestorable_clipboard_keeps_converted_text_without_clear -- --nocapture
```

Expected: FAIL, потому что текущий код вызывает `clear()` и не имеет `ClipboardDisposition::ConvertedTextKept`.

- [ ] **Step 3: Ввести типизированный результат без изменения остальных путей**

В `mod.rs` добавить:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardDisposition {
    Restored,
    ConvertedTextKept,
    ExternalChangePreserved,
    RestoreFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectedTextSwitchResult {
    Replaced {
        direction: ConversionDirection,
        clipboard_disposition: ClipboardDisposition,
    },
    NoSelectedText,
}
```

Обновить существующие успешные assertions с `clipboard_restored: true` на
`clipboard_disposition: ClipboardDisposition::Restored`.

- [ ] **Step 4: Создать минимальную транзакционную модель и провести orchestration через неё**

В новом `clipboard_transaction.rs` определить устойчивые границы:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ClipboardOwnerToken(pub(super) u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClipboardSnapshot {
    RestorableText(String),
    Unrestorable,
}

pub(super) trait ClipboardAccess {
    fn get_text(&mut self) -> Result<String, SelectedTextError>;
    fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError>;
    fn clear(&mut self) -> Result<(), SelectedTextError>;
    fn owner_token(&mut self) -> Option<ClipboardOwnerToken>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OwnedTextKind {
    Sentinel,
    Selected,
    Converted,
}

pub(super) struct ClipboardTransaction<'a, C: ClipboardAccess> {
    clipboard: &'a mut C,
    original: ClipboardSnapshot,
    expected_text: Option<String>,
    expected_owner: Option<ClipboardOwnerToken>,
    expected_kind: Option<OwnedTextKind>,
    last_meaningful_text: Option<String>,
    finalized: bool,
}
```

Минимальный `finish_success()` для первого GREEN обязан:

```rust
match &self.original {
    ClipboardSnapshot::RestorableText(previous) => {
        if self.clipboard.set_text(previous).is_ok() {
            ClipboardDisposition::Restored
        } else {
            ClipboardDisposition::RestoreFailed
        }
    }
    ClipboardSnapshot::Unrestorable => ClipboardDisposition::ConvertedTextKept,
}
```

На этом шаге сохранить существующий `PASTE_SETTLE_TIMEOUT = 300 ms`; условная
owner-проверка появится в Task 3.

- [ ] **Step 5: Повторить selected-text tests и подтвердить GREEN**

Run:

```bash
cargo test --locked --lib selected_text::clipboard -- --nocapture
```

Expected: все selected-text clipboard tests PASS; старый сценарий очистки заменён новым контрактом.

- [ ] **Step 6: Зафиксировать первый работающий slice**

```bash
git add src/daemon/selected_text/mod.rs \
        src/daemon/selected_text/clipboard.rs \
        src/daemon/selected_text/clipboard_transaction.rs
git commit -m "fix: keep converted text for unrestorable clipboard"
```

## Task 2: Rollback guard для ранних ошибок и panic

**Files:**

- Modify: `src/daemon/selected_text/clipboard_transaction.rs`
- Modify: `src/daemon/selected_text/clipboard.rs`

- [ ] **Step 1: Расширить fake boundaries точечными отказами**

Добавить в test-only fake счётчики и инъекции:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailCall {
    Read(usize),
    Write(usize),
    Clear(usize),
    Copy,
    Paste,
}

struct TestClipboard {
    current_text: Option<String>,
    owner: Option<ClipboardOwnerToken>,
    reads: usize,
    writes: usize,
    clears: usize,
    fail_call: Option<FailCall>,
    panic_call: Option<FailCall>,
    pending_reads: VecDeque<Result<String, SelectedTextError>>,
    trace: Vec<&'static str>,
}
```

`set_text()` сначала увеличивает `writes` и применяет тестовый режим
«ошибка до мутации» либо «ошибка после мутации». Это различие необходимо для
проверки намерения, записанного до внешнего вызова.

- [ ] **Step 2: Добавить failing error-path tests**

```rust
#[test]
fn copy_failure_restores_previous_text() {
    let mut clipboard = TestClipboard::with_current_text("previous");
    let mut transport = TestTransport::failing_copy();

    let error = SelectedTextOperation
        .execute(&mut clipboard, &mut transport, &LayoutConversionEngine)
        .unwrap_err();

    assert!(matches!(error, SwitcherError::InputSafety(_) | SwitcherError::Io(_)));
    assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
}

#[test]
fn paste_failure_restores_previous_text() {
    let mut clipboard = TestClipboard::with_current_text("previous");
    clipboard.queue_read(Ok("Ghbdtn".into()));
    let mut transport = TestTransport::failing_paste();

    assert!(SelectedTextOperation
        .execute(&mut clipboard, &mut transport, &LayoutConversionEngine)
        .is_err());
    assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
}
```

- [ ] **Step 3: Запустить error-path tests и подтвердить RED**

Run:

```bash
cargo test --locked --lib 'copy_failure_restores_previous_text' -- --nocapture
cargo test --locked --lib 'paste_failure_restores_previous_text' -- --nocapture
```

Expected: FAIL; текущие `?` выходят без общего rollback.

- [ ] **Step 4: Записать mutation intent до каждого внешнего write**

Метод транзакции должен иметь следующий порядок:

```rust
pub(super) fn write_operation_text(
    &mut self,
    kind: OwnedTextKind,
    value: &str,
) -> Result<(), SelectedTextError> {
    self.expected_text = Some(value.to_owned());
    self.expected_owner = None;
    self.expected_kind = Some(kind);
    if kind != OwnedTextKind::Sentinel {
        self.last_meaningful_text = Some(value.to_owned());
    }

    self.clipboard.set_text(value)?;
    self.expected_owner = self.clipboard.owner_token();
    Ok(())
}
```

После принятия copy-кандидата вызывать `adopt_selected_text(text, owner)`, чтобы
Drop знал последний осмысленный fallback и наблюдавшегося владельца.

- [ ] **Step 5: Реализовать непаникующий `Drop` и explicit finalizer**

```rust
impl<C: ClipboardAccess> Drop for ClipboardTransaction<'_, C> {
    fn drop(&mut self) {
        if self.finalized {
            return;
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let outcome = self.rollback_if_still_owned();
            self.log_cleanup_outcome(outcome);
        }));
    }
}
```

`rollback_if_still_owned()` возвращает внутренний typed outcome. Для
`RestorableText` он пытается вернуть previous; для `Unrestorable` очищает
только собственный sentinel, а после selected/converted оставляет последний
осмысленный текст. Ошибка cleanup логируется отдельно и не возвращается вместо
primary error.

- [ ] **Step 6: Добавить и пройти panic/secondary-error tests**

```rust
#[test]
fn copy_panic_runs_drop_rollback() {
    let mut clipboard = TestClipboard::with_current_text("previous");
    let mut transport = TestTransport::panicking_copy();

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = SelectedTextOperation
            .execute(&mut clipboard, &mut transport, &LayoutConversionEngine);
    }));

    assert!(unwind.is_err());
    assert_eq!(clipboard.current_text.as_deref(), Some("previous"));
}

#[test]
fn rollback_failure_preserves_primary_copy_error() {
    let mut clipboard = TestClipboard::with_failing_restore("previous");
    let mut transport = TestTransport::failing_copy();

    let error = SelectedTextOperation
        .execute(&mut clipboard, &mut transport, &LayoutConversionEngine)
        .unwrap_err();

    assert_eq!(error.to_string(), transport.expected_copy_error_text());
    assert!(clipboard.restore_was_attempted());
}
```

Run:

```bash
cargo test --locked --lib selected_text::clipboard -- --nocapture
cargo test --locked --lib selected_text::clipboard_transaction -- --nocapture
```

Expected: PASS; panic перехватывается тестом, previous восстанавливается, primary error не меняется.

- [ ] **Step 7: Зафиксировать rollback slice**

```bash
git add src/daemon/selected_text/clipboard.rs \
        src/daemon/selected_text/clipboard_transaction.rs
git commit -m "fix: roll back clipboard on selected text failures"
```

## Task 3: Owner-aware защита конкурентных изменений

**Files:**

- Create: `src/daemon/selected_text/clipboard_owner.rs`
- Modify: `src/daemon/selected_text/mod.rs`
- Modify: `src/daemon/selected_text/clipboard_transaction.rs`
- Modify: `src/daemon/selected_text/clipboard.rs`

- [ ] **Step 1: Добавить failing concurrency tests до owner-aware production-кода**

```rust
#[test]
fn foreign_text_wins_before_restore() {
    let mut fixture = OperationFixture::with_previous_text("previous");
    fixture.replace_before_restore("foreign", ClipboardOwnerToken(22));

    let result = fixture.execute_successfully();

    assert_eq!(result.clipboard_disposition(), ClipboardDisposition::ExternalChangePreserved);
    assert_eq!(fixture.clipboard.current_text.as_deref(), Some("foreign"));
}

#[test]
fn same_text_from_different_owner_wins_before_restore() {
    let mut fixture = OperationFixture::with_previous_text("previous");
    fixture.replace_before_restore("Привет", ClipboardOwnerToken(22));

    let result = fixture.execute_successfully();

    assert_eq!(result.clipboard_disposition(), ClipboardDisposition::ExternalChangePreserved);
    assert_eq!(fixture.clipboard.current_text.as_deref(), Some("Привет"));
}

#[test]
fn owner_change_during_observation_is_unknown_and_skips_restore() {
    let mut fixture = OperationFixture::with_previous_text("previous");
    fixture.owner_sequence([ClipboardOwnerToken(11), ClipboardOwnerToken(22)]);

    let result = fixture.execute_successfully();

    assert_ne!(result.clipboard_disposition(), ClipboardDisposition::Restored);
    assert_eq!(fixture.clipboard.current_text.as_deref(), Some("Привет"));
}
```

- [ ] **Step 2: Запустить concurrency filter и подтвердить RED**

Run:

```bash
cargo test --locked --lib 'wins_before_restore' -- --nocapture
cargo test --locked --lib owner_change_during_observation_is_unknown_and_skips_restore -- --nocapture
```

Expected: FAIL; минимальный Task 1 finalizer пока восстанавливает previous без проверки owner/value.

- [ ] **Step 3: Реализовать согласованное observation owner/value/owner**

В `clipboard_transaction.rs` добавить:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClipboardObservation {
    Stable {
        owner: ClipboardOwnerToken,
        text: Option<String>,
    },
    Uncertain,
}

fn observe_coherently<C: ClipboardAccess>(clipboard: &mut C) -> ClipboardObservation {
    let before = clipboard.owner_token();
    let text = clipboard.get_text().ok();
    let after = clipboard.owner_token();

    match (before, after) {
        (Some(before), Some(after)) if before == after => {
            ClipboardObservation::Stable { owner: before, text }
        }
        _ => ClipboardObservation::Uncertain,
    }
}
```

Условная запись разрешена только при `Stable`, точном совпадении expected text и
expected owner. Для уникального sentinel после неоднозначно завершившегося
write допустима cleanup-проверка по точному sentinel, но не для converted text.

- [ ] **Step 4: Создать реальный X11/XWayland owner probe**

`clipboard_owner.rs` должен содержать только платформенную механику:

```rust
use super::clipboard_transaction::ClipboardOwnerToken;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

pub(super) struct X11ClipboardOwnerProbe {
    connection: RustConnection,
    clipboard_atom: Atom,
}

impl X11ClipboardOwnerProbe {
    pub(super) fn try_new() -> Option<Self> {
        let (connection, _) = RustConnection::connect(None).ok()?;
        let clipboard_atom = connection
            .intern_atom(false, b"CLIPBOARD")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some(Self { connection, clipboard_atom })
    }

    pub(super) fn current_owner(&self) -> Option<ClipboardOwnerToken> {
        self.connection
            .get_selection_owner(self.clipboard_atom)
            .ok()?
            .reply()
            .ok()
            .map(|reply| ClipboardOwnerToken(reply.owner))
    }
}
```

`SystemClipboard` хранит `Option<X11ClipboardOwnerProbe>`. Ошибка probe не
мешает создать `arboard::Clipboard`; она только переводит restore в безопасный
skip. В логе фиксируется отсутствие proof без содержимого clipboard.

- [ ] **Step 5: Обновить copy polling, чтобы принимать coherent observation**

`CopyOutcome::SelectedText` должен нести текст и owner token:

```rust
enum CopyOutcome {
    SelectedText {
        text: String,
        owner: Option<ClipboardOwnerToken>,
    },
    TimedOut,
}
```

Существующие `COPY_TIMEOUT`, `COPY_CHANGE_STABLE_FOR`,
`COPY_MIN_ACCEPT_DELAY`, `SENTINEL_CONFIRM_TIMEOUT` и
`PASTE_SETTLE_TIMEOUT` остаются без изменений.

- [ ] **Step 6: Пройти owner/concurrency tests и весь selected-text slice**

Run:

```bash
cargo test --locked --lib selected_text::clipboard_transaction -- --nocapture
cargo test --locked --lib selected_text::clipboard -- --nocapture
```

Expected: PASS; foreign owner/value никогда не вызывает restore-запись в fake trace.

- [ ] **Step 7: Зафиксировать concurrency slice**

```bash
git add src/daemon/selected_text/mod.rs \
        src/daemon/selected_text/clipboard.rs \
        src/daemon/selected_text/clipboard_transaction.rs \
        src/daemon/selected_text/clipboard_owner.rs
git commit -m "fix: preserve concurrent clipboard updates"
```

## Task 4: Диагностика, полная failure matrix и регрессии

**Files:**

- Modify: `src/daemon/selected_text/runner.rs:183-206`
- Modify: `src/daemon/selected_text/clipboard.rs`
- Modify: `src/daemon/selected_text/clipboard_transaction.rs`

- [ ] **Step 1: Добавить failing test для пользовательского предупреждения**

Вынести чистую классификацию:

```rust
fn clipboard_warning(disposition: ClipboardDisposition) -> Option<&'static str>;
```

Тест:

```rust
#[test]
fn runner_warns_only_for_restore_failed() {
    assert!(clipboard_warning(ClipboardDisposition::RestoreFailed).is_some());
    assert_eq!(clipboard_warning(ClipboardDisposition::Restored), None);
    assert_eq!(clipboard_warning(ClipboardDisposition::ConvertedTextKept), None);
    assert_eq!(clipboard_warning(ClipboardDisposition::ExternalChangePreserved), None);
}
```

Run:

```bash
cargo test --locked --lib runner_warns_only_for_restore_failed -- --nocapture
```

Expected: FAIL, helper ещё отсутствует, а runner знает только старый boolean.

- [ ] **Step 2: Реализовать точные diagnostics**

```rust
fn clipboard_warning(disposition: ClipboardDisposition) -> Option<&'static str> {
    match disposition {
        ClipboardDisposition::RestoreFailed => {
            Some("Не удалось восстановить предыдущее содержимое буфера обмена.")
        }
        ClipboardDisposition::Restored
        | ClipboardDisposition::ConvertedTextKept
        | ClipboardDisposition::ExternalChangePreserved => None,
    }
}
```

Debug-line содержит только `clipboard_disposition={disposition:?}` и уже
существующие обезличенные summaries. Содержимое, owner window title и пути не
логируются.

- [ ] **Step 3: Добавить table-driven failure-at-every-step test**

Матрица содержит точные точки:

```rust
const FAILURE_STEPS: &[FailureStep] = &[
    FailureStep::SnapshotRead,
    FailureStep::SentinelWriteBeforeMutation,
    FailureStep::SentinelWriteAfterMutation,
    FailureStep::Copy,
    FailureStep::CopiedTextRead,
    FailureStep::ConvertedWriteBeforeMutation,
    FailureStep::ConvertedWriteAfterMutation,
    FailureStep::Paste,
    FailureStep::RestoreWrite,
    FailureStep::SentinelClear,
];
```

Для каждой точки assertions требуют:

```rust
assert!(!fixture.clipboard.contains_internal_sentinel());
assert!(!fixture.overwrote_foreign_content());
assert_eq!(fixture.primary_error_kind(), step.expected_primary_error());
assert!(fixture.total_clipboard_calls() <= step.maximum_calls());
```

Snapshot read с `ContentNotAvailable` является продуктовым fallback, а не
ошибкой: операция доходит до paste и оставляет converted text.

- [ ] **Step 4: Запустить matrix и исправить только обнаруженные пробелы**

Run:

```bash
cargo test --locked --lib selected_text::clipboard -- --nocapture
cargo test --locked --lib selected_text::clipboard_transaction -- --nocapture
cargo test --locked --lib selected_text::runner -- --nocapture
```

Expected: PASS без обращения к host clipboard/X11.

- [ ] **Step 5: Проверить неизменность таймингов отдельными assertions**

Сохранить существующий тест 300 мс и добавить:

```rust
#[test]
fn clipboard_transaction_keeps_existing_timing_bounds() {
    assert_eq!(COPY_POLL_INTERVAL, Duration::from_millis(10));
    assert_eq!(COPY_TIMEOUT, Duration::from_millis(900));
    assert_eq!(COPY_CHANGE_STABLE_FOR, Duration::from_millis(60));
    assert_eq!(COPY_MIN_ACCEPT_DELAY, Duration::from_millis(120));
    assert_eq!(SENTINEL_CONFIRM_TIMEOUT, Duration::from_millis(120));
    assert_eq!(PASTE_SETTLE_TIMEOUT, Duration::from_millis(300));
}
```

- [ ] **Step 6: Зафиксировать диагностический и regression slice**

```bash
git add src/daemon/selected_text/runner.rs \
        src/daemon/selected_text/clipboard.rs \
        src/daemon/selected_text/clipboard_transaction.rs
git commit -m "test: close clipboard transaction failure paths"
```

## Task 5: Package, аудит и доказательства

**Files:**

- Modify: `debian/changelog`
- Modify: `docs/audits/2026-07-30-audit-remediation-status.md`
- Create: `docs/audits/2026-08-11-clipboard-transaction-validation.md`

- [ ] **Step 1: Обновить Debian changelog до `0.1.0-7`**

Добавить запись:

```text
open-switcher (0.1.0-7) stable; urgency=medium

  * Restore text clipboard on selected-text failures and panic unwinds.
  * Preserve concurrent clipboard updates instead of blindly restoring stale data.
  * Keep converted text when the previous clipboard format is not restorable.

 -- Andrey <andrei.m@dot818.com>  Tue, 11 Aug 2026 00:00:00 +0300
```

Перед commit заменить время на фактическое значение `date -R`, сохранив дату
2026-08-11 и часовой пояс `+0300`.

- [ ] **Step 2: Выполнить быстрые gates**

Run:

```bash
cargo fmt --check
cargo test --locked --lib selected_text -- --nocapture
git diff --check
bash tests/debian_package_scripts_test.sh
bash tests/input_access_package_test.sh
bash tests/manage_package_deb_test.sh
```

Expected: все команды exit 0. Если sandbox блокирует Unix sockets или mock
artifact parent, повторить неизменённую команду вне sandbox и записать причину.

- [ ] **Step 3: Выполнить полный последовательный gate**

Run outside restricted sandbox:

```bash
cargo test --locked --all-targets -- --test-threads=1
```

Expected: минимум исходные 957 тестов плюс новые clipboard tests, 0 failed,
ровно существующий ignored release-only guardian benchmark.

- [ ] **Step 4: Собрать канонический DEB и зафиксировать receipt**

Run:

```bash
./manage.sh package deb
dpkg-deb -f dist/packages/open-switcher_0.1.0-7_amd64.deb Package Version Architecture
sha256sum dist/packages/open-switcher_0.1.0-7_amd64.deb
```

Expected: package `open-switcher`, version `0.1.0-7`, architecture `amd64`,
успешный SHA-256 receipt.

- [ ] **Step 5: Выполнить минимальный package-first VM smoke в двух профилях**

Использовать сохранённые sessions `mint-installed` и `ubuntu-installed`, не
удаляя overlays/evidence. В каждой гостевой системе установить exact DEB и
сохранить:

```text
profile/session type
installed dpkg version
guest-side DEB SHA-256
previous text -> selected-text conversion -> previous text restored
non-text/unreadable snapshot -> conversion succeeds -> converted text remains
foreign unique text written before restore -> foreign text remains
NoSelectedText -> internal sentinel absent
daemon PID and NRestarts before/after
```

Mint должен подтвердить Cinnamon/X11, Ubuntu — GNOME/Wayland. Все clipboard и
input действия выполняются только внутри гостевых систем. После проверки обе
VM штатно выключить, лабораторию и evidence сохранить.

- [ ] **Step 6: Обновить аудит без завышения статуса**

В статусном документе зафиксировать:

```text
M-01 — закрыто: rollback guard + failure/panic matrix + VM smoke.
M-02 — принятое продуктовое поведение: non-text не блокирует операцию и может
       быть заменён converted text; произвольные MIME не архивируются.
M-03 — закрыто в практической границе owner/value checks; атомарный clipboard
       CAS отсутствует, протокольное микроокно задокументировано.
```

Сводка после успешной проверки: 17 закрыто, 1 принято, 3 открыто; открыты только
M-04, M-05 и M-06.

- [ ] **Step 7: Написать validation report только по фактическим результатам**

Документ `docs/audits/2026-08-11-clipboard-transaction-validation.md` должен
содержать commit, DEB SHA-256, точные test counts, VM receipts, ограничения и
оставшиеся риски. Не записывать ожидаемый результат как уже полученный.

- [ ] **Step 8: Финальная самопроверка и commit evidence**

Run:

```bash
git diff --check
git status --short --branch
git diff --stat master...HEAD
```

Затем:

```bash
git add debian/changelog \
        docs/audits/2026-07-30-audit-remediation-status.md \
        docs/audits/2026-08-11-clipboard-transaction-validation.md
git commit -m "docs: validate clipboard transaction safety"
```

Expected: ветка содержит только согласованный clipboard slice и его
доказательства; пользовательские изменения основной рабочей копии не затронуты.
