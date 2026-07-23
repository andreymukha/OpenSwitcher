# Deferred Replay ACK и X11 Focus Barrier — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Исключить потерю текстового хвоста при редком пересечении асинхронной
F12-коррекции с `Alt+Tab` в X11: deferred-событие считается доставленным только
после фактической записи writer, а текст после завершённого focus shortcut
ждёт подтверждённой смены `_NET_ACTIVE_WINDOW`.

**Architecture:** Deferred replay использует новый вид существующей
`WriterTransaction`: один key event, ACK после успешных `uinput write +
synchronize`, общий deadline 1 с и уже имеющийся fail-stop при неоднозначном
исходе. X11 watcher сохраняет монотонное поколение active target независимо от
одноразового invalidation flag. Session отмечает полные `Alt/Meta+Tab`
последовательности по sequence id и после подтверждённого финального release
приостанавливает только следующий deferred-хвост до смены поколения либо
fail-open deadline 300 мс.

**Tech Stack:** Rust 2021/1.95, `evdev`, `uinput 0.1.3`, X11/XCB
`_NET_ACTIVE_WINDOW`, `std::sync`, встроенный Rust test harness, Debian
packaging, сохранённые Mint Cinnamon X11 и Ubuntu GNOME Wayland VM-профили.

---

## Зафиксированные границы

- Утверждённая спецификация:
  `docs/superpowers/specs/2026-07-23-deferred-input-conservation-design.md`,
  раздел «Подтверждение deferred-событий и X11 focus barrier».
- Исходный runtime-дефект подтверждён только в Mint Cinnamon X11:
  `F12`, почти немедленный полный `Alt+Tab`, затем цифры; цифры пропадают,
  хотя ledger сообщает `accepted=acknowledged`.
- Обычный ввод, обычный `Alt+Tab`, F12 без пересечения, pointer-click policy,
  Wayland-путь и подобранные `layout_delay_ms`, `typing_ms`,
  `backspace_ms` не меняются.
- Host не открывает `/dev/input` или `/dev/uinput`, не посылает нажатия и не
  меняет clipboard, layout, systemd, udev или ACL. Runtime-ввод и fault
  injection допустимы только внутри VM.
- VM-профили запускаются строго последовательно. Лаборатория не удаляется без
  прямой просьбы пользователя.

## Карта файлов

- Modify: `src/daemon/keyboard.rs`
  - acknowledged deferred-forward transaction;
  - X11 active-target generation;
  - controller API для ACK и чтения generation;
  - unit tests writer/watcher.
- Modify: `src/daemon/service.rs`
  - чистое состояние X11 focus markers/barrier;
  - origin-aware forwarding для deferred replay;
  - bounded event-loop wait;
  - regression tests порядка, нескольких shortcut и deadline.
- Modify: `debian/changelog`
  - новая Debian revision `0.1.0-3`, чтобы пакет с новым кодом однозначно
    обновлял установленный `0.1.0-2`.
- Create:
  `docs/audits/2026-07-23-deferred-input-conservation-validation.md`
  - локальные и VM-результаты, hashes и остаточные ограничения.
- Generated, do not commit: `dist/packages/open-switcher_0.1.0-3_amd64.deb`.

`src/error/mod.rs` не меняется, если существующие typed transaction errors
полностью покрывают enqueue failure, disconnect и 1-second timeout.

### Task 1: ACK deferred event после writer mutation

**Files:**

- Modify: `src/daemon/keyboard.rs:35-40`
- Modify: `src/daemon/keyboard.rs:243-286`
- Modify: `src/daemon/keyboard.rs:838-851`
- Modify: `src/daemon/keyboard.rs:1460-1467`
- Modify: `src/daemon/keyboard.rs:2665-2771`
- Modify: `src/daemon/keyboard.rs:5226-5288`
- Test: `src/daemon/keyboard.rs:8350-8465`

- [ ] **Step 1: Написать RED-тест, что deferred forward ждёт reply writer**

Добавить test helper:

```rust
fn deferred_forward_transaction(key: Key, value: i32) -> WriterTransactionKind {
    WriterTransactionKind::ForwardDeferredEvent { key, value }
}
```

Добавить тест
`deferred_forward_returns_only_after_writer_completion_is_published`:

1. Создать `test_writer_handle(WRITER_QUEUE_CAPACITY, true)`.
2. Запустить `handle.forward_deferred_event(KEY_1, 1)` в отдельном потоке.
3. Получить `WriterCommand::Transaction`, но не публиковать reply.
4. Проверить через отдельный bounded channel, что caller ещё не завершился.
5. Опубликовать:

   ```rust
   CorrectionExecutionOutcome {
       layout_switch: CorrectionLayoutSwitchOutcome::NotNeeded,
   }
   ```

6. Проверить `Ok(())`.

Run:

```bash
cargo test --locked --lib deferred_forward_returns_only_after_writer_completion_is_published
```

Expected RED: метод/variant ещё отсутствует либо caller пока использует
обычный FIFO fast path.

- [ ] **Step 2: Написать RED-тест 1-second fail-stop semantics**

Добавить
`deferred_forward_timeout_retains_transaction_failure_and_blocks_later_mutations`.
В тесте использовать короткий test-only timeout через внутренний
`forward_deferred_event_with_timeout`, удержать reply и проверить:

```rust
assert!(matches!(
    error,
    SwitcherError::VirtualKeyboardWriterTransactionTimedOut { .. }
));
assert!(handle.transaction_failure_request_id().is_some());
assert!(!handle.is_alive());
assert!(handle.forward_event(Key::KEY_2, 1).is_err());
```

Run:

```bash
cargo test --locked --lib deferred_forward_timeout_retains_transaction_failure_and_blocks_later_mutations
```

Expected RED: отдельного deferred transaction API ещё нет.

- [ ] **Step 3: Добавить acknowledged transaction без второго timeout-протокола**

Ввести константу:

```rust
const DEFERRED_FORWARD_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(1);
```

и добавить в существующий `WriterTransactionKind` variant:

```rust
ForwardDeferredEvent { key: Key, value: i32 },
```

В `WriterTransactionKind::execution_timeout()` вернуть для этого variant
`Ok(DEFERRED_FORWARD_TRANSACTION_TIMEOUT)`.

В `VirtualKeyboardHandle` добавить:

```rust
fn forward_deferred_event(&self, key: Key, value: i32) -> Result<(), SwitcherError> {
    self.run_transaction(WriterTransactionKind::ForwardDeferredEvent { key, value })
        .map(|_| ())
}
```

Test-only `forward_deferred_event_with_timeout` обязан вызывать тот же
`run_transaction_with_timeout`; отдельные atomics, reply channel и deadline не
создавать.

В `KeyboardController` добавить одноимённый `pub(crate)` метод. Обычный
`forward_event()` оставить без изменений.

- [ ] **Step 4: Выполнить mutation и publish в правильном порядке**

В writer dispatch для `ForwardDeferredEvent` выполнить:

```rust
control.authorize_mutation_start()?;
device.write(INPUT_EVENT_KEYBOARD, key.code() as i32, value)?;
device.synchronize()?;
Ok(CorrectionExecutionOutcome {
    layout_switch: CorrectionLayoutSwitchOutcome::NotNeeded,
})
```

Существующий внешний порядок должен остаться:

```text
authorize -> write -> synchronize -> ensure_active -> publish reply
```

Если timeout победил после system call, transaction остаётся fail-stop;
неоднозначное событие остаётся в голове ledger и не воспроизводится через новое
поколение backend.

- [ ] **Step 5: Добавить unit seam для порядка write/synchronize**

Вынести только тело одиночной мутации в небольшой helper с test closures либо
test sink. Добавить
`deferred_forward_publishes_success_only_after_write_and_synchronize`:

- trace до reply равен `["write", "synchronize"]`;
- ошибка write не вызывает synchronize;
- ошибка synchronize не публикуется как success.

Run:

```bash
cargo test --locked --lib deferred_forward -- --nocapture
cargo fmt --all -- --check
git diff --check
```

Expected GREEN: все `deferred_forward` tests PASS; обычные writer transaction
tests также остаются зелёными.

- [ ] **Step 6: Зафиксировать Task 1**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: acknowledge deferred input writer mutations"
```

### Task 2: Независимое поколение X11 active target

**Files:**

- Modify: `src/daemon/keyboard.rs:932-1005`
- Modify: `src/daemon/keyboard.rs:1350-1420`
- Modify: `src/daemon/keyboard.rs:1951-2071`
- Modify: `src/daemon/keyboard.rs:9450-9473`
- Modify: все test constructors `InputTargetWatcher` около
  `src/daemon/keyboard.rs:9485-9750`

- [ ] **Step 1: Написать RED-тест постоянного generation**

Расширить текущий
`x11_context_events_set_only_the_matching_invalidation_flag` либо добавить
`x11_active_window_change_advances_generation_independently_of_flag_drain`.

Контракт теста:

```rust
assert_eq!(generation.load(Ordering::SeqCst), 0);
publish_x11_context_event(
    X11ContextEvent::ActiveWindowChanged {
        previous: Some(1),
        current: Some(2),
    },
    &changed,
    &pointer_click,
    &generation,
);
assert_eq!(generation.load(Ordering::SeqCst), 1);
assert!(changed.swap(false, Ordering::SeqCst));
assert_eq!(generation.load(Ordering::SeqCst), 1);
publish_x11_context_event(
    X11ContextEvent::ActiveWindowChanged {
        previous: Some(2),
        current: Some(3),
    },
    &changed,
    &pointer_click,
    &generation,
);
assert_eq!(generation.load(Ordering::SeqCst), 2);
```

Pointer click не меняет generation.

Run:

```bash
cargo test --locked --lib x11_active_window_change_advances_generation_independently_of_flag_drain
```

Expected RED: watcher хранит только одноразовый `changed_flag`.

- [ ] **Step 2: Добавить `target_generation: Arc<AtomicU64>`**

Добавить поле в `InputTargetWatcher`, инициализировать нулём в `spawn()` и
`disabled()`, передать в worker рядом с `changed_flag`.

На `X11ContextEvent::ActiveWindowChanged` выполнить:

```rust
changed_flag.store(true, Ordering::SeqCst);
target_generation.fetch_add(1, Ordering::SeqCst);
```

Generation повышается для каждого подтверждённого изменения
`_NET_ACTIVE_WINDOW`; чтение и очистка boolean-флага его не сбрасывают.

Добавить:

```rust
fn target_generation(&self) -> u64
pub(crate) fn input_target_generation(&self) -> u64
```

Второй метод делегирует из `KeyboardController` в watcher.

- [ ] **Step 3: Обновить все constructors и проверить watcher**

Run:

```bash
cargo test --locked --lib x11_context -- --nocapture
cargo test --locked --lib input_target -- --nocapture
cargo fmt --all -- --check
git diff --check
```

Expected GREEN: generation сохраняется после `take_change_invalidation()`;
pointer-click semantics и остановка watcher не изменились.

- [ ] **Step 4: Зафиксировать Task 2**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: retain X11 input target generation"
```

### Task 3: Чистое состояние shortcut markers и bounded barrier

**Files:**

- Modify: `src/daemon/service.rs:35-39`
- Modify: `src/daemon/service.rs:90-155`
- Modify: `src/daemon/service.rs:441-466`
- Modify: `src/daemon/service.rs:631-651`
- Test: `src/daemon/service.rs:5333-5535`

- [ ] **Step 1: Написать RED-тесты state machine**

Добавить чистые тесты:

```text
x11_full_alt_tab_records_marker_after_final_release
x11_incomplete_alt_tab_does_not_record_marker
x11_two_complete_shortcuts_preserve_marker_order
x11_barrier_opens_immediately_after_generation_change
x11_barrier_times_out_open_without_dropping_tail
x11_barrier_allows_leading_releases_without_opening
wayland_deferred_shortcut_does_not_create_x11_barrier
```

Первый тест подаёт:

```text
Alt down(seq=1) -> Tab down(2) -> Tab up(3) -> Alt up(4)
```

и ожидает marker:

```rust
DeferredFocusBarrierMarker {
    after_sequence_id: 4,
    target_generation: generation_before_tab,
}
```

Incomplete-варианты обязаны покрыть отсутствие `Tab up` и отсутствие
финального modifier release. Два полных shortcut не имеют права заменять друг
друга.

Run:

```bash
cargo test --locked --lib x11_full_alt_tab_records_marker_after_final_release
```

Expected RED: state machine ещё нет.

- [ ] **Step 2: Реализовать чистые типы barrier**

Добавить:

```rust
const X11_DEFERRED_FOCUS_BARRIER_TIMEOUT: Duration = Duration::from_millis(300);
const X11_DEFERRED_FOCUS_BARRIER_POLL: Duration = Duration::from_millis(5);

struct DeferredFocusBarrierMarker {
    after_sequence_id: u64,
    target_generation: u64,
    deadline: Option<Instant>,
}

#[derive(Default)]
struct DeferredFocusBarrierState {
    active_shortcut: Option<DeferredFocusShortcut>,
    markers: VecDeque<DeferredFocusBarrierMarker>,
}
```

`DeferredFocusShortcut` хранит generation, факт `Tab down` и состояние,
достаточное для подтверждения обоих release. Marker создаётся только когда:

- `Tab` был нажат с ровно одним focus modifier family (`Alt` либо `Meta`) и
  без `Ctrl`;
- `Tab` уже отпущен;
- участвующие `Alt/Meta` больше не нажаты.

Текущий `is_wayland_focus_switch_shortcut()` переименовать в нейтральный
`is_focus_switch_shortcut()`. Wayland-specific wrapper
`should_invalidate_for_wayland_focus_switch_shortcut()` остаётся
Wayland-only; X11 state machine использует только нейтральный classifier.

Baseline generation снимается на `Tab down`. Финальный sequence id принадлежит
последнему release, который завершил chord.

- [ ] **Step 3: Реализовать решения до/после ACK**

Чистый API состояния должен выражать четыре результата:

```rust
enum DeferredFocusBarrierDecision {
    Ready,
    ReleaseAllowed,
    Waiting { remaining: Duration },
    TimedOut { marker: DeferredFocusBarrierMarker },
}
```

- `after_ack(sequence_id, generation, now)` ставит deadline только после ACK
  финального release.
- `before_next_event(generation, now, ledger_head)`:
  - удаляет marker сразу, если generation уже отличается;
  - возвращает `ReleaseAllowed`, если текущая голова ledger имеет `value == 0`;
    release отправляется и ACK-ается строго на своём месте, marker остаётся
    активным;
  - возвращает `Waiting`, пока deadline не истёк;
  - возвращает `TimedOut` и удаляет marker при 300 мс.
- Timeout не удаляет и не ACK-ает следующий ledger event.
- Нельзя перепрыгивать через удерживаемый press/repeat ради более позднего
  release: порядок ledger остаётся строгим. Разрешаются только ведущие
  release-события, для которых соответствующий press уже был доставлен до
  barrier.

- [ ] **Step 4: Подключить наблюдение только на physical admission**

Расширить `DeferredManualCurrentWordSession` полем
`deferred_focus_barriers`. В
`observe_deferred_physical_focus_invalidation()` перед/после обновления
`observed_physical_modifiers` передать:

- `SessionType`;
- admitted event;
- текущее `KeyboardController::input_target_generation()`.

Wayland invalidation остаётся прежним. X11 marker не является новым поводом
отменять коррекцию и не меняет word context сам по себе.

- [ ] **Step 5: Запустить чистую матрицу и зафиксировать Task 3**

```bash
cargo test --locked --lib deferred_focus_barrier -- --nocapture
cargo test --locked --lib x11_ -- --nocapture
cargo fmt --all -- --check
git diff --check
git add src/daemon/service.rs
git commit -m "fix: track deferred X11 focus barriers"
```

Expected GREEN: полные/incomplete/multiple/timeout/Wayland cases PASS.

### Task 4: Origin-aware ACK и focus barrier в реальном drain

**Files:**

- Modify: `src/daemon/service.rs:1529-1537`
- Modify: `src/daemon/service.rs:1793-1835`
- Modify: `src/daemon/service.rs:1863-1921`
- Modify: forwarding sites inside
  `src/daemon/service.rs:1939-2588`
- Test: `src/daemon/service.rs:5487-5535`

- [ ] **Step 1: Написать RED-тест origin routing**

Добавить testable selector/helper и тест
`deferred_replay_uses_acknowledged_forward_while_other_origins_use_fast_path`.

Ожидание:

```text
Physical      -> fast forward
DeferredRetry -> fast forward
DeferredReplay -> acknowledged deferred transaction
```

Run:

```bash
cargo test --locked --lib deferred_replay_uses_acknowledged_forward_while_other_origins_use_fast_path
```

Expected RED: все origin сейчас заканчиваются обычным `forward_event()`.

- [ ] **Step 2: Централизовать forwarding по `InputOrigin`**

Добавить один helper:

```rust
fn forward_event_for_origin(
    keyboard: &mut KeyboardController,
    origin: InputOrigin,
    key: evdev::Key,
    value: i32,
) -> Result<(), SwitcherError> {
    match origin {
        InputOrigin::DeferredReplay => keyboard.forward_deferred_event(key, value),
        InputOrigin::Physical | InputOrigin::DeferredRetry => {
            keyboard.forward_event(key, value)
        }
    }
}
```

Все прямые `forward_event()` внутри `handle_key_event()` и передаваемых им
word-tracking closures заменить этим helper. Capture forwarding до manual-flow
routing и другие вызовы вне этого handler не менять.

Событие, сознательно swallowed существующей логикой, считается обработанным
без writer mutation. Событие, которое должно попасть в uinput, возвращает
`Ok(())` только после writer ACK.

- [ ] **Step 3: Написать RED combined regression**

Добавить
`x11_generation_barrier_holds_digit_tail_after_acknowledged_alt_tab`.

Тест использует ledger:

```text
Alt↓(1), Tab↓(2), Tab↑(3), Alt↑(4),
1↓(5), 1↑(6), 2↓(7), 2↑(8)
```

и чистый acknowledged-forward collector.

Проверки:

1. При generation `41` первые четыре события проходят и ACK-аются.
2. До смены generation ни одно событие с sequence `5..=8` не передано и не
   ACK-нуто.
3. При generation `42` хвост передан ровно один раз и в исходном порядке.
4. Ledger пуст только после ACK sequence 8.

Отдельный тест
`x11_generation_barrier_deadline_releases_but_does_not_preack_digit_tail`
проверяет fail-open: до 300 мс хвост сохранён, после deadline отправляется,
но удаляется только после successful handler result.

Добавить
`x11_generation_barrier_forwards_leading_modifier_release_before_text_wait`.
Ledger после финального `Alt up` содержит `Shift up`, затем `1 down/up`.
`Shift up` должен пройти и получить ACK при старом generation, цифра должна
остаться в голове до generation change/deadline. Marker после `Shift up` не
снимается. Это исключает временно залипший modifier, не нарушая порядок.

- [ ] **Step 4: Встроить barrier в `drain_one_deferred_input_event()`**

Порядок одной итерации:

```text
read persistent generation
-> evaluate front waiting marker
-> if ReleaseAllowed: handle и ACK только текущий key-up, marker сохранить
-> if Waiting: do not send/ACK ledger head
-> if TimedOut: log ids/counts only, continue
-> handle one ledger head through origin-aware forwarding
-> ACK ledger head
-> arm marker if this ACK completed focus shortcut
```

Лог timeout не содержит key data или набранный текст. Достаточны request id,
marker sequence id, elapsed/deadline и generation before/current.

Если `forward_deferred_event()` возвращает timeout/disconnect/backend error,
`drain_deferred_head_with()` не ACK-ает голову. Ошибка поднимается в
существующий release-first terminal recovery, который reconciles оставшийся
ledger и не переносит неоднозначное событие в новый backend.

- [ ] **Step 5: Убрать busy-spin только на время активного barrier**

`event_fetch_timeout()` возвращает:

- `Duration::ZERO` при обычном `DrainingDeferredInput`;
- `Duration::ZERO`, если активный barrier разрешил текущую leading release;
- `min(remaining, X11_DEFERRED_FOCUS_BARRIER_POLL)` при активном ожидании;
- прежние значения для `Idle`, `InFlight`, `CancelRequested`.

Это не correctness polling `_NET_ACTIVE_WINDOW`: generation меняется
event-driven watcher. 5 мс только ограничивают задержку, с которой основной
loop заметит уже опубликованное поколение и продолжит принимать physical
events.

- [ ] **Step 6: Проверить интеграцию и зафиксировать Task 4**

```bash
cargo test --locked --lib deferred_replay -- --nocapture
cargo test --locked --lib x11_generation_barrier -- --nocapture
cargo test --locked --lib invalidation_while_draining -- --nocapture
cargo fmt --all -- --check
git diff --check
git add src/daemon/service.rs
git commit -m "fix: gate deferred X11 text on target change"
```

Expected GREEN: хвост не отправляется до generation/deadline; pointer
invalidation и существующий полный `Alt+Tab` ledger test остаются зелёными.

### Task 5: Аварийные ветви и отсутствие scope drift

**Files:**

- Modify only if a RED test exposes a defect:
  `src/daemon/keyboard.rs`, `src/daemon/service.rs`, `src/error/mod.rs`

- [ ] **Step 1: Добавить targeted edge tests**

Покрыть:

```text
deferred_forward_disconnect_keeps_ledger_head
deferred_forward_write_error_keeps_ledger_head
generation_change_before_final_release_skips_wait_after_ack
pointer_click_during_barrier_does_not_drop_marker_or_tail
physical_events_admitted_while_barrier_waits_remain_ordered
leading_modifier_release_is_not_delayed_by_barrier
blocked_press_is_not_reordered_past_its_later_release
second_alt_tab_marker_cannot_replace_first_marker
barrier_state_is_discarded_only_with_terminal_session_reset
```

Для timeout/disconnect не создавать новый backend в unit test: проверить
сохранённую голову, sticky writer failure и terminal reconciliation report.

- [ ] **Step 2: Проверить неизменность чувствительных semantics**

Run:

```bash
rg -n "layout_delay_ms|typing_ms|backspace_ms" src
rg -n "X11_DEFERRED_FOCUS_BARRIER|DEFERRED_FORWARD_TRANSACTION_TIMEOUT" src/daemon
rg -n "forward_event\\(" src/daemon/service.rs
git diff 69fc1a8 -- src/daemon/keyboard.rs src/daemon/service.rs src/error/mod.rs
git diff --check
```

Expected:

- пользовательские correction delays не изменены;
- новые `300 ms`, `5 ms`, `1 s` используются только в заявленных границах;
- обычный physical fast path не превращён в transaction;
- pointer movement/touch/click classification не изменена;
- новых `unsafe` и key-content logging нет.

- [ ] **Step 3: Полная безопасная локальная матрица**

На host разрешены только тесты, которые не открывают реальные input/uinput:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo test --locked --lib
cargo test --locked --features settings-ui --lib -j1
cargo test --locked --test dbus_api
cargo clippy --locked --all-targets --all-features
git diff --check
```

Expected:

- core lib: все tests PASS;
- settings-ui lib: все tests PASS;
- D-Bus integration: все tests PASS;
- check/clippy exit 0; известные pre-existing warnings записываются, но не
  маскируют новые warnings в изменённых местах.

- [ ] **Step 4: Выполнить два независимых review-pass**

Review A — соответствие спецификации:

- ACK только после `write+synchronize`;
- один in-flight deferred event;
- 1-second sticky fail-stop;
- marker после полного release;
- persistent generation;
- 300 ms fail-open без pre-ACK;
- release-first safety без обхода строгого ledger order;
- несколько markers и X11-only scope.

Review B — регрессии:

- потеря/дублирование/перестановка press/release;
- stuck Alt/Tab/modifier;
- deadlock terminal gate;
- busy-spin;
- replay неоднозначного события в новом backend;
- изменение ordinary F12/Alt+Tab/Wayland;
- утечка key data в лог.

Каждое конкретное замечание сначала воспроизвести RED-тестом, затем исправить
минимально и повторить Step 2–3.

- [ ] **Step 5: Commit review fixes при наличии diff**

```bash
git add src/daemon/keyboard.rs src/daemon/service.rs src/error/mod.rs
git commit -m "fix: close deferred focus barrier review gaps"
```

Пустой commit не создавать.

### Task 6: Debian `0.1.0-3`, exact-package VM acceptance и отчёт

**Files:**

- Modify: `debian/changelog`
- Create:
  `docs/audits/2026-07-23-deferred-input-conservation-validation.md`
- Generate: `dist/packages/open-switcher_0.1.0-3_amd64.deb`

- [ ] **Step 1: Поднять Debian revision**

Добавить верхнюю запись `0.1.0-3` с двумя фактическими пунктами:

- deferred physical input подтверждается после writer mutation;
- X11 text tail ждёт подтверждённой смены active target после focus shortcut.

Cargo application version и systemd policy не менять.

Run:

```bash
head -n 14 debian/changelog
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
git add debian/changelog
git commit -m "chore: prepare 0.1.0-3 package"
```

Expected: package-script tests PASS; новая запись выше `0.1.0-2`.

- [ ] **Step 2: Собрать и идентифицировать canonical DEB**

```bash
DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb
sha256sum dist/packages/open-switcher_0.1.0-3_amd64.deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-3_amd64.deb
dpkg-deb --fsys-tarfile dist/packages/open-switcher_0.1.0-3_amd64.deb \
  | tar -xOf - ./usr/bin/open-switcher-daemon \
  | sha256sum
```

Expected: version `0.1.0-3`, arch `amd64`; package SHA и daemon SHA записаны в
validation report. DEB не коммитить.

- [ ] **Step 3: Установить exact package в Mint Cinnamon X11**

Из VM-lab worktree:

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session mint-installed
```

Передать exact DEB на loopback SSH port `22223`, сверить SHA внутри гостя,
установить через гостевой root/QGA control channel, выполнить user
`daemon-reload` и restart. Подтвердить:

```text
dpkg-query version = 0.1.0-3
/usr/bin/open-switcher-daemon SHA = packaged daemon SHA
/proc/$PID/exe не содержит "(deleted)"
session = Cinnamon/X11
```

- [ ] **Step 4: Выполнить Mint package-first matrix**

Проверить на обычных значениях:

- baseline typing;
- `ыгвщ` + F12 -> `sudo`;
- F12 + немедленный `tail` -> `sudotail`;
- обычный `Alt+Tab` + цифры;
- движение pointer не инвалидирует контекст;
- physical click инвалидирует и переносит хвост ровно один раз;
- Enter и прежние reset events не изменились.

Затем временно поставить внутри VM ранее использованные максимальные валидные
`backspace_ms=10`, `typing_ms=10` и повторить исходный stress:

```text
F12
~2 ms
Alt down -> Tab down -> Tab up -> Alt up
сразу 1/2/3/4
```

Acceptance:

- все четыре focus-shortcut события подтверждены;
- `_NET_ACTIVE_WINDOW` generation изменилось либо явно сработал 300-ms
  deadline;
- `1234` присутствует ровно один раз в целевом окне;
- ledger: `accepted == acknowledged`, `reconciled == 0`;
- PID и virtual device не меняются на successful path;
- нет stuck Alt/Tab.

Сохранить новый input log и screenshots рядом с:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/
```

Старое RED evidence не перезаписывать.

- [ ] **Step 5: Установить exact package и выполнить Wayland smoke**

Mint VM сначала штатно остановить. Затем:

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session ubuntu-installed
```

Установить тот же SHA через port `22222`. Подтвердить GNOME/Wayland и
повторить доступные:

- F12 current word;
- F12 + немедленный хвост;
- обычный `Alt+Tab` + ввод;
- click/movement behavior;
- stop/start daemon.

Acceptance: Wayland не получает X11 300-ms barrier; ordinary behavior и
delivery не регрессировали. Результат не выдавать за X11/XTest проверку.

- [ ] **Step 6: Записать validation report**

Создать
`docs/audits/2026-07-23-deferred-input-conservation-validation.md` на русском:

- commit range и package/daemon SHA;
- исходный RED trace и точный сценарий;
- локальная test matrix с фактическими counts;
- Mint GREEN scenario и generation/deadline branch;
- Ubuntu Wayland smoke;
- подтверждение неизменности tuned delays;
- отсутствие host-side input/session mutations;
- остаточные ограничения: kernel/desktop вне daemon, `SIGKILL`, power loss,
  невозможность доказать визуальный target без VM runtime.

- [ ] **Step 7: Финальная проверка и commit отчёта**

```bash
git status --short
git diff --check
cargo fmt --all -- --check
git add -f docs/audits/2026-07-23-deferred-input-conservation-validation.md
git commit -m "docs: validate deferred X11 focus barrier"
git log --oneline 69fc1a8..HEAD
```

Expected: только намеренные commits, tests/VM evidence отражены фактически,
лаборатория сохранена.

## Критерии завершения

- На deferred replay ledger ACK означает завершившийся writer
  `write+synchronize`, а не enqueue в FIFO.
- При timeout/disconnect/error голова ledger не удалена; backend переходит в
  существующий fail-stop recovery без replay неоднозначного события.
- После полного X11 `Alt/Meta+Tab` текст не отправляется до смены persistent
  target generation или bounded 300-ms fail-open.
- Уже ожидающие leading key-up проходят до текстового ожидания, не снимают
  marker и не переставляются относительно других событий.
- Несколько shortcut markers не заменяют друг друга.
- Обычный physical fast path, F12, pointer policy, reset events и Wayland
  остались прежними.
- Исходный Mint/X11 stress проходит на exact Debian `0.1.0-3`, тот же package
  проходит Wayland smoke.
- Полные локальные tests и package-script tests зелёные; validation report
  содержит hashes, evidence и честные ограничения.
