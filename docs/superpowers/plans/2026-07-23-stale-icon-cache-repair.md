# Восстановление устаревшего icon-cache — план выполнения

> **Для агентных исполнителей:** REQUIRED SUB-SKILL: использовать
> `superpowers:executing-plans` и выполнить шаги последовательно.

**Цель:** Вернуть иконку OpenSwitcher в меню приложений, удалив устаревшую
ссылку из производного пользовательского cache темы `hicolor`.

**Архитектура:** Код, DEB, desktop entry и файлы иконок не меняются. Cache
пересобирается штатной утилитой из фактического содержимого пользовательской
темы, затем результат проверяется через GTK текущей Cinnamon/X11-сессии.

**Технологии:** `gtk-update-icon-cache`, GTK 3 / PyGObject, тема `hicolor`.

---

### Задача 1: Пересобрать и проверить пользовательский icon-cache

**Файлы:**

- Пересоздать: `/home/andrey/.local/share/icons/hicolor/icon-theme.cache`
- Не изменять: исходники OpenSwitcher, DEB, системную тему `/usr/share/icons`

- [x] **Шаг 1: подтвердить исходный RED**

Через `Gtk.IconTheme.lookup_icon("open-switcher", 48, 0)` подтвердить, что GTK
возвращает отсутствующий путь
`/home/andrey/.local/share/icons/hicolor/512x512/apps/open-switcher.png`.

- [x] **Шаг 2: пересобрать cache из фактического содержимого темы**

```bash
gtk-update-icon-cache --force --ignore-theme-index \
  /home/andrey/.local/share/icons/hicolor
```

Ожидается exit code `0` и сообщение об успешном создании cache.

- [x] **Шаг 3: подтвердить GREEN через GTK**

Для размеров `16, 24, 32, 48, 64, 128, 512` вызвать
`Gtk.IconTheme.lookup_icon("open-switcher", size, 0)`.

Ожидаемый путь для каждого размера:

```text
/usr/share/icons/hicolor/512x512/apps/open-switcher.png
```

- [x] **Шаг 4: проверить границы изменения**

Убедиться, что системная PNG читается, cache больше не содержит устаревшую
запись `open-switcher`, а существовавшие пользовательские изменения Git не
затронуты.

Фактический результат: новый процесс GTK разрешил имя `open-switcher` в
`/usr/share/icons/hicolor/512x512/apps/open-switcher.png` для всех семи
проверенных размеров. После перезапуска Cinnamon пользователь подтвердил, что
иконка появилась в меню приложений.
