# H-06: этап 1 — DEB и обычный Mint smoke

> **Для agentic workers:** выполнять инлайн по
> `superpowers:executing-plans`; субагентов не использовать.

**Цель:** за один ограниченный отрезок получить точный candidate DEB,
статически проверить пакет и подтвердить обычную работу OpenSwitcher в
Mint/Cinnamon/X11.

**Граница этапа:** максимум три часа с
`2026-07-29T02:23:36+03:00`. Не выполнять fault injection, `SIGKILL`
daemon/guardian, Ubuntu campaign, baseline performance и расширенный stress.

**Правило отказа:** при подтверждённом функциональном дефекте сохранить
воспроизведение и остановиться. Не начинать исправление в этом этапе.

---

### Задача 1: Подготовить воспроизводимый checkpoint

- [x] Сохранить уже прошедший ignored fake max-debt test, но исключить его из
  обязательных gates.
- [x] Убрать несвязанные форматирующие изменения из `src/config.rs`,
  `src/model.rs` и `src/tray/tray_service.rs`.
- [x] Проверить только изменённый H-06 файл:

```bash
rustfmt --edition 2021 --check src/daemon/xtest_guardian/service.rs
cargo test --release --locked --lib \
  guardian_cleanup_latency_for_maximum_debt \
  -- --ignored --nocapture --test-threads=1
git diff --check
```

- [x] Зафиксировать checkpoint отдельным коммитом (`881c8f2`).

### Задача 2: Один раз собрать и проверить candidate DEB

- [x] Запустить вне restricted sandbox:

```bash
./manage.sh package deb
```

- [x] Вычислить один точный путь без wildcard и сохранить SHA-256:

```bash
package="$(dpkg-parsechangelog -S Source)"
version="$(dpkg-parsechangelog -S Version)"
arch="$(dpkg --print-architecture)"
CANDIDATE_DEB="$(realpath "dist/packages/${package}_${version}_${arch}.deb")"
test -f "$CANDIDATE_DEB"
sha256sum "$CANDIDATE_DEB"
dpkg-deb -f "$CANDIDATE_DEB" Package Version Architecture
```

- [x] В одном временном каталоге проверить binary, systemd units, maintainer
  scripts, stop helper и hidden guardian mode.
- [x] Проверить extracted units через временный `SYSTEMD_UNIT_PATH`.

### Задача 3: Обычный package-first smoke в Mint

- [x] Запустить только сохранённую `mint-installed` VM.
- [x] Передать exact candidate DEB и сверить SHA-256 в guest.
- [x] Установить exact DEB и сверить установленные binary с распакованным
  пакетом.
- [ ] Проверить daemon/tray и отсутствие failed user units.
  **Заблокировано подтверждённым дефектом:** `open-switcher-xtest-guardian`
  получает `EACCES` при проверке `/proc/<daemon-pid>/exe` под
  `PrivateDevices=yes`, после чего daemon теряет writer и прекращает
  перезапускаться.
- [ ] Реалистично проверить:
  - обычный ввод;
  - ручную коррекцию последнего слова через F12;
  - переключение раскладки;
  - исправление двух заглавных;
  - исправление случайного Caps Lock.
  **Не выполнялось:** обычный smoke невозможен, пока guardian не запускается.
- [ ] Не совмещать ввод с искусственными сменами фокуса и не выполнять
  аварийные сценарии этого этапа.

### Задача 4: Зафиксировать результат и остановиться

- [x] Сохранить компактный текстовый evidence: commit, DEB path/SHA-256,
  package checks и результаты пяти smoke-сценариев.
- [x] Корректно выключить Mint VM, не удаляя overlay или laboratory.
- [x] Записать точку продолжения: сначала исправление обнаруженного blocker,
  затем повтор обычного smoke; два H-06 fault-injection сценария остаются
  отдельным следующим этапом.
- [x] Остановить работу независимо от оставшегося времени.

## Результат этапа

Этап остановлен по правилу отказа после подтверждения функционального
дефекта. Подробности сохранены в
`docs/audits/2026-07-29-h06-stage-1-package-mint-smoke.md`.
