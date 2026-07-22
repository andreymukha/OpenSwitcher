# Сброс контекста только по настоящему клику — план реализации

> **Для агента-исполнителя:** ОБЯЗАТЕЛЬНЫЙ НАВЫК — выполнять этот план через `superpowers:executing-plans` по одному пункту с контрольными проверками. Работа выполняется inline; `superpowers:subagent-driven-development` допустим только после прямой просьбы пользователя.

**Цель:** перестать сбрасывать последнее набранное слово при движении или касании тачпада, сохранив сброс при физическом либо сформированном X11 логическом клике и не меняя остальные правила коррекции.

**Архитектура:** сырой evdev-канал распознаёт только явный список физических кнопок. X11-наблюдатель дополнительно подписывается через XInput2 на глобальные `RawButtonPress` без захвата указателя. Два атомарных флага безусловно извлекаются и объединяются контроллером; повторное наблюдение одного клика безопасно. Текущий 5-мс опрос X11 на этом этапе сохраняется.

**Технологии:** Rust 2021, `evdev`, `x11rb` 0.13.2 с `xinput`, атомики, Cargo-тесты, Debian-пакет, сохранённая Mint/Cinnamon X11 VM.

---

## Карта файлов

- Изменить `Cargo.toml`: включить feature `xinput` у уже используемого `x11rb`.
- Изменить `Cargo.lock`: только если Cargo действительно изменит lockfile после включения feature.
- Изменить `src/daemon/keyboard.rs`: классификаторы, XInput2-подписка, событие X11 и объединение флагов.
- Не изменять `src/daemon/service.rs`: существующие причины сброса контекста остаются источником истины.
- Создать `docs/audits/2026-07-22-pointer-context-invalidation-validation.md`: фактические результаты локальных и пакетных проверок, ограничения VM и остаточные риски.

## Жёсткие границы

- Не менять `POINTER_POLL_INTERVAL`, `INPUT_TARGET_POLL_INTERVAL`, задержки коррекции, раскладки, XTest replay или runtime snapshot.
- Не распознавать tap-to-click по `BTN_TOUCH`, координатам, времени касания или жестам.
- Не захватывать указатель и не создавать новое устройство ввода на хосте.
- Не менять Enter, Tab, пробел, системные сочетания, смену активного окна и lifecycle-сбросы.
- Не расширять инструменты VM: использовать уже сохранённую лабораторию и доступные QMP/SSH-каналы.
- Не удалять и не перестраивать лабораторию без прямой просьбы пользователя.

### Задача 1: Явный классификатор физических кнопок

**Файлы:**

- Изменить: `src/daemon/keyboard.rs` — `is_pointer_click()` и модуль тестов около текущих watcher-тестов.

- [ ] **Шаг 1: написать RED-тесты принимаемых физических кнопок**

Добавить табличный тест:

```rust
#[test]
fn pointer_click_classifier_accepts_only_physical_pointer_buttons() {
    for key in [
        Key::BTN_LEFT,
        Key::BTN_RIGHT,
        Key::BTN_MIDDLE,
        Key::BTN_SIDE,
        Key::BTN_EXTRA,
        Key::BTN_FORWARD,
        Key::BTN_BACK,
        Key::BTN_TASK,
    ] {
        assert!(is_pointer_click(key), "expected physical button: {key:?}");
    }
}
```

- [ ] **Шаг 2: написать RED-тесты событий, которые не являются кликом**

```rust
#[test]
fn pointer_click_classifier_rejects_touch_tool_and_non_pointer_codes() {
    for key in [
        Key::BTN_TOUCH,
        Key::BTN_TOOL_FINGER,
        Key::BTN_TOOL_DOUBLETAP,
        Key::BTN_TOOL_PEN,
        Key::BTN_STYLUS,
        Key::BTN_0,
        Key::BTN_SOUTH,
    ] {
        assert!(!is_pointer_click(key), "must not be a pointer click: {key:?}");
    }
}
```

- [ ] **Шаг 3: подтвердить падение теста на текущем диапазоне**

Выполнить:

```bash
cargo test --lib pointer_click_classifier -- --nocapture
```

Ожидаемый результат: хотя бы тест с `BTN_TOUCH` или `BTN_TOOL_*` падает, потому что текущий числовой диапазон считает касание кликом.

- [ ] **Шаг 4: заменить диапазон явным списком**

```rust
fn is_pointer_click(key: Key) -> bool {
    matches!(
        key,
        Key::BTN_LEFT
            | Key::BTN_RIGHT
            | Key::BTN_MIDDLE
            | Key::BTN_SIDE
            | Key::BTN_EXTRA
            | Key::BTN_FORWARD
            | Key::BTN_BACK
            | Key::BTN_TASK
    )
}
```

Не сужать `find_pointer_devices()`: тачпад всё ещё должен обнаруживаться, чтобы его настоящая механическая кнопка могла быть прочитана. Меняется только классификация события после открытия устройства.

- [ ] **Шаг 5: запустить GREEN-тест и форматирование**

```bash
cargo test --lib pointer_click_classifier -- --nocapture
cargo fmt --check
```

Ожидаемый результат: оба теста проходят; форматирование чистое.

- [ ] **Шаг 6: зафиксировать самостоятельный коммит**

```bash
git add src/daemon/keyboard.rs
git commit -m "fix: distinguish pointer buttons from touch contact"
```

### Задача 2: Классификация логических кнопок X11

**Файлы:**

- Изменить: `Cargo.toml`
- Возможно изменить: `Cargo.lock`
- Изменить: `src/daemon/keyboard.rs`

- [ ] **Шаг 1: включить только существующий feature XInput2**

Изменить зависимость без обновления версии:

```toml
x11rb = { version = "0.13.2", features = ["allow-unsafe-code", "xinput", "xkb", "xtest"] }
```

- [ ] **Шаг 2: написать RED-тесты X11 detail-классификатора**

```rust
#[test]
fn x11_pointer_click_classifier_accepts_primary_middle_secondary_and_navigation() {
    for detail in [1, 2, 3, 8, 9] {
        assert!(is_x11_pointer_click(detail), "detail={detail}");
    }
}

#[test]
fn x11_pointer_click_classifier_rejects_scroll_and_unknown_buttons() {
    for detail in [0, 4, 5, 6, 7, 10] {
        assert!(!is_x11_pointer_click(detail), "detail={detail}");
    }
}
```

Значения 4–7 — колёса/прокрутка и не должны сбрасывать слово. Флаг эмуляции события намеренно не проверяется: tap-to-click, который X11 уже признал кнопкой 1–3, должен учитываться.

- [ ] **Шаг 3: подтвердить RED**

```bash
cargo test --lib x11_pointer_click_classifier -- --nocapture
```

Ожидаемый результат: ошибка компиляции, потому что `is_x11_pointer_click()` ещё не существует.

- [ ] **Шаг 4: добавить минимальный чистый классификатор**

```rust
fn is_x11_pointer_click(detail: u32) -> bool {
    matches!(detail, 1 | 2 | 3 | 8 | 9)
}
```

- [ ] **Шаг 5: подтвердить GREEN и отсутствие неожиданного обновления зависимостей**

```bash
cargo test --lib x11_pointer_click_classifier -- --nocapture
git diff -- Cargo.toml Cargo.lock
```

Ожидаемый результат: тесты проходят; версия `x11rb` не меняется. Если lockfile изменился, в нём допустимы только следствия включения feature, без обновления версий.

### Задача 3: Необязательная XInput2-подписка и единый поток событий X11

**Файлы:**

- Изменить: `src/daemon/keyboard.rs` — `ActiveWindowMonitor`, `InputTargetWatcher`, `KeyboardController` и watcher-тесты.

- [ ] **Шаг 1: ввести тип события наблюдателя**

Добавить рядом с `ActiveWindowMonitor`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum X11ContextEvent {
    ActiveWindowChanged {
        previous: Option<u32>,
        current: Option<u32>,
    },
    PointerClick {
        detail: u32,
    },
}
```

Переименовать `poll_change()` в `poll_context_event()`. Существующая ветвь `_NET_ACTIVE_WINDOW` возвращает `ActiveWindowChanged`; `Event::XinputRawButtonPress(event)` возвращает `PointerClick` только при `is_x11_pointer_click(event.detail)`. Все остальные события продолжают извлекаться и игнорироваться.

- [ ] **Шаг 2: подписаться на XInput2 без grab**

После установки `PROPERTY_CHANGE` вызвать отдельный helper, который:

```rust
use x11rb::protocol::xinput::{
    ConnectionExt as _, Device, EventMask as XiEventMask, XIEventMask,
};

conn.xinput_xi_query_version(2, 0)?.reply()?;
conn.xinput_xi_select_events(
    root,
    &[XiEventMask {
        deviceid: Device::ALL_MASTER.into(),
        mask: vec![XIEventMask::RAW_BUTTON_PRESS],
    }],
)?.check()?;
conn.flush()?;
```

Ошибку query/select нельзя возвращать из `ActiveWindowMonitor::connect()`: записать ограниченную диагностическую строку и продолжить наблюдение `_NET_ACTIVE_WINDOW`. Ошибка базового X11-соединения или `PROPERTY_CHANGE` остаётся прежней ошибкой подключения.

- [ ] **Шаг 3: добавить отдельный X11-флаг клика**

Добавить `pointer_click_flag: Arc<AtomicBool>` в `InputTargetWatcher`. В worker:

- `ActiveWindowChanged` устанавливает только `changed_flag` и сохраняет прежний лог;
- `PointerClick` устанавливает только `pointer_click_flag` и пишет ограниченный лог с `detail`;
- цикл по-прежнему полностью вычитывает очередь и при её пустоте спит `INPUT_TARGET_POLL_INTERVAL`.

Обновить `disabled()` и все прямые test-конструкторы `InputTargetWatcher`, чтобы новый флаг существовал и в отключённом состоянии.

- [ ] **Шаг 4: написать RED-тест безусловного извлечения двух источников**

Вынести только атомарную операцию в небольшой helper и проверить обе стороны:

```rust
#[test]
fn pointer_click_invalidation_drains_both_sources() {
    let physical = AtomicBool::new(true);
    let logical = AtomicBool::new(true);

    assert!(take_pointer_click_flags(&physical, &logical));
    assert!(!physical.load(Ordering::SeqCst));
    assert!(!logical.load(Ordering::SeqCst));
    assert!(!take_pointer_click_flags(&physical, &logical));
}
```

Production helper обязан сначала выполнить оба `swap(false, SeqCst)`, а уже затем объединить значения. Нельзя писать короткое `physical.swap(...) || logical.swap(...)`: short-circuit оставит второй флаг установленным.

- [ ] **Шаг 5: использовать helper в контроллере**

`KeyboardController::take_pointer_click_invalidation()` извлекает флаги `PointerWatcher` и `InputTargetWatcher` одним вызовом helper. Два наблюдения одного клика дают один логический результат в текущей итерации и не оставляют второй флаг для следующей.

- [ ] **Шаг 6: запустить фокусные тесты**

```bash
cargo test --lib pointer_click -- --nocapture
cargo test --lib input_target_watcher_readiness -- --nocapture
cargo fmt --check
```

Ожидаемый результат: классификаторы, объединение и существующие readiness-тесты проходят.

- [ ] **Шаг 7: зафиксировать X11-часть отдельным коммитом**

```bash
git add Cargo.toml Cargo.lock src/daemon/keyboard.rs
git commit -m "fix: observe logical pointer clicks on X11"
```

Если `Cargo.lock` не изменился, не добавлять его искусственно.

### Задача 4: Доказать сохранность прочих правил контекста

**Файлы:**

- Проверить без изменения: `src/daemon/service.rs`
- Проверить: `src/daemon/keyboard.rs`

- [ ] **Шаг 1: запустить точные регрессионные тесты клавиатурного контекста**

```bash
cargo test --lib corrected_word_commit_state_for_enter_invalidates_context_and_replays_separator -- --nocapture
cargo test --lib corrected_word_commit_state_for_tab_invalidates_context_and_replays_separator -- --nocapture
cargo test --lib corrected_word_commit_state_for_space_updates_context_and_replays_separator -- --nocapture
cargo test --lib wayland_focus_switch_policy -- --nocapture
```

Ожидаемый результат: Enter и Tab сбрасывают контекст, пробел сохраняет своё специальное поведение, Wayland focus policy не меняется.

- [ ] **Шаг 2: подтвердить, что `service.rs` не менялся в этой работе**

```bash
git status --short
git show --stat --oneline HEAD~2..HEAD
```

Ожидаемый результат: два реализационных коммита затрагивают только заявленные файлы; `src/daemon/service.rs` отсутствует в их статистике.

- [ ] **Шаг 3: выполнить безопасную локальную матрицу**

```bash
cargo test --lib
cargo test --features settings-ui --lib
cargo fmt --check
```

Ожидаемый результат: все команды завершаются с кодом 0. Эти тесты не должны открывать `/dev/input`, создавать `/dev/uinput`, менять clipboard, раскладку или systemd хоста.

### Задача 5: Проверить установленный Debian-пакет в Mint VM

**Файлы:**

- Создать: `docs/audits/2026-07-22-pointer-context-invalidation-validation.md`
- Артефакт, не коммитить: `dist/packages/open-switcher_0.1.0-1_amd64.deb`

- [ ] **Шаг 1: собрать основной продукт в виде пакета**

Из remediation-worktree:

```bash
./manage.sh package deb
sha256sum dist/packages/open-switcher_0.1.0-1_amd64.deb
dpkg-deb --info dist/packages/open-switcher_0.1.0-1_amd64.deb
```

Ожидаемый результат: пакет собран, checksum и метаданные записаны в отчёт.

- [ ] **Шаг 2: запустить сохранённую Mint VM**

Из worktree `vm-lab`:

```bash
python3 -m tools.vm_lab.session mint-installed
```

Не создавать новую VM и не удалять `/home/andrey/VMs/OpenSwitcherLab`.

- [ ] **Шаг 3: передать и установить именно собранный пакет**

```bash
scp -P 22223 -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts /home/andrey/Projects/OpenSwitcher/.worktrees/audit-remediation/dist/packages/open-switcher_0.1.0-1_amd64.deb openswitcher@127.0.0.1:/tmp/open-switcher_0.1.0-1_amd64.deb
ssh -p 22223 -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts openswitcher@127.0.0.1 'sudo dpkg -i /tmp/open-switcher_0.1.0-1_amd64.deb && systemctl --user restart open-switcher-daemon.service && systemctl --user is-active open-switcher-daemon.service'
```

Ожидаемый результат: `active`. Проверить через `dpkg-query`, что установлен пакет `open-switcher` версии `0.1.0-1`.

- [ ] **Шаг 4: выполнить функциональную матрицу в Cinnamon X11**

Через существующий QMP/графический канал VM, не добавляя универсальный automation framework:

1. В новом окне набрать `ыгвщ`, сразу нажать F12 и убедиться, что исправлено всё слово.
2. Набрать слово, выполнить только движение указателя, затем F12 — слово должно исправиться целиком.
3. Набрать слово, выполнить прокрутку, затем F12 — контекст должен сохраниться.
4. Набрать слово, нажать основную, среднюю, вторичную или навигационную кнопку, затем F12 — старое слово исправляться не должно.
5. При включённом tap-to-click выполнить настоящий системный tap и проверить такой же сброс.
6. Повторить обычное переключение раскладки, автокоррекцию, исправление Caps Lock и двух заглавных букв.
7. Проверить Enter, Tab и специальное поведение пробела на установленном пакете.

QMP-указатель не является настоящим тачпадом. Если сохранённая VM не предоставляет `BTN_TOUCH`/tap-to-click, не выдавать шаг 5 за пройденный: подтвердить логический X11-клик доступным XTest/QMP-событием, а реальное касание оставить явно отмеченной ручной проверкой на оборудовании пользователя.

- [ ] **Шаг 5: проверить логи и здоровье службы**

```bash
ssh -p 22223 -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts openswitcher@127.0.0.1 'systemctl --user is-active open-switcher-daemon.service; journalctl --user -u open-switcher-daemon.service -b --no-pager -n 250'
```

Ожидаемый результат: служба активна; нет panic, смерти обязательного watcher, захвата указателя или повторного запуска daemon. В отчёт не копировать набранный пользователем текст и другие чувствительные данные.

- [ ] **Шаг 6: оформить русский отчёт проверки**

В `docs/audits/2026-07-22-pointer-context-invalidation-validation.md` записать:

- commit и SHA-256 проверенного deb;
- локальные команды и результаты;
- фактически выполненные VM-сценарии;
- отдельно сценарии, которые VM технически не могла воспроизвести;
- подтверждение, что 5-мс опрос пока сохранён;
- остаточный риск Wayland и необходимость наблюдения на реальном тачпаде пользователя.

- [ ] **Шаг 7: зафиксировать только отчёт**

```bash
git add -f docs/audits/2026-07-22-pointer-context-invalidation-validation.md
git commit -m "docs: validate pointer context invalidation"
```

## Критерии готовности изменения A

- `BTN_TOUCH`, `BTN_TOOL_*`, движение, прокрутка и жесты не сбрасывают слово.
- Физические кнопки и X11-кнопки 1, 2, 3, 8, 9 сбрасывают слово.
- XInput2 недоступен — смена активного окна продолжает отслеживаться.
- Enter, Tab, пробел и прочие прежние причины не изменены.
- Новый deb установлен и проверен в Mint/Cinnamon X11.
- Ограничения реального tap-to-click честно отмечены, если VM не дала его воспроизвести.
- `INPUT_TARGET_POLL_INTERVAL` всё ещё равен 5 мс; оптимизация ожидания выполняется только следующим независимым планом.
