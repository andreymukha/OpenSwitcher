# План реализации M-06: вывод прямого dev-runtime из эксплуатации

> **Для Codex:** выполнять инлайн в текущем worktree, соблюдая TDD и сверяя
> результат со спецификацией после единого implementation batch.

**Цель:** удалить из `manage.sh` самостоятельный PID/signal lifecycle и убрать
внутренний `OPEN_SWITCHER_RUNTIME_MODE=dev`, не изменив managed-поведение
установленного Debian-пакета.

**Архитектура:** `manage.sh` остаётся инструментом сборки, package/doctor и
явного управления `systemd --user`, но больше не является process supervisor.
Daemon и tray всегда используют существующую managed watchdog/recovery policy.
Старые dev-команды fail closed и не перенаправляются на systemd.

**Стек:** Bash, Rust, shell regression tests, Cargo, Debian package checks.

**Спецификация:**
`docs/superpowers/specs/2026-08-11-m06-dev-runtime-retirement-design.md`

---

## Этап 1. Безопасный RED для архитектурной границы

**Файлы:**

- создать: `tests/manage_dev_retirement_test.sh`

### Шаг 1. Добавить regression test

Тест должен сначала статически обнаруживать старую опасную реализацию и
останавливаться до любых lifecycle-вызовов. Проверить отсутствие как минимум:

- `PID_DIR`, `*_PIDFILE` и автоматического `.run` state;
- `start_component()`, `stop_component()`, `find_component_pids()` и
  `is_running()`;
- `nohup` и прямых `kill` по PID;
- `OPEN_SWITCHER_RUNTIME_MODE`, `RuntimeMode::Dev` и
  `is_dev_runtime_mode` в `manage.sh`/`src`.

После прохождения статического барьера тест в изолированной временной копии
проверяет:

- `manage.sh dev help` и все старые lifecycle-алиасы возвращают non-zero и
  понятное migration-сообщение;
- ни один stub `systemctl`, `cargo`, `nohup` или `ps` не вызван;
- `.run` не создан;
- `--help` рекламирует `build`, `package`, `doctor`, `systemd`, но не
  dev-runtime.

До исправления запускается только статический барьер и безопасный справочный
путь. Старые `start`/`stop` не должны выполняться.

### Шаг 2. Подтвердить RED

Запустить:

```bash
bash tests/manage_dev_retirement_test.sh
```

Ожидание: non-zero именно потому, что в `manage.sh` ещё присутствует прямой
PID/process lifecycle или `dev help` всё ещё принят.

---

## Этап 2. Удалить shell process supervisor и внутренний dev-mode

**Файлы:**

- изменить: `manage.sh`
- изменить: `src/system/mod.rs`
- изменить: `src/daemon/mod.rs`
- изменить: `src/tray/dbus_listener.rs`
- изменить: `tests/manage_dev_retirement_test.sh` только если RED обнаружил
  ошибку самого теста, а не расхождение требований

### Шаг 1. Сократить `manage.sh`

Удалить:

- `RUN_DIR`, `LOG_DIR`, `PID_DIR`, `DEV_RUNTIME_MODE` и PID-файлы;
- автоматический `mkdir -p .run/...`;
- dev debug-env defaults;
- `pidfile_for`, `logfile_for`, `process_name_for`, `find_component_pids`,
  `is_running`, `start_component`, `stop_component`, `show_status`,
  `show_logs`, `run_dev_command`;
- dev namespace и старые lifecycle-алиасы.

Сохранить:

- `PROFILE`, `TARGET_DIR`, пути к трём бинарникам;
- `binary_path_for`, `require_binary`, `build_binaries`;
- package, doctor, bootstrap migration и systemd-функции.

Добавить компактный `print_dev_runtime_retirement()`, который пишет в stderr,
что прямой dev-runtime удалён, и указывает `build`, `package deb` и `systemd`.

Dispatch должен быть явным:

- `build` вызывает только `build_binaries`;
- `dev|start|stop|restart|status|logs|settings` печатают retirement-сообщение и
  завершаются non-zero;
- пустая команда и help показывают актуальный usage;
- удалённая команда никогда не вызывает `run_systemd_command`.

### Шаг 2. Удалить Rust dev bypass

В `src/system/mod.rs` удалить `RUNTIME_MODE_ENV`, `RuntimeMode`,
`parse_runtime_mode`, `current_runtime_mode`, `is_dev_runtime_mode` и два
соответствующих unit-теста. Остальное использование `std::env` для определения
desktop/session оставить.

В `src/daemon/mod.rs` безусловно создавать `SessionBusTrayPresenceProbe` и
запускать существующий tray watchdog.

В `src/tray/dbus_listener.rs`:

- удалить импорт `is_dev_runtime_mode`;
- из `ensure_daemon_running()` убрать early return для dev;
- в listener thread всегда запускать существующий recovery, если daemon
  недоступен;
- не менять retries, delays, D-Bus semantics и quit-after-failed-recovery.

### Шаг 3. Подтвердить GREEN и focused regressions

```bash
bash tests/manage_dev_retirement_test.sh
bash -n manage.sh tests/manage_dev_retirement_test.sh
cargo test --lib tray::dbus_listener::tests -- --nocapture
cargo test --lib system::tests -- --nocapture
```

Ожидание: новый тест и обе существующие focused группы проходят.

### Шаг 4. Зафиксировать implementation batch

```bash
git add manage.sh src/system/mod.rs src/daemon/mod.rs \
  src/tray/dbus_listener.rs tests/manage_dev_retirement_test.sh
git commit -m "fix(dev): retire direct process lifecycle"
```

---

## Этап 3. Обновить поддерживаемый workflow и статус аудита

**Файлы:**

- изменить: `README.ru.md`
- изменить: `README.md`
- изменить: `docs/audits/2026-07-30-audit-remediation-status.md`
- создать после проверок:
  `docs/audits/2026-08-11-m06-dev-runtime-retirement-validation.md`

### Шаг 1. Обновить README

В обеих версиях:

- заменить quick start на package-first + `manage.sh build`;
- удалить инструкции `dev start/stop/status/logs/settings`;
- описать `systemd --user` как единственный поддерживаемый runtime из
  `manage.sh`;
- заменить development smoke на build/package/systemd workflow;
- убрать dev-команды из troubleshooting;
- явно отметить, что подробные debug env остаются opt-in.

### Шаг 2. Обновить M-06 status

До финальных gates поставить «закрыто реализацией, ожидает финальной проверки».
Основание: direct PID/signal lifecycle удалён, dev-mode bypass отсутствует,
старые команды fail closed.

Документ validation пока не утверждает результаты, которых ещё нет.

---

## Этап 4. Единые финальные gates и validation report

### Шаг 1. Shell/package regressions

```bash
bash tests/manage_dev_retirement_test.sh
bash tests/manage_package_deb_test.sh
bash tests/linux_input_setup_test.sh
bash tests/wayland_diagnostics_test.sh
bash -n manage.sh tests/manage_dev_retirement_test.sh
```

Все команды безопасны: они используют fixtures/mocks и не захватывают реальные
устройства, не отправляют ввод и не меняют systemd/udev/ACL хоста.

### Шаг 2. Rust и форматирование

```bash
cargo fmt --check
cargo test --all-targets --all-features -- --test-threads=1
```

Если общий `cargo fmt --check` обнаружит заранее существовавший drift, не
форматировать несвязанные файлы: проверить diff относительно base и отдельно
проверить только затронутые Rust-файлы.

### Шаг 3. Каноническая сборка DEB

```bash
./manage.sh package deb
```

Сборка может скачивать зависимости, но не устанавливает пакет и не меняет
системную конфигурацию. Runtime smoke на физических устройствах и в ВМ для этой
задачи не требуется, поскольку package-mode ветви watchdog/recovery не изменили
семантику, а input pipeline не затронут.

### Шаг 4. Inline review

Без субагентов, в соответствии с выбранным пользователем inline-процессом:

```bash
git diff d9b785441f92a5bc78e65f63be0e254723019079...HEAD --check
git diff --stat d9b785441f92a5bc78e65f63be0e254723019079...HEAD
git diff d9b785441f92a5bc78e65f63be0e254723019079...HEAD -- \
  manage.sh src/system/mod.rs src/daemon/mod.rs src/tray/dbus_listener.rs \
  tests/manage_dev_retirement_test.sh README.ru.md README.md
```

Сверить каждый инвариант спецификации, особенно отсутствие неявного mapping
старых команд на systemd и сохранение `build`/`systemd install`.

### Шаг 5. Записать фактический validation report

В русском отчёте указать:

- exact commits и scope;
- RED/GREEN evidence;
- результаты каждой команды с exit status/count;
- путь и checksum собранного DEB;
- почему runtime DEB не изменился;
- остаточные ограничения: старые `.run` файлы не удаляются автоматически,
  явные debug env остаются доступными;
- отсутствие VM/host input tests и обоснование.

После подтверждённых gates обновить M-06 status на «Закрыто» со ссылкой на
отчёт.

### Шаг 6. Зафиксировать документацию

```bash
git add README.ru.md README.md \
  docs/audits/2026-07-30-audit-remediation-status.md
git add -f docs/audits/2026-08-11-m06-dev-runtime-retirement-validation.md
git commit -m "docs: validate M-06 dev runtime retirement"
```

### Шаг 7. Повторная post-commit проверка

```bash
git status --short --branch
git log --oneline -3
git diff --check d9b785441f92a5bc78e65f63be0e254723019079...HEAD
```

Остановиться до merge и передать пользователю точный результат, артефакт DEB и
оставшиеся действия.
