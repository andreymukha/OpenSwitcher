# План реализации синхронизации README

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Синхронизировать сведения о runtime-зависимостях в английском и русском README и убрать устаревающий номер DEB из обеих инструкций установки.

**Architecture:** Изменения ограничены двумя Markdown-файлами. `debian/control` служит источником истины для runtime-зависимостей, а нейтральный токен `VERSION` сохраняет shell-команду безопасной и не привязывает документацию к конкретному Debian revision.

**Tech Stack:** Markdown, Debian package metadata, read-only shell-проверки через `rg`, `diff` и Git.

---

### Task 1: Синхронизировать обе версии README

**Files:**
- Modify: `README.md:45-53`
- Modify: `README.ru.md:46-54`
- Modify: `README.ru.md:172-181`
- Modify: `README.ru.md:258-260`
- Reference: `debian/control:19-29`

- [ ] **Step 1: Зафиксировать исходное расхождение**

Run:

```bash
rg -n 'open-switcher_0\.1\.0-4_amd64\.deb' README.md README.ru.md
rg -n 'x11-xkb-utils|gsettings-desktop-schemas' README.ru.md
```

Expected: первая команда показывает по одному совпадению в каждом README;
вторая не показывает совпадений и завершается с кодом `1`.

- [ ] **Step 2: Сделать пример установки version-neutral**

В `README.md` заменить команду и следующее пояснение на:

````markdown
```bash
sudo apt install ./open-switcher_VERSION_amd64.deb
```

Replace `VERSION` with the version in the downloaded filename. Add `--reinstall` only when
reinstalling the same package version that is already installed.
````

В `README.ru.md` использовать смысловой эквивалент:

````markdown
```bash
sudo apt install ./open-switcher_VERSION_amd64.deb
```

Замени `VERSION` на версию в имени скачанного файла. Добавляй `--reinstall`, только если
переустанавливается уже установленная версия пакета.
````

- [ ] **Step 3: Перенести runtime-зависимости в русский README**

После требования совместимого tray host в `README.ru.md` добавить:

```markdown
- инструменты определения раскладки: `setxkbmap` из `x11-xkb-utils`, `gsettings` из
  `libglib2.0-bin` и схемы из `gsettings-desktop-schemas`

APT устанавливает эти runtime-зависимости автоматически при установке OpenSwitcher
из поддерживаемого `.deb`-пакета.
```

Секцию после build dependencies заменить на:

```markdown
Дополнительный полезный пакет для Debian/Ubuntu-подобных систем:
- `lintian` для опциональной локальной проверки Debian-пакета
```

- [ ] **Step 4: Проверить фактическое соответствие**

Run:

```bash
test "$(rg -l 'open-switcher_VERSION_amd64\.deb' README.md README.ru.md | wc -l)" -eq 2
! rg -n 'open-switcher_0\.1\.0-4_amd64\.deb|open-switcher_<version>' README.md README.ru.md
for dep in x11-xkb-utils libglib2.0-bin gsettings-desktop-schemas; do
  rg -q "$dep" debian/control
  rg -q "$dep" README.md
  rg -q "$dep" README.ru.md
done
test "$(rg -c '^```' README.md)" -eq "$(rg -c '^```' README.ru.md)"
test "$(rg -c '^#{1,6} ' README.md)" -eq "$(rg -c '^#{1,6} ' README.ru.md)"
git diff --check
```

Expected: exit code `0`, без вывода от `git diff --check`.

- [ ] **Step 5: Просмотреть ограниченный diff**

Run:

```bash
git diff -- README.md README.ru.md
```

Expected: изменены только две инструкции установки и пропущенный русский блок
runtime/build dependencies; другие разделы отсутствуют в diff.

- [ ] **Step 6: Создать отдельный коммит**

```bash
git add README.md README.ru.md
git commit -m "docs: synchronize README variants"
```

Expected: коммит содержит только `README.md` и `README.ru.md`.
