# Прерываемое событийное ожидание X11 — план реализации

> **Для агента-исполнителя:** ОБЯЗАТЕЛЬНЫЙ НАВЫК — выполнять этот план через `superpowers:executing-plans` по одному пункту с контрольными проверками. Работа выполняется inline; `superpowers:subagent-driven-development` допустим только после прямой просьбы пользователя.

**Цель:** убрать постоянный 5-миллисекундный опрос X11, не возвращая гонку первой клавиши в новом окне и не задерживая остановку daemon.

**Архитектура:** выделенный X11-worker сначала полностью извлекает уже буферизованные события x11rb, затем блокируется в безопасной обёртке `poll(2)` одновременно на X11 fd и приватном `UnixStream` остановки. Остановка устанавливает атомарный флаг, закрывает пишущую половину wakeup-канала и выполняет `join`. Изменение независимо от классификации кликов и может быть целиком откатано с сохранением исправления указателя.

**Технологии:** Rust 2021, `x11rb` 0.13.2, `nix` 0.26.4 feature `poll`, `UnixStream::pair`, Cargo-тесты, Debian-пакет, сохранённая Mint/Cinnamon X11 VM.

---

## Предусловие

Начинать только после полного принятия плана `2026-07-22-pointer-context-invalidation.md`. Его итоговый commit и deb с рабочим 5-мс опросом становятся контрольной точкой A. Если этот план B не проходит хотя бы один критерий, откатывается только B, а контрольная точка A остаётся рабочим результатом.

## Карта файлов

- Изменить `Cargo.toml`: добавить прямую безопасную зависимость `nix` ровно версии линии, уже присутствующей в lockfile.
- Изменить `Cargo.lock`: зафиксировать прямую зависимость без необоснованного обновления пакетов.
- Создать `src/daemon/x11_wait.rs`: безопасное ожидание двух fd и модульные тесты readiness/EOF/EINTR.
- Изменить `src/daemon/mod.rs`: зарегистрировать `x11_wait`.
- Изменить `src/daemon/keyboard.rs`: wakeup lifecycle, drain-before-wait, удаление 5-мс sleep и устаревшего interval-теста.
- Создать `docs/audits/2026-07-22-wakeable-x11-watcher-validation.md`: сравнительные результаты пакетов A и B, stop/restart, отказ X11 и решение принять либо откатить B.

## Жёсткие границы

- Не менять смысл событий указателя из принятого изменения A.
- Не менять `_NET_ACTIVE_WINDOW`, задержки коррекции, раскладку, XTest replay или `DaemonService`.
- Не использовать непрерываемый `wait_for_event()`.
- Не читать одно X11-соединение из двух потоков.
- Не добавлять unsafe-код проекта: системный `poll` вызывается через безопасный API `nix`.
- Не обновлять `nix`, `x11rb` или другие зависимости сверх необходимого.
- Не принимать B только по unit-тестам: основной критерий — установленный deb в Mint VM.
- Не удалять VM-лабораторию.

### Задача 0: Сохранить контрольную точку A до любых изменений B

**Файлы:**

- Артефакт, не коммитить: `dist/packages/baselines/open-switcher_0.1.0-1_pointer-a_amd64.deb`

- [ ] **Шаг 1: убедиться, что изменение A принято и worktree чист**

```bash
git status --short
git log -1 --oneline
```

Ожидаемый результат: нет незакоммиченных изменений кода; последний отчёт подтверждает пакет A. Если отчёт A ещё не принят, B не начинать.

- [ ] **Шаг 2: собрать и сохранить deb A под отдельным именем**

```bash
./manage.sh package deb
mkdir -p dist/packages/baselines
install -m 0644 dist/packages/open-switcher_0.1.0-1_amd64.deb dist/packages/baselines/open-switcher_0.1.0-1_pointer-a_amd64.deb
sha256sum dist/packages/baselines/open-switcher_0.1.0-1_pointer-a_amd64.deb
```

Записать commit и SHA-256 в будущий отчёт B. Этот файл нужен только для сравнения и не добавляется в Git.

### Задача 1: Безопасное ожидание X11 fd и сигнала остановки

**Файлы:**

- Изменить: `Cargo.toml`
- Изменить: `Cargo.lock`
- Создать: `src/daemon/x11_wait.rs`
- Изменить: `src/daemon/mod.rs`

- [ ] **Шаг 1: добавить минимальную зависимость без обновления дерева**

```toml
nix = { version = "0.26.4", default-features = false, features = ["poll"] }
```

Выполнить минимальное локальное разрешение уже закэшированной зависимости:

```bash
cargo check --offline --lib
git diff -- Cargo.toml Cargo.lock
```

Ожидаемый результат: используется `nix 0.26.4`; в записи корневого пакета появляется прямая зависимость на уже существующий `nix 0.26.4`, а другие версии пакетов не обновлены.

- [ ] **Шаг 2: зарегистрировать модуль и написать RED-тесты**

В `src/daemon/mod.rs` добавить:

```rust
pub(crate) mod x11_wait;
```

В новом файле объявить тесты с `UnixStream::pair()` до реализации:

```rust
#[test]
fn x11_readiness_returns_x11_ready() { /* byte waiting on X11 pair */ }

#[test]
fn stop_readiness_returns_stop_requested() { /* shutdown write side */ }

#[test]
fn stop_wins_when_both_descriptors_are_ready() { /* both ready */ }

#[test]
fn x11_hangup_is_an_error() { /* dropped X11 peer */ }

#[test]
fn stop_hangup_is_a_stop_request() { /* dropped stop peer */ }

#[test]
fn interrupted_poll_is_retried() { /* first call EINTR, then ready fd */ }
```

Каждый readiness-тест делает fd готовым до вызова, чтобы ошибочная реализация не могла повиснуть в тесте навсегда. Для `EINTR` использовать приватный generic helper с внедряемым вызовом `poll`: первый вызов возвращает `Errno::EINTR`, второй выполняет настоящий неблокирующий `poll` уже готового fd. Production-функция передаёт в этот helper обычный `nix::poll::poll`.

- [ ] **Шаг 3: подтвердить RED**

```bash
cargo test --lib x11_wait -- --nocapture
```

Ожидаемый результат: ошибка компиляции из-за отсутствия `wait_for_x11_or_stop()` и `X11WaitOutcome`.

- [ ] **Шаг 4: реализовать безопасную обёртку**

Публичная внутри crate граница:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum X11WaitOutcome {
    X11Ready,
    StopRequested,
}

pub(crate) fn wait_for_x11_or_stop(
    x11_fd: RawFd,
    stop_fd: RawFd,
) -> io::Result<X11WaitOutcome>;
```

Алгоритм:

1. Создать два `PollFd` с `POLLIN`.
2. Вызвать `nix::poll::poll(&mut fds, -1)`.
3. При `Errno::EINTR` повторить ожидание.
4. `POLLIN | POLLHUP | POLLERR | POLLNVAL` на stop fd всегда возвращает `StopRequested` и имеет приоритет, если готовы оба fd.
5. `POLLHUP | POLLERR | POLLNVAL` на X11 fd возвращает `io::ErrorKind::BrokenPipe`.
6. `POLLIN` на X11 fd возвращает `X11Ready`.
7. Неизвестный пустой результат повторяет ожидание, а не создаёт busy loop.

- [ ] **Шаг 5: подтвердить GREEN**

```bash
cargo test --lib x11_wait -- --nocapture
cargo fmt --check
```

Ожидаемый результат: все шесть тестов проходят, unsafe отсутствует в новом модуле.

- [ ] **Шаг 6: зафиксировать инфраструктуру отдельным коммитом**

```bash
git add Cargo.toml Cargo.lock src/daemon/mod.rs src/daemon/x11_wait.rs
git commit -m "feat: add wakeable X11 descriptor wait"
```

### Задача 2: Прерываемый lifecycle `InputTargetWatcher`

**Файлы:**

- Изменить: `src/daemon/keyboard.rs`

- [ ] **Шаг 1: написать RED-тесты канала остановки**

Вынести создание/сигнал остановки в маленький тип либо функции, чтобы без X-сервера проверить:

```rust
#[test]
fn input_target_stop_signal_wakes_idle_waiter() { /* recv side becomes ready */ }

#[test]
fn repeated_input_target_stop_is_idempotent() { /* no panic, no block */ }
```

Отдельный тест существующих прямых конструкторов должен подтвердить, что disabled watcher не требует wakeup fd и остаётся ready.

- [ ] **Шаг 2: подтвердить RED**

```bash
cargo test --lib input_target_stop -- --nocapture
```

Ожидаемый результат: тесты не компилируются до появления wakeup-состояния.

- [ ] **Шаг 3: добавить wakeup pair во время spawn**

До `thread::spawn` создать `UnixStream::pair()`. При успешном запуске:

- `InputTargetWatcher` владеет `Option<UnixStream>` отправляющей стороны;
- worker единолично владеет принимающей стороной;
- disabled watcher хранит `None`;
- worker не читает X11 через stop-канал и не передаёт X11-соединение другому потоку.

- [ ] **Шаг 4: сделать остановку прерываемой и конечной**

Порядок `stop()`:

1. `stop_flag.store(true, Ordering::SeqCst)`;
2. сигнализировать wakeup через `shutdown(Shutdown::Write)` либо эквивалентную неблокирующую операцию;
3. забрать `handle` через `take()`;
4. выполнить `join()`;
5. повторный `stop()` безопасен.

В пути ошибки `wait_for_input_worker_startup_ready()` нельзя оставлять detached thread: установить stop, сигнализировать fd и выполнить `join` перед возвратом ошибки.

- [ ] **Шаг 5: обновить все readiness-конструкторы**

Прямые тестовые литералы `InputTargetWatcher` получают `stop_wakeup: None`. Не ослаблять `required/alive`-политику: смерть обязательного X11-worker по-прежнему делает keyboard controller неготовым.

- [ ] **Шаг 6: подтвердить lifecycle-тесты**

```bash
cargo test --lib input_target_stop -- --nocapture
cargo test --lib input_target_watcher_readiness -- --nocapture
cargo fmt --check
```

- [ ] **Шаг 7: зафиксировать lifecycle отдельным коммитом**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: make X11 watcher shutdown wakeable"
```

### Задача 3: Drain-before-wait и удаление 5-мс polling

**Файлы:**

- Изменить: `src/daemon/keyboard.rs`

- [ ] **Шаг 1: написать RED-тест состояния цикла**

Вынести один цикл в generic helper с тремя операциями: `next_event`, `handle_event`, `wait`. Тест должен записывать порядок вызовов:

```rust
#[test]
fn buffered_x11_events_are_drained_before_fd_wait() {
    // next_event: Some(event), затем None
    // ожидаемый порядок: next, handle, next, wait
}

#[test]
fn stop_observed_after_drain_skips_fd_wait() {
    // handler устанавливает stop; wait closure не должен вызываться
}
```

Helper возвращает `Ok(true)` для `X11Ready`, `Ok(false)` для `StopRequested` и передаёт ошибку ожидания наверх. Он не владеет соединением и не создаёт дополнительный поток.

- [ ] **Шаг 2: подтвердить RED**

```bash
cargo test --lib x11_event_cycle -- --nocapture
```

Ожидаемый результат: ошибка компиляции до реализации helper.

- [ ] **Шаг 3: открыть fd выделенного соединения только для ожидания**

Добавить в `ActiveWindowMonitor`:

```rust
fn connection_fd(&self) -> RawFd {
    self.conn.stream().as_raw_fd()
}
```

Импортировать `AsRawFd`. Все `poll_for_event()`, property query и обработка XInput остаются в том же worker; fd нигде не читается напрямую.

- [ ] **Шаг 4: заменить sleep на событийный цикл**

В каждой итерации worker:

1. вызывать `poll_context_event()` до `None` и обработать все события;
2. проверить `stop_flag` ещё раз после drain;
3. вызвать `wait_for_x11_or_stop(x11_fd, stop_fd)`;
4. при `X11Ready` продолжить drain, при `StopRequested` выйти;
5. при ошибке записать ограниченный лог и завершить обязательный worker, чтобы существующий health-check освободил input backend.

Только после GREEN удалить:

- `INPUT_TARGET_POLL_INTERVAL`;
- `thread::sleep(INPUT_TARGET_POLL_INTERVAL)`;
- тест `input_target_poll_interval_stays_below_first_key_race_budget`;
- комментарий, который оправдывает 5-мс polling.

`POINTER_POLL_INTERVAL` и pointer watcher не менять.

- [ ] **Шаг 5: запустить GREEN-тесты цикла и watcher**

```bash
cargo test --lib x11_event_cycle -- --nocapture
cargo test --lib x11_wait -- --nocapture
cargo test --lib input_target -- --nocapture
cargo fmt --check
```

Ожидаемый результат: порядок drain-before-wait доказан; остановка не ждёт события X11; старой 5-мс константы нет.

- [ ] **Шаг 6: проверить отсутствие polling и unsafe**

```bash
rg -n "INPUT_TARGET_POLL_INTERVAL|sleep\(INPUT_TARGET" src/daemon
rg -n "unsafe" src/daemon/x11_wait.rs src/daemon/keyboard.rs
```

Ожидаемый результат: первый поиск ничего не находит; второй не показывает новый unsafe-код этой работы.

- [ ] **Шаг 7: зафиксировать замену механизма ожидания**

```bash
git add src/daemon/keyboard.rs
git commit -m "perf: wait for X11 events without polling"
```

### Задача 4: Полная безопасная локальная проверка

**Файлы:**

- Проверить весь Rust-код без изменения устройств хоста.

- [ ] **Шаг 1: прогнать фокусные регрессии изменения A**

```bash
cargo test --lib pointer_click -- --nocapture
cargo test --lib corrected_word_commit_state_for_enter -- --nocapture
cargo test --lib corrected_word_commit_state_for_tab -- --nocapture
cargo test --lib corrected_word_commit_state_for_space -- --nocapture
cargo test --lib wayland_focus_switch_policy -- --nocapture
```

- [ ] **Шаг 2: прогнать полную библиотечную матрицу**

```bash
cargo test --lib
cargo test --features settings-ui --lib
cargo fmt --check
```

Ожидаемый результат: все команды завершаются с кодом 0. Не запускать daemon или тест, который открывает физический `/dev/input`, создаёт `/dev/uinput`, посылает реальные клавиши либо меняет clipboard/раскладку/systemd хоста.

- [ ] **Шаг 3: проверить область diff**

```bash
git status --short
git show --stat --oneline HEAD~3..HEAD
```

Ожидаемый результат: `src/daemon/service.rs` и параметры коррекции не изменены; diff ограничен заявленными файлами.

### Задача 5: Сравнить пакет A и пакет B в Mint VM

**Файлы:**

- Создать: `docs/audits/2026-07-22-wakeable-x11-watcher-validation.md`
- Артефакты, не коммитить: deb контрольной точки A и текущий deb B.

- [ ] **Шаг 1: проверить сохранённый до реализации B пакет A**

Использовать `dist/packages/baselines/open-switcher_0.1.0-1_pointer-a_amd64.deb`, созданный в задаче 0. Сверить его SHA-256 с записанным значением. Если файл или checksum потерян, не восстанавливать его destructive Git-командами: создать отдельный временный worktree на commit A, собрать пакет там и снова записать checksum.

- [ ] **Шаг 2: собрать пакет B**

```bash
./manage.sh package deb
sha256sum dist/packages/open-switcher_0.1.0-1_amd64.deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-1_amd64.deb
```

- [ ] **Шаг 3: запустить сохранённую Mint VM и установить пакет**

VM запускается командой из worktree `vm-lab`:

```bash
python3 -m tools.vm_lab.session mint-installed
```

Передача и установка выполняются теми же `scp`/`ssh` командами, что в плане A, через порт `22223`, с ключом `/home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519` и отдельным `known_hosts` лаборатории.

- [ ] **Шаг 4: проверить отсутствие регрессии первой клавиши**

На установленном пакете B не менее 20 раз повторить:

1. открыть или сфокусировать новое окно;
2. немедленно набрать первое слово;
3. сразу вызвать F12;
4. убедиться, что преобразуется всё слово, а не хвост.

Обязательно включить известный сценарий `ыгвщ` → F12 → `sudo`. Зафиксировать число успешных повторов и любые отклонения; единичная частичная коррекция означает отказ от B, а не подбор нового timeout.

- [ ] **Шаг 5: повторить функциональную матрицу A**

Проверить движение без клика, прокрутку, физические и логические клики, Enter, Tab, пробел, обычное переключение, автокоррекцию, Caps Lock и две заглавные. Результат должен совпасть с контрольным пакетом A.

- [ ] **Шаг 6: проверить stop/restart при полностью бездействующем X11**

```bash
ssh -p 22223 -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts openswitcher@127.0.0.1 'time systemctl --user stop open-switcher-daemon.service; time systemctl --user start open-switcher-daemon.service; systemctl --user is-active open-switcher-daemon.service'
```

Повторить 10 раз без генерации X11-событий. Каждый stop должен завершаться менее чем за 1 секунду, каждый start — возвращать `active`. Таймаут, зависший `join` или оставшийся процесс означает отказ от B.

- [ ] **Шаг 7: проверить отказ X11 только внутри VM**

Через независимый SSH/QMP-канал завершить пользовательскую X11-сессию либо недеструктивно разорвать выделенное X11-соединение доступным в госте способом. До опыта убедиться, что QMP и SSH работают и позволяют перезапустить гостевую сессию.

Ожидаемый результат: watcher не входит в busy loop; daemon обнаруживает потерю обязательного worker и проходит существующий fail-safe shutdown, после чего клавиатура гостя снова доступна. Этот опыт запрещён на хосте.

- [ ] **Шаг 8: сравнить idle CPU A и B**

Для каждого пакета после одинакового прогрева и при отсутствии ввода снять не менее трёх 30-секундных выборок CPU процесса daemon через `/proc/<pid>/stat` или `pidstat`, если он уже установлен в госте. Не устанавливать отдельный тяжёлый profiling stack.

Критерий: B не должен показывать периодические пробуждения с частотой около 200 Гц и должен заметно снизить idle CPU относительно A. Функциональная корректность и остановка имеют приоритет над величиной выигрыша.

- [ ] **Шаг 9: принять либо откатить B**

Принять B можно только если одновременно:

- 20/20 быстрых смен фокуса не дали частичной коррекции;
- stop/restart прошли 10/10 и каждый stop быстрее 1 секунды;
- потеря X11 завершилась fail-safe, без зависания и busy loop;
- вся функциональная матрица A сохранилась;
- idle CPU действительно ниже контрольной точки A.

Если любой критерий не выполнен, откатить только три коммита B обычным `git revert` в обратном порядке, снова прогнать тесты и оставить исправление A с 5-мс polling. Не увеличивать интервал и не маскировать отказ дополнительным timeout.

- [ ] **Шаг 10: оформить и зафиксировать русский отчёт**

В `docs/audits/2026-07-22-wakeable-x11-watcher-validation.md` записать commits и SHA-256 обоих deb, точные результаты повторов, stop latency, CPU-измерения, отказ X11, логи без пользовательского текста и итоговое решение `принято` либо `откачено`.

```bash
git add -f docs/audits/2026-07-22-wakeable-x11-watcher-validation.md
git commit -m "docs: validate wakeable X11 watcher"
```

## Критерии готовности изменения B

- В штатном X11-worker отсутствуют 5-мс таймер и busy loop.
- События x11rb всегда вычитываются до блокировки на fd.
- Остановка будит idle worker и гарантированно доходит до `join`.
- Ошибка X11 сохраняет fail-safe политику обязательного worker.
- Быстрая смена окна не возвращает частичную коррекцию.
- Пакет B проходит ту же функциональную матрицу, что пакет A.
- Фоновая нагрузка ниже контрольного 5-мс варианта.
- При любом сомнении B откатан независимо, а исправление клика A сохранено.
