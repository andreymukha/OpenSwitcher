# Проверка H-02 residual и M-07: безопасная передача физического ввода

**Дата:** 2026-07-31

**Ветка:** `fix/h02-m07-input-handoff`

**База `master`:**
`064b792174740a9fbc39489dd7f4505fe7c8fa10`

**Проверенное production-состояние:**
`c6438ce2609f2cd6ef9cfc5680d3fe00d6e03929`

**Статус:** реализация, общий source/package gate и целевая package-first
проверка в Linux Mint/Cinnamon/X11 завершены. Слияние в `master` и отправка в
remote не выполнялись.

## Краткий результат

Оба замечания закрыты в согласованной области:

- runtime fd физической клавиатуры теперь открывается непосредственно перед
  bounded handoff, а не до подготовки writer/watchers;
- события pre-grab очереди отбрасываются и не пересылаются повторно;
- удерживаемая клавиша или продолжающийся ввод запрещают `EVIOCGRAB`;
- после короткого спокойного окна backend активируется существующим
  lifecycle retry;
- `POLLHUP`, `POLLERR` и `POLLNVAL` больше не считаются timeout;
- runtime hot-unplug дал `poll events=0x18` (`POLLERR | POLLHUP`) и во всех
  трёх циклах перевёл тот же daemon в `Recovering`;
- hot-replug трижды восстановил backend без перезапуска daemon;
- PID, число event fd и число virtual backend после каждого восстановления
  остались стабильными;
- F12, обычный ввод и освобождение `Ctrl`/`Alt`/`Shift` после recovery
  проверены физическим QEMU input path.

## Реализация

Основные границы находятся в:

- `src/daemon/keyboard.rs:1508` — release при неудаче после grab и Caps Lock
  snapshot только после post-grab проверки;
- `src/daemon/keyboard.rs:1538` — bounded quiescent handoff;
- `src/daemon/keyboard.rs:1914` — порядок activation;
- `src/daemon/keyboard.rs:2047` — deferred open подтверждённого устройства;
- `src/daemon/keyboard.rs:3886` — typed классификация результата `poll(2)`;
- `src/daemon/input_backend.rs:24` — routing device loss в `Recovering`;
- `src/error/mod.rs:161` — `PhysicalKeyboardDeviceLost`;
- `src/error/mod.rs:166` — `PhysicalKeyboardHandoffBusy`.

Фиксированные пределы действуют только при startup/recovery:

```text
quiet window: 20 ms
handoff deadline одной попытки: 100 ms
```

Обычный ввод, F12 и автоматическая коррекция после активации backend через
этот handoff не проходят и дополнительной задержки не получили.

Коммиты реализации:

```text
c7ac6bc fix: recover on physical input poll failure
5b06a6c fix: acquire physical input only after quiet handoff
c6438ce test: close physical input recovery regressions
```

## Source и package gates

Выполнены:

```bash
cargo fmt --check
cargo test --locked --all-targets
git diff --check
bash tests/debian_package_scripts_test.sh
bash tests/input_access_package_test.sh
bash tests/manage_package_deb_test.sh
./manage.sh package deb
```

Результат Rust suite вне restricted syscall sandbox:

```text
library:       937 passed, 1 ignored
daemon binary:   4 passed
D-Bus API:       11 passed
H-06 VM probe:    5 passed
total:          957 passed, 0 failed, 1 ignored
```

Shell/package проверки:

| Проверка | Результат |
|---|---|
| `cargo fmt --check` | exit 0 |
| `git diff --check` | exit 0 |
| `tests/debian_package_scripts_test.sh` | `ok` |
| `tests/input_access_package_test.sh` | `ok` |
| `tests/manage_package_deb_test.sh` вне sandbox | `ok` |
| `./manage.sh package deb` | exit 0 |

Первый `cargo test --locked --all-targets` внутри restricted sandbox не был
валидным прогоном: `UnixStream::pair()` в существующем
`daemon::x11_wait::tests::stop_readiness_returns_stop_requested` получил
`EPERM`, после чего зависимые socket/D-Bus тесты дали каскадные ошибки.
Изолированный тест подтвердил тот же sandbox-denial. Точная полная команда
вне sandbox прошла с результатом выше.

`tests/manage_package_deb_test.sh` внутри sandbox также завершился exit 1 без
вывода из-за запрета создавать mock parent artifacts. Повтор точной команды
вне sandbox прошёл. Production-код ради этих ограничений не менялся.

## Идентичность Debian package

```text
Package:      open-switcher
Version:      0.1.0-6
Architecture: amd64
Size:         3313104 bytes

Path:
/home/andrey/Projects/OpenSwitcher/dist/packages/open-switcher_0.1.0-6_amd64.deb

SHA-256:
1efdcf0cc6f699c5c0ed38f8ff7bade1f17abcef74cae5ad6c5168dd805d90c6
```

В госте:

```text
dpkg-query: open-switcher 0.1.0-6
DEB SHA-256:
1efdcf0cc6f699c5c0ed38f8ff7bade1f17abcef74cae5ad6c5168dd805d90c6

/usr/bin/open-switcher-daemon SHA-256:
d0a1c50dc45118b2a2c4087059092ffeb2ea3a43cd76cd43d18760e0ffa576d9

daemon из DEB SHA-256:
d0a1c50dc45118b2a2c4087059092ffeb2ea3a43cd76cd43d18760e0ffa576d9
```

Пакет был обновлён в VM с `0.1.0-5` до точного `0.1.0-6`:

```bash
scp -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 \
  -P 22223 \
  -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts \
  -o StrictHostKeyChecking=yes \
  dist/packages/open-switcher_0.1.0-6_amd64.deb \
  openswitcher@127.0.0.1:/tmp/open-switcher_0.1.0-6_amd64.deb

ssh -i /home/andrey/VMs/OpenSwitcherLab/keys/id_ed25519 \
  -p 22223 \
  -o UserKnownHostsFile=/home/andrey/VMs/OpenSwitcherLab/keys/known_hosts \
  -o StrictHostKeyChecking=yes \
  openswitcher@127.0.0.1 \
  'sudo env DEBIAN_FRONTEND=noninteractive apt-get install --yes \
    /tmp/open-switcher_0.1.0-6_amd64.deb'
```

## Профиль VM

```text
OS:       Linux Mint 22.2 Zara
desktop:  Cinnamon
session:  X11, active local user session, seat0
kernel:   6.14.0-29-generic
layouts:  us,ru
QEMU:     8.2.2
```

Использован сохранённый overlay:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/disk.qcow2
```

Для однозначного сопоставления QMP input с hot-plug `testkbd` только в этой
тестовой сессии отключён встроенный виртуальный i8042:

```text
-machine q35,accel=kvm,i8042=off
-device qemu-xhci,id=testxhci
-device usb-kbd,id=testkbd,bus=testxhci.0,port=1
```

Физические устройства хоста в VM не передавались. Сеть хоста не менялась:
использован существующий QEMU user-mode forward
`127.0.0.1:22223 -> guest:22`.

QMP-события отправлялись через:

```text
input-send-event
device=video0
head=0
event={type:key,data:{down:<bool>,key:{type:qcode,data:<key>}}}
```

Hotplug:

```text
device_del {id:testkbd}

device_add {
  driver:usb-kbd,
  id:testkbd,
  bus:testxhci.0,
  port:1
}
```

Полный фактический QEMU argv сохранён в evidence.

## H-02: held-key и pre-grab очередь

Последовательность:

1. daemon остановлен, в Xed открыт пустой `held-key.txt`;
2. через QMP удержан `Shift`;
3. запущен daemon;
4. до release отправлен `B`, который продолжил напрямую получать desktop;
5. `Shift` отпущен;
6. после lifecycle retry появился active backend;
7. через активный OpenSwitcher отправлен `c`, затем `Ctrl+S`.

До release:

```text
input-handoff-complete: 0
input-handoff-busy: присутствует
```

Лог подтвердил реальные пакеты в pre-grab очереди:

```text
[input-debug] stage=input-handoff-busy
path=/dev/input/event1 discarded_events=6
```

Таких попыток с положительным `discarded_events` было 13. Ни один
отброшенный пакет не попал в virtual writer.

После release:

```text
[input-debug] stage=input-handoff-complete
path=/dev/input/event1 discarded_events=0
```

Точный результат файла:

```text
Bc\n
```

То есть прямой ввод до grab и ввод после grab появились по одному разу.

Дополнительно выполнен управляемый restart-burst длительностью около 600 ms:

```text
expected: qwertyuiopasdfghjklz\n
actual:   qwertyuiopasdfghjklz\n
```

Потерь, перестановки и дублей в принятом burst не было.

## M-07: runtime unplug/replug

Чистая hotplug-кампания началась после отдельного restart:

```text
MainPID=6263
NRestarts=0
event_fds=2
total_fds=23
virtual_devices=1
```

Каждый из трёх `device_del testkbd` дал:

```text
[input-debug] stage=keyboard-read-error
error=Physical keyboard device became unavailable:
/dev/input/event1 (poll events=0x18)

[input-debug] stage=input-backend-transition
previous_state=Ready next_state=Recovering result=applied
reason=Physical keyboard device became unavailable:
/dev/input/event1 (poll events=0x18)
```

`0x18` — одновременные `POLLERR | POLLHUP`. Это runtime-подтверждение
исходного M-07, а не вывод только из unit-теста.

Ожидаемый `ungrab` после физического исчезновения вернул `ENODEV`; код закрыл
fd и сохранил исходную typed-причину:

```text
[input-debug] stage=grab-release-error
action=close-fd-before-writer-wait error=No such device (os error 19)
```

Результаты:

| Цикл | Unplug | Replug | PID | Event fd после recovery | Total fd | Virtual backend |
|---|---|---|---:|---:|---:|---:|
| 1 | typed `0x18`, fd -> 0 | восстановлен | 6263 | 2 | 23 | 1 |
| 2 | наблюдён <= 279 ms | 2359 ms | 6263 | 2 | 23 | 1 |
| 3 | наблюдён <= 274 ms | 3046 ms | 6263 | 2 | 23 | 1 |

Времена циклов 2 и 3 — host end-to-end верхняя граница, включающая QMP,
loopback SSH и polling evidence. Внутренний terminal poll произошёл не позже
этой границы. Для цикла 1 точный end-to-end таймер не считается: первый
измерительный probe начался уже после QMP response.

Восстановление до примерно 3 секунд соответствует существующему steady retry
deadline. Перезапуска процесса не было. После каждого replug присутствовали
ровно один daemon и один `Open-Switcher Virtual Device`.

Пока клавиатура отсутствовала, discovery fail-closed отклонял
`/dev/input/event0` (`Power Button`) по отсутствию доступа. Это не создало
второй backend и не помешало восстановлению `testkbd`.

## Функциональный smoke после recovery

После трёх hotplug циклов в Xed через QEMU keyboard выполнено:

```text
ghbdtn + F12 -> привет
Ctrl down/up
Alt down/up
Shift down/up
Ctrl+S
```

Точный файл:

```text
привет\n
```

`EVIOCGKEY` для физического `testkbd` вернул пустой список нажатых клавиш.
`xinput query-state` для `Open-Switcher Virtual Device` показал все keys
`up`. Финальное состояние:

```text
MainPID=6263
NRestarts=0
active/running
```

Залипших клавиш или модификаторов не обнаружено.

## Ограничения и отброшенные прогоны

- Runtime проверен в одном целевом профиле Mint/Cinnamon/X11. Kernel evdev
  handoff/poll не зависит от desktop, но повтор в Wayland остаётся частью
  общей финальной кампании.
- Runtime hot-unplug подтвердил `POLLERR | POLLHUP`. Отдельный `POLLNVAL`
  покрыт unit-тестами, но эта модель QEMU его не породила.
- Полностью атомарной userspace-операции
  «проверить queue/key state и выполнить `EVIOCGRAB`» в Linux нет. Принятые
  quiet window, повторные pre/postchecks и отказ при замеченной клавише
  закрывают реалистичный сценарий; теоретическое микроокно из спецификации
  остаётся документированным.
- Предварительный диагностический генератор с паузами 3–5 ms оказался
  непригоден как acceptance evidence: QEMU оставил keycodes
  `[31, 32, 34, 37]` в состоянии down. OpenSwitcher в этом состоянии
  корректно не захватил клавиатуру. Все клавиши были явно отпущены, после
  чего начата отдельная чистая hotplug-кампания. Этот прогон в pass-результат
  не включён.
- Полная двухпрофильная install/remove/upgrade и single-fault кампания не
  повторялась: она остаётся одним объединённым этапом после остальных
  remediation-задач.

## Evidence

Основной каталог:

```text
/home/andrey/VMs/OpenSwitcherLab/runs/mint-install-v1/
  h02-m07-input-handoff-20260731/
```

Ключевые файлы:

```text
qemu-argv.txt
metadata.txt
input-debug.log
burst-debug.log
hotplug-debug.log
hotplug-fd-counts.txt
hotplug-summary.txt
modifier-state.txt
held-key.txt
burst.txt
functional-smoke.txt
functional-smoke-result.txt
journal.txt
SHA256SUMS
```

VM штатно выключена. QEMU process, QMP socket и SSH forward отсутствуют.
Overlay, ключи и evidence сохранены; лаборатория не удалялась.

## Итог

**H-02 — закрыто.** Runtime fd больше не живёт во время продолжительной
подготовки, held input не захватывается, pre-grab пакеты отбрасываются, а
принятый desktop input не дублируется.

**M-07 — закрыто.** Terminal poll flags имеют приоритет над `POLLIN`/timeout,
typed device loss немедленно закрывает старый backend и переводит lifecycle в
recovery; replug восстанавливает тот же daemon без fd/backend growth.

В проверенной области механизм handoff/release ведёт себя fail-safe: при
неопределённости он оставляет ввод desktop и повторяет подключение позже, а
при потере устройства закрывает старое состояние до публикации нового
backend.
