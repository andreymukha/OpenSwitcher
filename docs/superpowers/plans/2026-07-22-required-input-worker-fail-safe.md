# Fail-safe обязательных input-worker — план реализации

> **Для agentic workers:** REQUIRED SUB-SKILL: использовать
> `superpowers:subagent-driven-development` (рекомендуется) либо
> `superpowers:executing-plans` и выполнять задачи по отмечаемым пунктам.
> Для этой работы пользователь уже выбрал inline-выполнение через
> `superpowers:executing-plans`.

**Цель:** при смерти обязательного pointer/X11 worker после `EVIOCGRAB`
быстро освободить физическую клавиатуру, оставить daemon живым и автоматически
восстановить полный input backend перед следующим захватом.

**Архитектура:** существующие атомарные `alive` становятся общей runtime
health-границей наряду с writer health. `InputWorkerDisconnected` направляется
в уже существующий `InputBackendLifecycle::Recovering`; уже существующий
shutdown снимает grab до `join`, а backoff заново готовит весь pipeline.
В X11 невозможность создать `_NET_ACTIVE_WINDOW` monitor больше не разрешает
работу с необязательным disabled watcher.

**Технологии:** Rust, `evdev`, `x11rb`, unit-тесты `cargo test`, Debian package,
Mint/Cinnamon X11 VM, ограниченная fault injection только внутри гостя.

---

## Структура изменения

- `src/daemon/keyboard.rs` — единая политика здоровья pointer/X11 watcher,
  runtime health API контроллера и обязательное подключение X11 monitor.
- `src/daemon/input_backend.rs` — перевод `InputWorkerDisconnected` из `Ready`
  в `Recovering` с существующим backoff.
- `src/daemon/service.rs` — проверка всех обязательных input-компонентов до и
  после fetch и перед каждым событием; recoverable health-ошибка прекращает
  текущую итерацию и удаляет backend.
- `docs/audits/2026-07-22-required-input-worker-fail-safe-validation.md` —
  русский отчёт с локальными и package-first VM-доказательствами.

Не изменяются параметры коррекции, clipboard, классификация кликов, X11
event-wait, systemd units и пакетные права.

### Задача 1: Опубликовать runtime health обязательных watcher

**Файлы:**

- Изменить: `src/daemon/keyboard.rs:987-1006,1155-1204`
- Тесты: `src/daemon/keyboard.rs` (`#[cfg(test)]`)

- [ ] **Шаг 1: добавить RED-тест точных watcher-ошибок**

В секции watcher readiness добавить:

```rust
#[test]
fn runtime_input_watcher_health_names_dead_required_worker() {
    assert!(matches!(
        ensure_input_watchers_ready(false, true),
        Err(SwitcherError::InputWorkerDisconnected {
            worker: "pointer-watcher"
        })
    ));
    assert!(matches!(
        ensure_input_watchers_ready(true, false),
        Err(SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher"
        })
    ));
    assert!(ensure_input_watchers_ready(true, true).is_ok());
}
```

- [ ] **Шаг 2: наблюсти ожидаемый RED**

```bash
cargo test --lib runtime_input_watcher_health_names_dead_required_worker -- --nocapture
```

Ожидается ошибка компиляции только из-за отсутствия
`ensure_input_watchers_ready`.

- [ ] **Шаг 3: реализовать общую политику watcher health**

Рядом с `ensure_input_dependencies_ready` добавить и использовать:

```rust
fn ensure_input_watchers_ready(
    pointer_watcher_ready: bool,
    input_target_watcher_ready: bool,
) -> Result<(), SwitcherError> {
    if !pointer_watcher_ready {
        return Err(SwitcherError::InputWorkerDisconnected {
            worker: "pointer-watcher",
        });
    }
    if !input_target_watcher_ready {
        return Err(SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher",
        });
    }
    Ok(())
}
```

`ensure_input_dependencies_ready` после отдельной проверки writer должен
делегировать две watcher-проверки этой функции. В `KeyboardController` добавить:

```rust
pub fn input_worker_health_error(&self) -> Option<SwitcherError> {
    ensure_input_watchers_ready(
        self.pointer_watcher.is_ready(),
        self.input_target_watcher.is_ready(),
    )
    .err()
}
```

Disabled watcher остаётся healthy благодаря существующей семантике
`is_ready() == !required || alive`.

- [ ] **Шаг 4: наблюсти GREEN и отсутствие startup-регрессии**

```bash
cargo test --lib runtime_input_watcher_health_names_dead_required_worker -- --nocapture
cargo test --lib activation_rejects_dead_dependencies_before_physical_grab -- --nocapture
cargo test --lib watcher_readiness -- --nocapture
```

Ожидается PASS во всех командах.

- [ ] **Шаг 5: зафиксировать границу health**

```bash
git add src/daemon/keyboard.rs
git commit -m "feat: expose required input worker health"
```

### Задача 2: Запретить X11 grab без обязательного monitor

**Файлы:**

- Изменить: `src/daemon/keyboard.rs:1447-1485`
- Тесты: `src/daemon/keyboard.rs` (`#[cfg(test)]`)

- [ ] **Шаг 1: добавить RED-тест X11 connection failure**

```rust
#[test]
fn x11_input_target_connection_failure_is_recoverable_worker_failure() {
    let error = prepare_input_target_monitor(SessionType::X11, || {
        Err::<u8, _>(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "test X11 endpoint unavailable",
        ))
    })
    .unwrap_err();

    assert!(matches!(
        error,
        SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher"
        }
    ));
}

#[test]
fn non_x11_input_target_does_not_attempt_x11_connection() {
    let connect_called = Cell::new(false);
    let monitor = prepare_input_target_monitor(SessionType::Wayland, || {
        connect_called.set(true);
        Ok::<_, io::Error>(7u8)
    })
    .unwrap();

    assert_eq!(monitor, None);
    assert!(!connect_called.get());
}
```

- [ ] **Шаг 2: наблюсти ожидаемый RED**

```bash
cargo test --lib input_target_connection -- --nocapture
```

Ожидается ошибка компиляции только из-за отсутствия
`prepare_input_target_monitor`.

- [ ] **Шаг 3: реализовать policy helper и подключить production spawn**

```rust
fn prepare_input_target_monitor<T>(
    session_type: SessionType,
    connect: impl FnOnce() -> io::Result<T>,
) -> Result<Option<T>, SwitcherError> {
    if !should_enable_x11_input_target_watcher(session_type) {
        return Ok(None);
    }

    connect().map(Some).map_err(|error| {
        log_input_debug(
            "input-target-watcher-start-error",
            &format!("source=x11 error={error}"),
        );
        SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher",
        }
    })
}
```

В `InputTargetWatcher::spawn` вызвать helper с
`ActiveWindowMonitor::connect`. `None` должен создавать прежний disabled watcher
только для non-X11 policy. `Err` в X11 должен выйти из `spawn` до grab. Старый
тест с именем `input_target_watcher_readiness_is_true_when_x11_monitor_is_unavailable`
переименовать в `disabled_input_target_watcher_object_is_ready`, потому что он
проверяет только объект, а не допустимость X11 startup.

- [ ] **Шаг 4: наблюсти GREEN**

```bash
cargo test --lib input_target_connection -- --nocapture
cargo test --lib non_x11_input_target_does_not_attempt_x11_connection -- --nocapture
cargo test --lib input_target_watcher_readiness -- --nocapture
cargo test --lib input_target_stop -- --nocapture
```

Ожидается PASS; тесты не подключаются к реальному X11.

- [ ] **Шаг 5: зафиксировать X11 startup policy**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: require input target monitor before X11 grab"
```

### Задача 3: Перевести смерть input-worker в Recovering

**Файлы:**

- Изменить: `src/daemon/input_backend.rs:167-188`
- Тесты: `src/daemon/input_backend.rs` (`#[cfg(test)]`)

- [ ] **Шаг 1: добавить RED-тест lifecycle-перехода**

```rust
#[test]
fn runtime_input_worker_disconnect_enters_recovering() {
    let opener = FakeOpener {
        outcome: FakeOutcome::Ok {
            shutdowns: Rc::new(RefCell::new(0)),
            readiness: ready_readiness(),
        },
    };
    let mut lifecycle = InputBackendLifecycle::new(opener);
    lifecycle.mark_backend_ready(ready_readiness());
    let now = Instant::now();
    let error = SwitcherError::InputWorkerDisconnected {
        worker: "input-target-watcher",
    };

    assert!(lifecycle.record_runtime_failure(&error, now));
    assert_eq!(lifecycle.state(), InputBackendState::Recovering);
    assert!(lifecycle.retry_deadline().is_some_and(|deadline| deadline > now));
}
```

- [ ] **Шаг 2: наблюсти ожидаемый RED**

```bash
cargo test --lib runtime_input_worker_disconnect_enters_recovering -- --nocapture
```

Ожидается assertion failure: `record_runtime_failure` возвращает `false`, а
состояние остаётся `Ready`.

- [ ] **Шаг 3: добавить минимальный lifecycle mapping**

В `record_runtime_failure` добавить отдельную ветвь:

```rust
SwitcherError::InputWorkerDisconnected { .. } => Some(InputBackendState::Recovering),
```

Не расширять recoverable-множество writer и transaction errors.

- [ ] **Шаг 4: наблюсти GREEN и прежние переходы**

```bash
cargo test --lib runtime_input_worker_disconnect_enters_recovering -- --nocapture
cargo test --lib runtime_device_loss_enters_recovering -- --nocapture
cargo test --lib non_recoverable_runtime_error_does_not_leave_ready -- --nocapture
```

Ожидается PASS.

- [ ] **Шаг 5: зафиксировать lifecycle mapping**

```bash
git add src/daemon/input_backend.rs
git commit -m "fix: recover after required input worker loss"
```

### Задача 4: Направить runtime health-ошибку через fail-open shutdown

**Файлы:**

- Изменить: `src/daemon/service.rs:178-224,896-1030,1062-1088`
- Тесты: `src/daemon/service.rs` (`#[cfg(test)]`)

- [ ] **Шаг 1: добавить RED-тест маршрутизации health failure**

```rust
#[test]
fn recoverable_runtime_health_failure_requests_recovery() {
    let recovery_called = Cell::new(false);
    let result = route_runtime_health_failure(
        SwitcherError::InputWorkerDisconnected {
            worker: "input-target-watcher",
        },
        |error| {
            recovery_called.set(matches!(
                error,
                SwitcherError::InputWorkerDisconnected { .. }
            ));
            true
        },
    );

    assert!(result.is_ok());
    assert!(recovery_called.get());
}

#[test]
fn fatal_runtime_health_failure_preserves_detailed_error() {
    let result = route_runtime_health_failure(
        SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 901 },
        |_| false,
    );

    assert!(matches!(
        result,
        Err(SwitcherError::VirtualKeyboardWriterTransactionTimedOut { request_id: 901 })
    ));
}
```

- [ ] **Шаг 2: наблюсти ожидаемый RED**

```bash
cargo test --lib runtime_health_failure -- --nocapture
```

Ожидается ошибка компиляции только из-за отсутствия
`route_runtime_health_failure`.

- [ ] **Шаг 3: реализовать testable routing boundary**

Рядом с существующими generic health helpers добавить:

```rust
fn route_runtime_health_failure<E>(
    error: E,
    recover: impl FnOnce(&E) -> bool,
) -> Result<(), E> {
    if recover(&error) {
        Ok(())
    } else {
        Err(error)
    }
}
```

- [ ] **Шаг 4: расширить service health gate без изменения writer policy**

Добавить:

```rust
fn ensure_input_backend_healthy(&mut self) -> Result<(), SwitcherError> {
    self.ensure_writer_healthy()?;
    if let Some(error) = self
        .keyboard
        .as_ref()
        .and_then(KeyboardController::input_worker_health_error)
    {
        log_input_debug("input-worker-health-error", &format!("error={error}"));
        return Err(error);
    }
    Ok(())
}
```

`poll_manual_completion_and_ensure_writer_healthy` переименовать в
`poll_manual_completion_and_ensure_input_backend_healthy` и после completion
вызывать новую общую проверку. Эту функцию продолжать использовать:

- до fetch;
- после fetch;
- перед каждым событием batch.

Во всех трёх ветвях `WriterHealthyOperationError::Health`,
`WriterHealthyBatchError::Health` и верхней pre-fetch health-проверке выполнить:

```rust
route_runtime_health_failure(error, |error| {
    self.handle_runtime_input_failure(error)
})?;
continue 'event_loop;
```

Для верхней ветви добавить label `'event_loop`. После успешной обработки
recoverable health-ошибки нельзя продолжать старые `events`; следующая итерация
работает уже без backend. Fatal writer error возвращается неизменённой.

- [ ] **Шаг 5: наблюсти GREEN и защиту batch ordering**

```bash
cargo test --lib runtime_health_failure -- --nocapture
cargo test --lib writer_health -- --nocapture
cargo test --lib completion_gate -- --nocapture
cargo test --lib batch_gate -- --nocapture
cargo test --lib runtime_input_worker -- --nocapture
```

Ожидается PASS.

- [ ] **Шаг 6: проверить формат и зафиксировать service wiring**

```bash
cargo fmt --check
git diff --check
git add src/daemon/service.rs
git commit -m "fix: release input backend when required worker dies"
```

### Задача 5: Полная локальная проверка

**Файлы:** production-файлы не добавлять.

- [ ] **Шаг 1: targeted регрессии**

```bash
cargo test --lib input_worker -- --nocapture
cargo test --lib input_target -- --nocapture
cargo test --lib input_backend -- --nocapture
cargo test --lib writer_health -- --nocapture
cargo test --lib corrected_word_commit_state_for_enter -- --nocapture
cargo test --lib corrected_word_commit_state_for_tab -- --nocapture
cargo test --lib corrected_word_commit_state_for_space -- --nocapture
```

- [ ] **Шаг 2: обе полные матрицы последовательно**

```bash
cargo test --lib -- --test-threads=1
cargo test --lib --features settings-ui -- --test-threads=1
```

- [ ] **Шаг 3: статические границы diff**

```bash
cargo fmt --check
git diff --check
git diff --stat 625c0c9..HEAD
git diff 625c0c9..HEAD -- src/daemon/service.rs src/daemon/input_backend.rs src/daemon/keyboard.rs
rg -n "unsafe\s*\{" src/daemon/keyboard.rs src/daemon/input_backend.rs src/daemon/service.rs
```

Ожидается: нет нового `unsafe`, нет изменений параметров коррекции/clipboard,
diff ограничен заявленным lifecycle.

### Задача 6: Package-first VM-проверка и отчёт

**Файлы:**

- Создать: `docs/audits/2026-07-22-required-input-worker-fail-safe-validation.md`
- Артефакт, не коммитить: `dist/packages/open-switcher_0.1.0-1_amd64.deb`

- [ ] **Шаг 1: собрать и идентифицировать Debian-пакет**

```bash
./manage.sh package deb
sha256sum dist/packages/open-switcher_0.1.0-1_amd64.deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-1_amd64.deb
dpkg-deb --fsys-tarfile dist/packages/open-switcher_0.1.0-1_amd64.deb \
  | tar -xOf - ./usr/bin/open-switcher-daemon \
  | sha256sum
```

- [ ] **Шаг 2: установить пакет в сохранённую Mint/Cinnamon X11 VM**

Использовать существующую VM `mint-install-v1`, SSH `127.0.0.1:22223`, ключ
`/home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519` и `known_hosts` лаборатории.
Передать пакет в `/tmp`, установить внутри гостя и подтвердить совпадение hash
`/usr/bin/open-switcher-daemon`. Лабораторию не удалять и не менять сеть хоста.

- [ ] **Шаг 3: повторить безопасную функциональную матрицу**

Проверить 20/20 `ыгвщ -> F12 -> sudo`, движение, scroll, физический и логический
клик, Enter, Tab, пробел, автокоррекцию, две заглавные и обычное переключение.
Отдельный ранее подтверждённый Caps Lock defect не объявлять регрессией этого
изменения, но записать его статус без маскировки.

- [ ] **Шаг 4: повторить bounded stop/start**

Выполнить 10 разнесённых циклов. Каждый stop должен снять grab и завершиться
менее чем за 1 секунду; start должен вернуть `active`, не доводя systemd до
`start-limit-hit`.

- [ ] **Шаг 5: повторить точечную fault injection X11 watcher**

Только внутри гостя временно включить обезличенный input debug. Через уже
проверенный root/gdb-путь вызвать `shutdown(SHUT_RDWR)` только для X11 fd
`input-target-watcher`; не завершать X server и пользовательскую сессию.

Доказать одновременно:

- старый watcher завершился;
- daemon сохранил PID и перешёл в `Recovering`;
- независимый ограниченный `EVIOCGRAB` probe получил реальное окно успеха и
  немедленно освободил устройство;
- повторный grab произошёл только после нового живого X11 watcher;
- daemon вернулся в `Ready` без busy loop.

- [ ] **Шаг 6: проверить startup/retry без X11 endpoint**

Внутри VM кратковременно сделать подключение monitor недоступным, не ломая SSH
и QMP. Пока endpoint недоступен, daemon должен оставаться без grab и повторять
подготовку по backoff. После восстановления X11 pipeline должен автоматически
вернуться в `Ready` с тем же PID.

- [ ] **Шаг 7: вернуть пакет в штатное состояние и оформить отчёт**

Выключить временный debug, убедиться в `active`, рабочем X11 watcher и отсутствии
изменённых отладочных override. Записать commits, hashes, число тестов,
функциональную матрицу, stop latency, fault timeline и остаточные ограничения в
русский отчёт.

```bash
git add -f docs/audits/2026-07-22-required-input-worker-fail-safe-validation.md
git commit -m "docs: validate required input worker recovery"
```

## Критерии готовности

- Обязательный watcher не может умереть незаметно при активном grab.
- `InputWorkerDisconnected` переводит backend в `Recovering` и быстро снимает
  grab через существующий shutdown order.
- Daemon сохраняет PID и автоматически возвращает полный backend после
  восстановления зависимостей.
- В X11 подключение active-window monitor обязательно до grab; non-X11 policy
  не меняется.
- Fatal writer/transaction policy, ввод, коррекция, клики и clipboard не
  изменились.
- Локальные матрицы и package-first VM fault injection зелёные.
- VM-лаборатория сохранена; её удаление не выполняется без прямой просьбы
  пользователя.
