# H-02 residual + M-07: безопасная передача физического ввода

**Дата:** 2026-07-31

**Статус:** согласовано

**Область:** остаток H-02, обработка `evdev`-очереди перед `EVIOCGRAB`,
обнаружение потери физического устройства по `poll(2)`

## Цель

Устранить два связанных дефекта жизненного цикла физической клавиатуры:

1. OpenSwitcher открывает `/dev/input/event*` задолго до `EVIOCGRAB`.
   События за это время уже получает рабочий стол, но они одновременно
   накапливаются в очереди fd OpenSwitcher и после захвата могут быть
   пересланы повторно.
2. Runtime-ожидание считает `POLLHUP`, `POLLERR` и `POLLNVAL` обычным
   отсутствием событий, поэтому отключение устройства не всегда немедленно
   запускает освобождение старого backend и восстановление.

Исправление не должно добавлять задержку к обычному вводу, F12,
автоматической коррекции, исправлению Caps Lock или двух заглавных букв.
Короткое ожидание разрешено только при первоначальном подключении input
backend или его восстановлении.

## Подтверждённые факты

Первоначальная часть H-02 уже исправлена коммитом `c1a998b`: virtual writer,
pointer watcher, input-target watcher и selected-text worker подготавливаются
до `EVIOCGRAB`. Остался другой порядок:

```text
resolve physical device
open physical fd
prepare writer/watchers/selected-text worker
EVIOCGRAB
```

В Linux `evdev` имеет отдельную очередь для каждого открытого клиента.
Пока exclusive grab отсутствует, события передаются всем клиентам. Установка
grab выбирает единственного последующего получателя, но не очищает очередь
этого клиента. Следовательно, события между `open()` и `EVIOCGRAB` могли уже
дойти до desktop и остаться непрочитанными в fd OpenSwitcher.

При отключении устройства `evdev_poll()` публикует `EPOLLHUP | EPOLLERR`.
Даже если в очереди одновременно остались данные и присутствует `EPOLLIN`,
`evdev_read()` сначала проверяет существование устройства и возвращает
`ENODEV`.

Основной источник:

- Linux `drivers/input/evdev.c`:
  <https://github.com/torvalds/linux/blob/master/drivers/input/evdev.c>

## Пользовательское поведение

Правило согласовано следующим:

- если при запуске или восстановлении OpenSwitcher пользователь продолжает
  печатать либо удерживает клавишу, физическая клавиатура пока не
  захватывается;
- ввод продолжает напрямую получать рабочий стол;
- OpenSwitcher подключается после короткого спокойного промежутка;
- если безопасный промежуток не найден за ограниченное время, fd закрывается
  без grab, а lifecycle повторяет попытку позже;
- это правило не выполняется перед каждой коррекцией и не увеличивает время
  коррекции после успешной активации backend.

## Инварианты

1. Live fd физической клавиатуры не существует во время продолжительной
   подготовки writer/watchers/selected-text worker.
2. Ни одно событие, прочитанное до успешного `EVIOCGRAB`, не передаётся в
   virtual writer: его уже мог получить desktop.
3. Grab не выполняется при известной нажатой физической клавише.
4. Неудача или timeout до grab закрывает fd и не вызывает `ungrab`.
5. Неудача после успешного grab сначала явно освобождает grab; ошибка
   `ungrab` приводит к закрытию fd.
6. Grab разрешён только при актуальном session lease и живых обязательных
   input-компонентах.
7. `POLLHUP`, `POLLERR` и `POLLNVAL` никогда не превращаются в timeout.
8. При смешанном `POLLIN | terminal flag` приоритет имеет потеря устройства,
   а не чтение хвоста.
9. Новый backend не устанавливается, пока shutdown старого backend не
   завершил существующий fail-safe протокол.
10. После потери устройства transient word/modifier/capture state
    инвалидируется существующим recovery-путём.

## Выбранная архитектура

### Разделение prepared и active состояния

`PreparedKeyboardController` хранит проверенное описание физического
устройства (`VerifiedInputDevice`), session lease и уже готовые
writer/watchers, но не хранит открытый `evdev::Device`.

`KeyboardController` создаётся только после успешного handoff и получает
физический handle уже в состоянии `Grabbed`. Это делает невозможным случайное
чтение старой pre-grab очереди обычным event loop.

Состояния физического handle должны быть явными:

```text
Verified -> OpenUngrabbed -> Grabbed -> Released/Closed
```

Переход назад из `Grabbed` всегда проходит через явный release; `Drop`
остаётся последней страховкой, а не основным протоколом.

### Порядок подготовки

Порядок открытия backend:

```text
session/seat authorization
resolve and verify physical-device metadata
prepare virtual writer and receive ready ACK
prepare mandatory watchers and receive ready ACK
prepare selected-text worker and verify health
recheck session/dependency health
open physical fd and verify fd identity
bounded quiescent handoff
EVIOCGRAB
post-grab validation and Caps Lock snapshot
publish active backend
```

Поиск устройства может кратковременно открывать проверочные fd, как и сейчас,
но они закрываются внутри discovery. Единственный fd, из которого runtime
позже читает и пересылает события, открывается непосредственно перед handoff.

### Bounded quiescent handoff

Handoff использует тот же poll-adapter, что и runtime, и не меняет физическое
устройство:

1. Открыть подтверждённый path и повторно проверить `fstat`/device identity.
2. Прочитать и отбросить все уже готовые полные `evdev`-пакеты. До grab их
   продолжает получать desktop.
3. Получить текущее состояние клавиш через `EVIOCGKEY`.
4. Если клавиши отпущены, ждать один короткий quiet window без `POLLIN`.
5. При новом пакете отбросить его и начать quiet window заново.
6. Перед ioctl ещё раз проверить отсутствие нажатых клавиш, пустую очередь,
   session lease и readiness обязательных компонентов.
7. Если общий handoff deadline исчерпан, закрыть fd без grab и вернуть
   отдельный recoverable результат «устройство сейчас занято вводом».

Начальные фиксированные пределы:

- quiet window: `20 ms`;
- общий handoff deadline одной попытки: `100 ms`.

Это внутренние safety-константы, а не пользовательские параметры. При
отсутствии ввода они добавляют только 20 ms к запуску/recovery, но не к
обработке клавиш после запуска. Если пользователь печатает дольше 100 ms,
существующий lifecycle закрывает подготовленный backend и повторяет открытие
по своей bounded retry policy.

### Граница `EVIOCGRAB`

Непосредственно перед ioctl повторяются session/dependency prechecks.
Непосредственно после успешного ioctl:

1. повторно проверяется session lease;
2. проверяется, что физическая клавиша не оказалась нажатой на самой границе;
3. при неудаче выполняется явный `ungrab`, fd закрывается и handoff
   повторяется через lifecycle;
4. после успешной проверки считывается актуальное состояние Caps Lock;
5. события, пришедшие после этого, принадлежат обычному physical-event path.

Caps Lock snapshot переносится с позиции «перед grab» на позицию «сразу после
подтверждённого grab». Это уменьшает окно рассинхронизации: состояние
фиксируется уже после завершения handoff, но до публикации backend.

В userspace нет атомарной операции «проверить пустую очередь + проверить
клавиши + выполнить `EVIOCGRAB`». Поэтому полностью математически устранить
последнее микроскопическое окно нельзя. Quiet window, повторные проверки и
отказ от grab при замеченной нажатой клавише устраняют реалистичные сценарии.
Если press начался прямо после ioctl и замечен postcheck, handoff безопасно
отменяется, но один пограничный символ теоретически может не попасть в
desktop. Полный press+release, физически завершившийся целиком между последней
проверкой и ioctl, остаётся теоретическим риском повтора. Оба окна
документируются и проверяются целевым VM-тестом; timestamp-ledger ради них не
вводится.

### Единая классификация `poll(2)`

Сырой `revents` преобразуется отдельной чистой функцией в один из результатов:

- `TimedOut`;
- `Readable`;
- `DeviceLost { flags }`.

Приоритет классификации:

1. любой из `POLLHUP | POLLERR | POLLNVAL` -> `DeviceLost`;
2. `POLLIN` без terminal flags -> `Readable`;
3. нулевой результат `poll` -> `TimedOut`;
4. неожиданный положительный `poll` без известных флагов -> typed
   recoverable poll failure, но не timeout.

Комбинация `POLLIN | POLLHUP` классифицируется как `DeviceLost`. Последние
события не пересылаются: Linux всё равно возвращает `ENODEV` при read после
disconnect, а попытка восстановить неизвестный хвост опаснее явного сброса
transient state.

`EINTR` сохраняет существующее повторение системного вызова. Остальные ошибки
`poll` проходят через текущую `io::Error`-классификацию.

### Typed errors и recovery

Вводятся два различимых recoverable результата:

- handoff временно занят физическим вводом;
- физическое устройство потеряно либо его poll fd стал недействителен.

Они не маскируются строковым сравнением и явно маршрутизируются:

- busy до grab -> backend не публикуется, fd закрывается, scheduled retry;
- device loss активного backend -> состояние `Recovering`, shutdown старого
  backend, reset transient state, повторный discovery/open;
- device loss во время handoff -> fd закрывается без grab, scheduled retry.

При физическом unplug ядро уже прекращает существование устройства и
освобождает его grab. OpenSwitcher всё равно выполняет обычный shutdown:
`ungrab` может вернуть `ENODEV`, после чего существующий
`release_grab_or_close_device()` закрывает fd. Эта ожидаемая ошибка не должна
подменять первоначальную причину и запрещать recovery.

## Диагностика

Используется существующий bounded debug logger. Логи не содержат введённый
текст или keycodes.

Достаточны события:

- начало и результат handoff;
- число отброшенных pre-grab пакетов, без их содержимого;
- причина retry: held key, continued input, deadline, session/dependency
  change;
- terminal poll flags и переход lifecycle в recovery;
- успешное повторное подключение.

Повторяющийся busy retry агрегируется либо rate-limit, чтобы удерживаемая
клавиша не засоряла журнал.

## Производительность и совместимость

- Постоянный новый поток не создаётся.
- Периодический polling после успешной активации не добавляется.
- Runtime остаётся на существующем blocking `poll` с bounded timeout.
- Quiet handoff выполняется только при startup/recovery.
- Алгоритмы раскладки, коррекции, Caps Lock, двух заглавных, выделенного
  текста и clipboard не меняются.
- Существующая session/seat identity защита и writer fail-stop protocol
  сохраняются.

## Тестирование

Реализация выполняется через TDD. Host input devices не используются.

### Unit и fault injection

- prepared pipeline не держит live physical fd;
- writer/watchers/selected-text readiness всегда предшествуют physical open;
- pre-grab пакеты читаются, но никогда не достигают writer;
- held key не допускает вызов grab;
- непрерывные события до deadline дают recoverable busy и close без ungrab;
- остановившийся ввод проходит quiet window и допускает grab;
- session или worker failure перед ioctl не вызывает grab;
- session failure после ioctl вызывает release до shutdown;
- Caps Lock snapshot выполняется после grab и до публикации backend;
- `POLLIN` -> readable;
- timeout -> timeout;
- каждый из `POLLHUP`, `POLLERR`, `POLLNVAL` -> device loss;
- `POLLIN | terminal flag` -> device loss;
- неожиданный ненулевой `revents` не становится timeout;
- device loss маршрутизируется в `Recovering`;
- ошибка `ungrab` после unplug закрывает fd и не запрещает recovery;
- новый backend не устанавливается до завершения shutdown старого.

### Общие безопасные gates

После focused-тестов выполняются:

```text
cargo fmt --check
cargo test --all-targets
git diff --check
Debian package script tests
```

### Целевая VM-проверка

В сохранённой лаборатории используется только управляемая виртуальная
клавиатура гостевой системы:

1. Инъекция до grab подтверждает, что desktop observer получает пакет один
   раз, а OpenSwitcher его не повторяет.
2. Удерживаемая клавиша во время запуска не блокируется; после release backend
   автоматически активируется.
3. Поток событий дольше handoff deadline не допускает grab; после прекращения
   ввода recovery завершается.
4. Virtual unplug в idle и при готовом `POLLIN` немедленно переводит backend в
   recovery.
5. Replug восстанавливает backend без рестарта daemon и без остаточно
   нажатых модификаторов.
6. Несколько повторов unplug/replug не оставляют старые fd или второй live
   backend.

Проверяется собранный DEB. Host-клавиатура, host clipboard, host layout,
systemd и udev не изменяются. Полная двухпрофильная runtime-кампания остаётся
отложена до завершения остальных аудиторских исправлений.

## Не входит в задачу

- Timestamp-ledger для теоретически атомарной классификации каждого события
  вокруг ioctl.
- Изменение writer transaction, synthetic-input ledger или H-06 guardian.
- Изменение layout detection H-07.
- Clipboard M-01..M-03.
- Поддержка нескольких одновременно захватываемых физических клавиатур.
- Новая постоянная служба, поток или пользовательская настройка handoff
  таймингов.
- Полная повторная runtime-кампания всех исправлений аудита.

## Критерии приёмки

1. Physical fd, используемый runtime, открывается только после готовности
   всех обязательных зависимостей.
2. Все полные события, обнаруженные до grab, отбрасываются и не пересылаются.
3. При удерживаемой клавише или непрерывном вводе grab не выполняется, а
   desktop продолжает получать ввод.
4. В спокойном случае backend активируется автоматически с единственной
   startup/recovery-задержкой quiet window.
5. `POLLHUP`, `POLLERR`, `POLLNVAL` и их комбинации с `POLLIN` немедленно
   запускают recovery.
6. После unplug/replug OpenSwitcher повторно находит устройство и продолжает
   работу без рестарта.
7. Никакой новый overhead не появляется в steady-state input/correction path.
8. Focused, полный безопасный suite, package gates и целевая VM-проверка
   проходят.
