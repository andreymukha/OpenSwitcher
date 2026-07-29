# H-06 XTEST latency — план реализации

> **Для agentic workers:** REQUIRED SUB-SKILL: использовать
> `superpowers:subagent-driven-development` (рекомендуется) или
> `superpowers:executing-plans` для пошагового выполнения. Шаги отслеживаются
> checkbox (`- [ ]`).

**Цель:** Сократить задержку коррекции Cinnamon/X11, удалив доказуемо
дублирующий X11 round-trip и повторную проверку одинаковых mapping, не меняя
deliberate delays, trace или fail-safe семантику H-06.

**Архитектура:** Первый независимый коммит превращает успешный
`VoidCookie::check()` в одноразовое типизированное подтверждение, потребляемое
протокольным `Synchronize`. Второй независимый коммит добавляет bounded
operation-scoped mapping cache в guardian service; каждый `PrepareKey`
по-прежнему создаёт уникальный token. Оба коммита получают отдельный DEB и
сравниваются с точным H-06 пакетом в сохранённой Mint/Cinnamon/X11 ВМ.

**Стек:** Rust 1.95, x11rb 0.13.2, XTEST, Unix `SOCK_SEQPACKET`, Cargo tests,
Debian packaging, QEMU/KVM Mint/Cinnamon/X11 laboratory.

---

## Карта файлов

- `src/daemon/xtest_guardian/x11.rs` — checked X11 mutation proof и его
  одноразовое потребление; emergency round-trip остаётся прежним.
- `src/daemon/xtest_guardian/service.rs` — bounded mapping cache одной
  `OperationId`, уникальные token и очистка при terminal transition.
- `docs/superpowers/specs/2026-07-29-h06-xtest-latency-design.md` — утверждённые
  ограничения и уточнения ревью.
- `docs/audits/2026-07-29-h06-xtest-latency-validation.md` — фактические
  результаты тестов, DEB identity, VM trace и timing.

## Задача 1: Зафиксировать уточнения дизайна

**Файлы:**

- Изменить:
  `docs/superpowers/specs/2026-07-29-h06-xtest-latency-design.md`
- Создать:
  `docs/superpowers/plans/2026-07-29-h06-xtest-latency.md`

- [ ] **Шаг 1: проверить три уточнения**

```bash
rg -n \
  'Порядок внедрения|старый proof|трёх чередующихся серий|checked_fake_key.*до' \
  docs/superpowers/specs/2026-07-29-h06-xtest-latency-design.md
```

Ожидается: документ требует два независимых коммита, запрет stale proof,
чередующиеся серии и VM probe до локального `Synchronize`.

- [ ] **Шаг 2: проверить документы**

```bash
rg -n 'TO''DO|T''BD|FIX''ME|PLACE''HOLDER' \
  docs/superpowers/specs/2026-07-29-h06-xtest-latency-design.md \
  docs/superpowers/plans/2026-07-29-h06-xtest-latency.md
git diff --check
```

Ожидается: placeholder отсутствуют; `git diff --check` завершается с кодом `0`.

- [ ] **Шаг 3: зафиксировать документы**

```bash
git add -f \
  docs/superpowers/specs/2026-07-29-h06-xtest-latency-design.md \
  docs/superpowers/plans/2026-07-29-h06-xtest-latency.md
git commit -m "docs: refine XTEST latency implementation gates"
```

## Задача 2: Повторно использовать checked-mutation proof

**Файлы:**

- Изменить и тестировать: `src/daemon/xtest_guardian/x11.rs`

- [ ] **Шаг 1: добавить RED-тесты**

Расширить `FakeX11Connection` полем
`fail_fake_event_number: Option<usize>`. В test implementation `fake_key`
сначала записывает событие, затем возвращает ошибку на указанном номере.
В `Default` новое поле получает `None`.

Добавить три теста:

```rust
#[test]
fn checked_mutation_satisfies_sync_without_second_round_trip() {
    let identity = test_server_identity();
    let connection = FakeX11Connection::default().with_identity(&identity);
    let mut executor = GuardianX11Executor::from_connection(connection, identity).unwrap();

    executor.key_up(38).unwrap();
    executor.synchronize().unwrap();

    assert_eq!(executor.connection_ref().fake_events, [(38, false)]);
    assert_eq!(executor.connection_ref().round_trips, 0);
}

#[test]
fn checked_mutation_confirmation_is_consumed_once() {
    let identity = test_server_identity();
    let connection = FakeX11Connection::default().with_identity(&identity);
    let mut executor = GuardianX11Executor::from_connection(connection, identity).unwrap();

    executor.key_up(38).unwrap();
    executor.synchronize().unwrap();

    assert!(executor.synchronize().is_err());
    assert_eq!(executor.connection_ref().round_trips, 0);
}

#[test]
fn failed_new_mutation_cannot_reuse_stale_confirmation() {
    let identity = test_server_identity();
    let connection = FakeX11Connection {
        fail_fake_event_number: Some(2),
        ..FakeX11Connection::default().with_identity(&identity)
    };
    let mut executor = GuardianX11Executor::from_connection(connection, identity).unwrap();

    executor.key_up(38).unwrap();
    assert!(executor.key_up(39).is_err());

    assert!(executor.synchronize().is_err());
    assert_eq!(
        executor.connection_ref().fake_events,
        [(38, false), (39, false)]
    );
}
```

- [ ] **Шаг 2: запустить RED**

```bash
cargo test --locked --lib checked_mutation_ -- --nocapture
cargo test --locked --lib failed_new_mutation_cannot_reuse_stale_confirmation \
  -- --exact --nocapture
```

Ожидается: новые assertions падают, потому что текущий `synchronize` выполняет
`round_trip`, допускает повторный sync и не хранит proof.

- [ ] **Шаг 3: добавить типизированное подтверждение**

В `x11.rs` ввести закрытый тип и сделать boundary-контракт явным:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CheckedX11Mutation(());

pub(crate) trait X11ConnectionBoundary {
    // Остальные методы без изменений.
    fn checked_fake_key(
        &mut self,
        keycode: u8,
        pressed: bool,
    ) -> Result<CheckedX11Mutation, SwitcherError>;
    fn round_trip(&mut self) -> Result<(), SwitcherError>;
}
```

`RustX11Connection::checked_fake_key` возвращает `CheckedX11Mutation` только
после существующих успешных `.check()` и `flush()`.

Добавить executor state:

```rust
pub(crate) struct GuardianX11Executor<C: X11ConnectionBoundary> {
    connection: C,
    identity: X11ServerIdentity,
    pending_confirmation: Option<CheckedX11Mutation>,
}
```

Оба конструктора инициализируют `pending_confirmation: None`.

- [ ] **Шаг 4: минимально реализовать одноразовое потребление**

Добавить helper:

```rust
fn checked_mutation(
    &mut self,
    keycode: u8,
    pressed: bool,
    failure: &'static str,
) -> Result<(), InputSafetyError> {
    self.pending_confirmation = None;
    let confirmation = self
        .connection
        .checked_fake_key(keycode, pressed)
        .map_err(|_| executor_error(failure))?;
    self.pending_confirmation = Some(confirmation);
    Ok(())
}
```

`key_down` и `key_up` вызывают helper. `synchronize` не обращается к X11:

```rust
fn synchronize(&mut self) -> Result<(), InputSafetyError> {
    self.pending_confirmation
        .take()
        .map(|_| ())
        .ok_or_else(|| {
            executor_error("XTEST guardian synchronization has no checked mutation")
        })
}
```

`EmergencyX11Releaser::release_token` вызывает `checked_fake_key` и отбрасывает
типизированный результат; его следующий явный `round_trip` не меняется.

- [ ] **Шаг 5: запустить GREEN и существующие X11 tests**

```bash
cargo test --locked --lib xtest_guardian::x11::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::service::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::client::tests -- --test-threads=1
```

Ожидается: все PASS; normal executor не вызывает второй `round_trip`,
emergency tests по-прежнему требуют его.

- [ ] **Шаг 6: проверить формат и diff**

```bash
cargo fmt --all -- --check
git diff --check
git diff -- src/daemon/xtest_guardian/x11.rs
```

Ожидается: изменён только XTEST boundary/executor и его тесты; protocol,
delays и uinput отсутствуют в diff.

- [ ] **Шаг 7: зафиксировать первый production-коммит**

```bash
git add src/daemon/xtest_guardian/x11.rs
git commit -m "perf: reuse checked XTEST mutation confirmation"
```

## Задача 3: Добавить operation-scoped mapping cache

**Файлы:**

- Изменить и тестировать: `src/daemon/xtest_guardian/service.rs`

- [ ] **Шаг 1: добавить RED-test support**

Расширить `FakeXtestExecutor`:

```rust
prepare_calls: Vec<u16>,
fail_prepare_number: Option<usize>,
```

В `Default` задать `prepare_calls: Vec::new()` и
`fail_prepare_number: None`.

И изменить `prepare_key`:

```rust
fn prepare_key(
    &mut self,
    evdev_code: u16,
) -> Result<(u8, ServerEpoch), InputSafetyError> {
    self.prepare_calls.push(evdev_code);
    if self.fail_prepare_number == Some(self.prepare_calls.len()) {
        return Err(InputSafetyError::Invariant {
            context: "fake prepare failure",
        });
    }
    Ok(((evdev_code + 8) as u8, self.identity.epoch))
}
```

- [ ] **Шаг 2: добавить три RED-теста**

Добавить тесты:

```rust
#[test]
fn repeated_prepares_share_mapping_but_keep_unique_tokens() {
    let mut executor = FakeXtestExecutor::default();
    let mut session = GuardianSession::ready(test_session(), &mut executor).unwrap();
    let mut token_ids = Vec::new();

    for sequence in 1..=4 {
        let Response::Prepared { token, .. } = session
            .handle_request(
                Sequence(sequence),
                Request::PrepareKey {
                    operation: OperationId(1),
                    evdev_code: 30,
                    deadline: DEADLINE,
                },
                NOW_NS,
            )
            .unwrap()
        else {
            panic!("expected prepared response");
        };
        token_ids.push(token.token_id);
    }

    assert_eq!(token_ids, [1, 2, 3, 4]);
    assert_eq!(session.executor_ref().prepare_calls, [30]);
}

#[test]
fn mapping_cache_revalidates_same_key_for_new_operation() {
    let mut cache = OperationMappingCache::default();
    let mut executor = FakeXtestExecutor::default();

    cache.resolve(OperationId(1), 30, &mut executor).unwrap();
    cache.resolve(OperationId(1), 30, &mut executor).unwrap();
    cache.resolve(OperationId(2), 30, &mut executor).unwrap();

    assert_eq!(executor.prepare_calls, [30, 30]);
}

#[test]
fn failed_mapping_is_not_cached() {
    let mut cache = OperationMappingCache::default();
    let mut executor = FakeXtestExecutor {
        fail_prepare_number: Some(1),
        ..FakeXtestExecutor::default()
    };

    assert!(cache
        .resolve(OperationId(1), 30, &mut executor)
        .is_err());
    executor.fail_prepare_number = None;
    cache.resolve(OperationId(1), 30, &mut executor).unwrap();

    assert_eq!(executor.prepare_calls, [30, 30]);
}
```

- [ ] **Шаг 3: запустить RED**

```bash
cargo test --locked --lib mapping_cache_ -- --nocapture
cargo test --locked --lib repeated_prepares_share_mapping_but_keep_unique_tokens \
  -- --exact --nocapture
```

Ожидается: тесты не собираются из-за отсутствующего
`OperationMappingCache`/нового поведения. Это допустимый RED API-test; до
production-кода cache API не существует.

- [ ] **Шаг 4: реализовать bounded cache**

Импортировать `OperationId` и `MAX_PREPARED_TOKENS`. Добавить:

```rust
#[derive(Clone, Copy)]
struct PreparedMapping {
    evdev_code: u16,
    x11_keycode: u8,
    epoch: ServerEpoch,
}

#[derive(Default)]
struct OperationMappingCache {
    operation: Option<OperationId>,
    entries: Vec<PreparedMapping>,
}

impl OperationMappingCache {
    fn resolve<E: XtestExecutor>(
        &mut self,
        operation: OperationId,
        evdev_code: u16,
        executor: &mut E,
    ) -> Result<(u8, ServerEpoch), InputSafetyError> {
        if self.operation != Some(operation) {
            self.operation = Some(operation);
            self.entries.clear();
        }
        if let Some(mapping) = self
            .entries
            .iter()
            .find(|mapping| mapping.evdev_code == evdev_code)
            .copied()
        {
            return Ok((mapping.x11_keycode, mapping.epoch));
        }

        let (x11_keycode, epoch) = executor.prepare_key(evdev_code)?;
        if self.entries.len() < MAX_PREPARED_TOKENS {
            self.entries.push(PreparedMapping {
                evdev_code,
                x11_keycode,
                epoch,
            });
        }
        Ok((x11_keycode, epoch))
    }

    fn clear(&mut self) {
        self.operation = None;
        self.entries.clear();
    }
}
```

Добавить `mapping_cache: OperationMappingCache` в `GuardianSession` и
инициализировать `default()`.

- [ ] **Шаг 5: подключить cache без изменения token ledger**

В `Request::PrepareKey` заменить только вызов mapping:

```rust
let (x11_keycode, epoch) = self
    .mapping_cache
    .resolve(operation, evdev_code, self.executor)
    .map_err(RequestFailure::backend)?;
```

Проверка epoch, генерация `next_token_id` и
`ProtocolState::record_prepared` остаются byte-for-byte прежними.

В начале первого terminal transition перед `protocol.begin_terminal()`:

```rust
self.mapping_cache.clear();
```

- [ ] **Шаг 6: запустить GREEN и service/protocol suites**

```bash
cargo test --locked --lib mapping_cache_ -- --nocapture
cargo test --locked --lib repeated_prepares_share_mapping_but_keep_unique_tokens \
  -- --exact --nocapture
cargo test --locked --lib xtest_guardian::service::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::protocol::tests -- --test-threads=1
```

Ожидается: один mapping call на четыре одинаковых prepare, четыре token,
повторная валидация в новой операции и отсутствие cache после ошибки.

- [ ] **Шаг 7: добавить и проверить terminal-clear test**

Подготовить один token, проверить `mapping_cache.entries.len() == 1`, вызвать
`finish_with_clock`, затем проверить `operation.is_none()` и пустые entries.

```bash
cargo test --locked --lib terminal_transition_clears_mapping_cache \
  -- --exact --nocapture
```

Ожидается: PASS.

- [ ] **Шаг 8: проверить формат и diff**

```bash
cargo fmt --all -- --check
git diff --check
git diff -- src/daemon/xtest_guardian/service.rs
```

Ожидается: cache не попал в `SyntheticOperation`, protocol codec, uinput или
конфигурацию задержек.

- [ ] **Шаг 9: зафиксировать второй production-коммит**

```bash
git add src/daemon/xtest_guardian/service.rs
git commit -m "perf: reuse XTEST mapping within one operation"
```

## Задача 4: Полная безопасная проверка и два DEB

**Файлы и артефакты:**

- Проверить: весь репозиторий
- Создать: barrier-only и combined DEB с commit SHA в имени под
  `/home/andrey/VMs/OpenSwitcherLab/artifacts/`

- [ ] **Шаг 1: выполнить focused safety suites**

```bash
cargo test --locked --lib synthetic_input::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::protocol::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::service::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::client::tests -- --test-threads=1
cargo test --locked --lib xtest_guardian::process_tests \
  -- --test-threads=1 --nocapture
```

Ожидается: все PASS; реальные X11/input/uinput не открываются.

- [ ] **Шаг 2: выполнить полную регрессию**

```bash
env -u DISPLAY -u WAYLAND_DISPLAY \
  cargo test --locked --all-targets --features settings-ui \
  -- --test-threads=1
cargo check --locked --all-targets --features settings-ui
cargo fmt --all -- --check
git diff --check
```

Ожидается: полная suite PASS вне restricted syscall sandbox; format/diff чисты.

- [ ] **Шаг 3: выполнить package suites**

```bash
bash tests/wayland_diagnostics_test.sh
bash tests/linux_input_setup_test.sh
bash tests/debian_package_scripts_test.sh
bash tests/manage_package_deb_test.sh
```

Ожидается: четыре результата `ok`.

- [ ] **Шаг 4: собрать DEB на каждом production-коммите**

На barrier-only и combined commit выполнить:

```bash
./manage.sh package deb
package="$(dpkg-parsechangelog -S Source)"
version="$(dpkg-parsechangelog -S Version)"
arch="$(dpkg --print-architecture)"
built="$(realpath "dist/packages/${package}_${version}_${arch}.deb")"
test -f "$built"
sha256sum "$built"
dpkg-deb -f "$built" Package Version Architecture
```

Скопировать каждый exact файл под уникальным именем с полным source commit SHA.
Между двумя сборками не менять исходники и lockfile.

## Задача 5: Mint package-first performance и safety campaign

**Среда:**

- Controller:
  `/home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab`
- VM: `mint-installed`, SSH `127.0.0.1:22223`
- Evidence:
  `/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/h06-xtest-latency/`
- Пакеты: точный H-06, barrier-only и combined DEB

- [ ] **Шаг 1: запустить только Mint VM**

```bash
cd /home/andrey/Projects/OpenSwitcher/.worktrees/vm-lab
python3 -m tools.vm_lab.session mint-installed
```

Ожидается: profile `mint-installed`, QMP path и port `22223`; Ubuntu выключена.

- [ ] **Шаг 2: передать exact DEB и probe**

```bash
KEY=/home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519
KNOWN_HOSTS=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts
ssh -i "$KEY" -p 22223 -o UserKnownHostsFile="$KNOWN_HOSTS" \
  -o StrictHostKeyChecking=yes \
  openswitcher@127.0.0.1 \
  'install -d -m 0700 /home/openswitcher/h06-xtest-latency'
```

Передать три exact DEB и `h06_x11_vm_probe` через `scp`, затем сверить
`sha256sum` host/guest до первой установки.

- [ ] **Шаг 3: выполнить чередующиеся серии**

Порядок пакетов:

```text
H-06 -> barrier-only -> combined -> H-06 -> combined -> barrier-only
```

Для каждого установленного состояния выполнить 30 одинаковых четырёхбуквенных
F12-коррекций через QMP physical keyboard. Сохранить input-debug, observer
JSONL и package SHA. Не менять `backspace_ms=10`, `typing_ms=10`,
`layout_delay_ms=30`.

- [ ] **Шаг 4: рассчитать распределения**

Для каждого состояния сохранить:

```text
n, minimum, median, p95, maximum
full completion elapsed_ms
first Backspace press -> final replay release
guardian p50_us/p95_us/max_us
```

Normalized press/release trace и количество событий должны совпадать с H-06.

- [ ] **Шаг 5: выполнить check-before-local-sync probe**

В observer connection подтвердить, что XTEST key state уже изменён после
успешного checked mutation response и до локального протокольного
`Synchronize`; затем всегда выполнить matching release и проверить key-up.

- [ ] **Шаг 6: повторить safety gates для combined candidate**

Проверить normal F12, Caps Lock, две заглавные, Shift/modifier, stop/restart,
daemon `SIGKILL` и guardian `SIGKILL`. После каждого аварийного сценария:

```text
physical marker вводится
keymap clean
нет orphan guardian
нет timeout/protocol failure/Unreconciled
```

- [ ] **Шаг 7: выключить VM без удаления**

Сохранить evidence и штатно выключить guest. Overlay, base image, packages,
keys и лабораторию не удалять.

## Задача 6: Отчёт и решение о кандидате

**Файлы:**

- Создать:
  `docs/audits/2026-07-29-h06-xtest-latency-validation.md`

- [ ] **Шаг 1: написать фактический русский отчёт**

Указать source commits, SHA-256 трёх DEB, версии guest, конфигурацию задержек,
число серий, распределения, exact trace, safety gates и ограничения.

- [ ] **Шаг 2: принять техническое решение**

Возможны только три вывода:

1. barrier-only даёт достаточный выигрыш — рекомендовать только первый commit;
2. combined даёт дополнительный устойчивый выигрыш без регрессии —
   рекомендовать оба;
3. выигрыш не превышает шум либо safety gate красный — не рекомендовать
   performance-ветку.

Ни один вариант не сливается автоматически.

- [ ] **Шаг 3: проверить и зафиксировать отчёт**

```bash
rg -n 'TO''DO|T''BD|FIX''ME|PLACE''HOLDER' \
  docs/audits/2026-07-29-h06-xtest-latency-validation.md
git diff --check
git add -f docs/audits/2026-07-29-h06-xtest-latency-validation.md
git commit -m "docs: record XTEST latency validation"
```

Ожидается: worktree чистый; merge, push и удаление laboratory не выполняются
без отдельного решения пользователя.
