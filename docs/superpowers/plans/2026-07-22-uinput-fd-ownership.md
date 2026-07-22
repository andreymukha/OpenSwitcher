# Закрытие `/dev/uinput` fd при recovery — план реализации

> **Для agentic workers:** REQUIRED SUB-SKILL: использовать
> `superpowers:subagent-driven-development` (рекомендуется) либо
> `superpowers:executing-plans` и выполнять задачи по отмечаемым пунктам.
> Для этой работы пользователь ранее выбрал inline-выполнение через
> `superpowers:executing-plans`.

**Цель:** гарантировать закрытие каждого принадлежащего OpenSwitcher
`/dev/uinput` fd на успешных и ошибочных путях создания/уничтожения виртуального
устройства, не меняя пользовательскую логику ввода.

**Архитектура:** точная копия используемого `uinput 0.1.3` фиксируется локально
через `[patch.crates-io]`. `Builder` владеет fd до успешной передачи в `Device`;
оба типа закрывают принадлежащий fd через `Drop`, а `Device` перед закрытием
сохраняет прежний `UI_DEV_DESTROY`.

**Технологии:** Rust 2015/2021, Cargo path patch, `libc`, `nix 0.10`, unit-тесты
на pipe fd, Debian package, Mint/Cinnamon X11 VM.

---

## Структура изменения

- `vendor/uinput-0.1.3/` — неизменённая исходная версия зависимости и два
  локальных RAII-исправления в `Builder`/`Device` с изолированными тестами.
- `Cargo.toml` — локальный `[patch.crates-io]` без изменения публичной версии.
- `Cargo.lock` — фиксирует path-подмену `uinput 0.1.3`.
- `docs/audits/2026-07-22-required-input-worker-fail-safe-validation.md` —
  доказательства RED/GREEN, package hash и VM recovery без роста fd.

`src/daemon/keyboard.rs`, обработка клавиш, тайминги, X11, раскладки, clipboard
и systemd units этой задачей не меняются.

### Задача 1: Зафиксировать исходную зависимость без изменения поведения

**Файлы:**

- Создать: `vendor/uinput-0.1.3/**`
- Изменить: `Cargo.toml`
- Изменить: `Cargo.lock`

- [ ] **Шаг 1: скопировать точный source `uinput 0.1.3`**

Механически скопировать содержимое локального Cargo registry:

```bash
cp -a \
  /home/andrey/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/uinput-0.1.3 \
  vendor/uinput-0.1.3
```

До RAII-задач не менять код зависимости.

- [ ] **Шаг 2: подключить локальную копию**

В конец `Cargo.toml` добавить:

```toml
[patch.crates-io]
uinput = { path = "vendor/uinput-0.1.3" }
```

Обновить lockfile без сети:

```bash
cargo update -p uinput --offline
```

- [ ] **Шаг 3: подтвердить точную выбранную зависимость**

```bash
cargo tree -i uinput --offline
cargo check --lib --offline
```

Ожидается `uinput v0.1.3 (/.../vendor/uinput-0.1.3)` и успешная проверка
OpenSwitcher без предупреждений, внесённых этой задачей.

- [ ] **Шаг 4: зафиксировать воспроизводимую dependency boundary**

```bash
git add -f vendor/uinput-0.1.3 Cargo.toml Cargo.lock
git commit -m "build: vendor uinput dependency"
```

На этом коммите локальный source должен быть функционально равен registry
source.

### Задача 2: Закрывать fd виртуального `Device`

**Файлы:**

- Изменить: `vendor/uinput-0.1.3/src/device/device.rs`

- [ ] **Шаг 1: добавить RED-тест на безопасном pipe fd**

В конец `device.rs` добавить:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn pipe_write_fd() -> c_int {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::close(fds[0]) }, 0);
        fds[1]
    }

    fn fd_is_open(fd: c_int) -> bool {
        unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
    }

    #[test]
    fn device_drop_closes_owned_fd() {
        let fd = pipe_write_fd();
        drop(Device::new(fd));

        let still_open = fd_is_open(fd);
        if still_open {
            assert_eq!(unsafe { libc::close(fd) }, 0);
        }
        assert!(!still_open, "Device::drop left its fd open");
    }
}
```

Pipe не является uinput-устройством: `UI_DEV_DESTROY` вернёт ошибку, но вызов
ограничен тестовым fd и не воздействует на реальные устройства.

- [ ] **Шаг 2: наблюсти ожидаемый RED**

```bash
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml \
  device_drop_closes_owned_fd -- --nocapture
```

Ожидается assertion failure `Device::drop left its fd open`.

- [ ] **Шаг 3: добавить минимальное закрытие fd**

Заменить `Drop for Device` на:

```rust
impl Drop for Device {
    fn drop(&mut self) {
        if self.fd < 0 {
            return;
        }

        unsafe {
            ui_dev_destroy(self.fd);
        }
        let _ = unistd::close(self.fd);
        self.fd = -1;
    }
}
```

- [ ] **Шаг 4: наблюсти GREEN**

```bash
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml \
  device_drop_closes_owned_fd -- --nocapture
```

Ожидается `1 passed, 0 failed`.

### Задача 3: Закрывать fd `Builder` и безопасно передавать владение

**Файлы:**

- Изменить: `vendor/uinput-0.1.3/src/device/builder.rs`

- [ ] **Шаг 1: добавить RED-тест ошибочного/прерванного Builder**

В конец `builder.rs` добавить helpers и первый тест:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn pipe_write_fd() -> c_int {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::close(fds[0]) }, 0);
        fds[1]
    }

    fn fd_is_open(fd: c_int) -> bool {
        unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
    }

    fn builder_with_fd(fd: c_int) -> Builder {
        Builder {
            fd: fd,
            def: unsafe { mem::zeroed() },
            abs: None,
        }
    }

    #[test]
    fn builder_drop_closes_owned_fd() {
        let fd = pipe_write_fd();
        drop(builder_with_fd(fd));

        let still_open = fd_is_open(fd);
        if still_open {
            assert_eq!(unsafe { libc::close(fd) }, 0);
        }
        assert!(!still_open, "Builder::drop left its fd open");
    }
}
```

- [ ] **Шаг 2: наблюсти поведенческий RED**

```bash
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml \
  builder_drop_closes_owned_fd -- --nocapture
```

Ожидается assertion failure `Builder::drop left its fd open`.

- [ ] **Шаг 3: добавить RED-тест передачи владения**

В тот же `mod tests` добавить:

```rust
#[test]
fn builder_into_raw_fd_transfers_ownership() {
    let fd = pipe_write_fd();
    let transferred = builder_with_fd(fd).into_raw_fd();

    assert_eq!(transferred, fd);
    assert!(fd_is_open(fd), "Builder closed the transferred fd");
    assert_eq!(unsafe { libc::close(fd) }, 0);
}
```

- [ ] **Шаг 4: наблюсти второй ожидаемый RED**

```bash
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml \
  builder_into_raw_fd_transfers_ownership -- --nocapture
```

Ожидается compile error: у `Builder` отсутствует `into_raw_fd`.

- [ ] **Шаг 5: реализовать единое владение fd**

В `impl Builder` добавить:

```rust
fn into_raw_fd(mut self) -> c_int {
    let fd = self.fd;
    self.fd = -1;
    fd
}
```

Успешный конец `create()` заменить на:

```rust
let fd = self.into_raw_fd();
Ok(Device::new(fd))
```

После `impl Builder` добавить:

```rust
impl Drop for Builder {
    fn drop(&mut self) {
        if self.fd >= 0 {
            let _ = unistd::close(self.fd);
            self.fd = -1;
        }
    }
}
```

Любой ранний `try!` теперь уничтожит builder и закроет fd; успешный `create()`
передаст ровно одно владение в `Device`.

- [ ] **Шаг 6: наблюсти GREEN всей локальной зависимости**

```bash
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml -- --nocapture
```

Ожидается прохождение трёх новых тестов и существующих тестов crate.

- [ ] **Шаг 7: зафиксировать RAII-исправление**

```bash
git add -f vendor/uinput-0.1.3/src/device/builder.rs \
  vendor/uinput-0.1.3/src/device/device.rs
git commit -m "fix: close uinput descriptors on drop"
```

### Задача 4: Проверить OpenSwitcher и собрать Debian package

**Файлы:**

- Изменить: `docs/audits/2026-07-22-required-input-worker-fail-safe-validation.md`

- [ ] **Шаг 1: проверить целевые и полные Rust-матрицы**

```bash
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml
cargo test --lib
cargo test --lib --features settings-ui -j1
rustfmt --edition 2021 --check src/daemon/keyboard.rs \
  src/daemon/input_backend.rs src/daemon/service.rs
git diff --check
```

Ожидается отсутствие failures; sandbox-зависимые Unix-socket tests при `EPERM`
повторяются вне restricted sandbox, как в основном отчёте.

- [ ] **Шаг 2: собрать точный основной артефакт**

```bash
DEB_BUILD_OPTIONS=nocheck ./manage.sh package deb
sha256sum dist/packages/open-switcher_0.1.0-1_amd64.deb
```

Использование `nocheck` допустимо только после свежих полных матриц шага 1.

- [ ] **Шаг 3: установить именно этот package в VM**

Скопировать package в гостя и запустить `dpkg -i` через QGA root:

```bash
scp -P 22223 \
  dist/packages/open-switcher_0.1.0-1_amd64.deb \
  openswitcher@127.0.0.1:/tmp/open-switcher_0.1.0-1_amd64.deb
python3 /tmp/openswitcher_qga.py \
  /home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/agent.sock \
  '{"execute":"guest-exec","arguments":{"path":"/usr/bin/dpkg","arg":["-i","/tmp/open-switcher_0.1.0-1_amd64.deb"],"capture-output":true}}'
```

Полученный QGA PID дождаться через `guest-exec-status`. Из-за известного
`M-09a` явно выполнить по SSH:

```bash
systemctl --user restart open-switcher-daemon.service
sha256sum /usr/bin/open-switcher-daemon
```

Hash должен совпасть с daemon, извлечённым из этого `.deb`.

- [ ] **Шаг 4: выполнить три recovery в одном PID**

Для каждого цикла определить новый X11 watcher fd через `/proc/PID/task/*/syscall`
и read-only просмотр его `pollfd` в gdb, затем выполнить только
`shutdown(fd, SHUT_RDWR)` существующим QGA/gdb fault harness. После возвращения
`Ready` записать PID, число ссылок
`/proc/PID/fd -> /dev/uinput`, число `Open-Switcher Virtual Device` и результат
bounded `EVIOCGRAB` probe. Ожидается:

```text
same_pid=true
uinput_fd_count=1
virtual_device_count=1
recovered_and_regrabbed=true
```

во всех трёх циклах.

- [ ] **Шаг 5: повторить пользовательскую матрицу**

```bash
PYTHONPATH=.worktrees/vm-lab \
  python3 /tmp/openswitcher_vm_behavior_matrix.py
```

Ожидается `summary=8/8` и одинаковые start/end PID.

- [ ] **Шаг 6: восстановить и проверить состояние VM**

Оставить `delay_ms=30`, `backspace_ms=0`, `typing_ms=0`, штатные features,
`DISPLAY=:0`, X11, layout group `0`, debug выключенным, daemon
`active/running`, Xephyr/Xed отсутствующими. VM-лабораторию не удалять.

- [ ] **Шаг 7: обновить итоговый отчёт и зафиксировать проверенный результат**

Добавить в русский validation report RED/GREEN, три runtime recovery, новые
package/daemon hashes и оставшиеся ограничения. Затем:

```bash
git add -f docs/audits/2026-07-22-required-input-worker-fail-safe-validation.md
git commit -m "docs: validate fail-safe input recovery"
```

### Задача 5: Финальное review и интеграция

**Файлы:** все изменения ветки относительно `94f0372`.

- [ ] **Шаг 1: запросить независимое read-only code review**

Проверить корректность fd ownership, отсутствие double-close, сохранение
writer/grab ordering и закрытие двух предыдущих Important review-gap.

- [ ] **Шаг 2: повторить финальные проверки после замечаний**

```bash
cargo test --manifest-path vendor/uinput-0.1.3/Cargo.toml
cargo test --lib
cargo test --lib --features settings-ui -j1
git diff --check
git status --short
```

- [ ] **Шаг 3: fast-forward объединить с `master` без потери пользовательских файлов**

Сначала сравнить конфликтующие ignored/untracked docs в основном worktree и
при необходимости временно переместить идентичные файлы в резервный каталог.
Затем выполнить:

```bash
git -C /home/andrey/Projects/OpenSwitcher merge --ff-only \
  perf/wakeable-x11-watcher
```

Пользовательскую `.gitignore` и несвязанные untracked docs не изменять.

- [ ] **Шаг 4: положить итоговый package в основной каталог**

Скопировать проверенный `.deb` в
`/home/andrey/Projects/OpenSwitcher/dist/packages/`, повторить SHA-256 и оставить
worktree/VM-лабораторию сохранёнными для будущей работы.
