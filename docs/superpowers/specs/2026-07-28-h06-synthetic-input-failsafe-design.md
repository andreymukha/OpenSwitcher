# Проект fail-safe жизненного цикла синтетического ввода H-06

**Дата:** 2026-07-28

**Статус:** концепция согласована; документ подготовлен для проверки
пользователем перед планированием и реализацией

**Область:** баланс синтетических нажатий uinput/XTEST, аварийная очистка,
жизненный цикл XTEST executor-guardian и подтверждённое завершение операций

## Краткий результат

OpenSwitcher должен считать каждое начатое синтетическое нажатие обязательством,
которое снимается только после подтверждённого отпускания или безопасного
уничтожения владеющего им backend.

Для uinput существующая граница владения остаётся kernel-owned: уничтожение
виртуального устройства и закрытие его fd сбрасывают состояние устройства.
Поверх неё добавляется общий operation-wide ledger, который закрывает ошибки
между отдельными `key-down`, `key-up` и `synchronize`.

Для XTEST этого недостаточно. Проверка в сохранённой Linux Mint/Cinnamon/X11 ВМ
подтвердила, что X-сервер сохраняет синтетически нажатую клавишу как после
обычного закрытия X11-клиента, так и после его `SIGKILL`. Поэтому все XTEST
мутации передаются отдельному socket-activated executor-guardian в собственной
`systemd --user` cgroup. Он является единственным процессом, который отправляет
XTEST press/release, ведёт собственный ledger и переживает гибель основного
daemon.

Штатная последовательность событий, раскладка и существующие deliberate
задержки коррекции не меняются. При редком hard failure допускается частично
изменённое слово или уже переключившаяся раскладка. Для успешно reconciled
single-fault путей synthetic debt должен быть снят. Если backend не позволяет
это доказать, OpenSwitcher обязан прекратить мутации, освободить физический
grab, зафиксировать `Unreconciled` и завершить процесс, а не сообщать ложный
success.

## Причина работы

### Исходное замечание аудита

H-06 зафиксировал, что синтетические последовательности состоят из нескольких
зависимых мутаций:

- временное отпускание физических модификаторов;
- `Backspace` press/release;
- временный `Shift`;
- воспроизведение букв и знаков;
- сочетания Copy/Paste и смены раскладки;
- восстановление удерживаемых пользователем модификаторов;
- синтетический разделитель после коррекции.

Ошибка, timeout, panic или остановка между любыми двумя шагами способны оставить
получателя в промежуточном состоянии. Локальные cleanup-ветви уменьшают риск, но
не дают одного проверяемого инварианта для всех операций и всех точек отказа.

### Что уже исправлено к началу H-06

Текущий `master` уже содержит важные защиты, поэтому H-06 не должен
переписывать их:

- exception-safe отпускание текущего uinput stroke;
- XTEST cleanup для известных локальных ошибок tap/Shift;
- cleanup shortcut и uinput layout combo;
- soft-cancel normalization модификаторов;
- bounded writer transaction и terminal mutation gate;
- подтверждённый writer shutdown и fail-stop;
- освобождение `EVIOCGRAB` до потенциально долгого secondary shutdown;
- уничтожение uinput device и закрытие fd;
- conservation ledger отложенных физических событий.

Новая работа объединяет оставшиеся синтетические долги и закрывает
межпроцессный XTEST-сценарий. Она не отменяет предыдущие границы.

## Подтверждённое поведение XTEST

Проверка выполнена 2026-07-28 только внутри сохранённой
Linux Mint 22.2/Cinnamon/X11 ВМ. Хостовые `/dev/input`, `/dev/uinput`, clipboard
и рабочая сессия не использовались.

Одноразовый probe через штатные `libX11` и `libXtst` использовал `Shift_L`
с X11 keycode 50. Состояние проверялось отдельным X11-клиентом через
`XQueryKeymap`.

Обычное закрытие клиента:

```text
Shift_L keycode=50 down=false
Shift_L keycode=50 injected=down; closing-client
Shift_L keycode=50 down=true
Shift_L keycode=50 injected=up
Shift_L keycode=50 down=false
```

Принудительное завершение клиента:

```text
Shift_L keycode=50 injected=down; pid=1751
Shift_L keycode=50 down=true
client killed with SIGKILL
Shift_L keycode=50 down=true
Shift_L keycode=50 injected=up
Shift_L keycode=50 down=false
```

Следовательно:

1. закрытие X11 connection не является аналогом уничтожения uinput device;
2. `Drop` в daemon не выполняется при `SIGKILL`;
3. даже закрытие connection само по себе не снимает XTEST key state;
4. in-process RAII может быть основной cleanup-ветвью для обычной ошибки или
   panic, но не является достаточной защитой от смерти daemon;
5. процесс, способный очистить XTEST debt, должен жить независимо от основного
   daemon хотя бы до завершения cleanup.

## Цели

1. Исключить ложный success, пока остаётся неподтверждённый synthetic key debt.
2. После ошибки, timeout, cancel или panic отпустить все клавиши, для которых
   текущая операция могла выполнить synthetic down.
3. После смерти основного daemon отпустить незавершённые XTEST keys без его
   `Drop`.
4. После смерти XTEST guardian дать daemon одну независимую точечную
   cleanup-попытку и затем перейти в fail-stop.
5. Никогда не продолжать synthetic operation и не переключаться на fallback
   после неоднозначного результата мутации.
6. Не удерживать `EVIOCGRAB` в ожидании потенциально зависшего X11 cleanup.
7. Сохранить штатный input trace, порядок и существующие задержки.
8. Сделать ledger независимым от Cinnamon, X11, Wayland и конкретного desktop,
   чтобы новый backend реализовывал общий контракт, а не копировал safety-логику.
9. Сохранить package-first установку: финальный release gate проходит на точном
   Debian package.

## Не цели

В эту работу не входят:

- автоматический откат уже удалённого или напечатанного текста;
- обратное переключение уже изменившейся раскладки после hard failure;
- изменение алгоритмов автокоррекции, F12-коррекции, Caps Lock и двух заглавных;
- изменение подобранных `typing_ms`, `backspace_ms`, `layout_delay_ms`,
  X11 focus/barrier или polling параметров;
- новая поддержка KDE Plasma или другого desktop;
- переработка clipboard и selected-text engine за пределами баланса его
  синтетических Copy/Paste shortcuts;
- новый публичный D-Bus fault-injection API;
- попытка гарантировать cleanup после одновременного безусловного уничтожения
  daemon, guardian и X-сервера либо после kernel/power failure.

## Модель безопасности

### Два разных вида состояния

Нельзя смешивать:

1. **synthetic debt** — клавиша, которую OpenSwitcher начал синтетически
   нажимать и ещё не подтвердил её отпускание;
2. **desired physical state** — модификаторы, которые пользователь физически
   удерживал в согласованном snapshot и которые временно отпускались на время
   коррекции.

Synthetic cleanup освобождает только долг текущей операции. Он не перебирает
все клавиши глобальной X11 keymap и не делает безусловный `key-up` для всех
модификаторов.

На success и soft cancel desired physical state восстанавливается по уже
существующему frozen snapshot, после чего deferred physical ledger применяет
события, накопленные во время операции.

На hard failure произвольное восстановление snapshot не выполняется. В этом
состоянии snapshot может быть устаревшим, а новое synthetic down опаснее
кратковременной потери модификатора. Пользователь может отпустить и повторно
нажать физическую клавишу; залипшая клавиша или продолжение мутации недопустимы.

### Состояния одного synthetic down

Backend-neutral ledger использует как минимум следующие состояния:

```text
NotAttempted
    -> AttemptingDown
    -> PossiblyDown
    -> UpAcknowledged
```

- `NotAttempted`: разрешение на backend mutation ещё не выдано.
- `AttemptingDown`: backend-вызов начат; результат уже может быть неоднозначным.
- `PossiblyDown`: down подтверждён либо вызов вернул ошибку после начала
  мутации, поэтому release обязателен.
- `UpAcknowledged`: matching up и требуемая синхронизация подтверждены; долг
  можно удалить.

Ключ записывается в консервативное состояние **до** вызова, который способен
изменить backend. Ошибка вида «событие применено, но ACK потерян» не должна
оставить ledger пустым.

Down с неоднозначным результатом никогда автоматически не повторяется. Иначе
потерянный ACK может превратиться в дублированный ввод.

Повторный точечный key-up для остающегося долга разрешён: отпускание идемпотентно
с точки зрения цели cleanup и безопаснее сохранения возможного down.

### Operation-wide guard

`SyntheticOperation` связывает:

- backend `SyntheticKeySink`;
- `SyntheticKeyLedger`;
- transaction deadline/cancellation;
- frozen physical modifier state;
- explicit terminal outcome.

Штатное завершение обязано явно вызвать finalizer. `Drop` остаётся последней
страховкой для раннего `return` и unwind, но не единственной cleanup-ветвью.

Finalizer:

1. запрещает новые рабочие down;
2. исчерпывающе отпускает ledger в обратном порядке;
3. продолжает отпускать остальные keys после первой cleanup-ошибки;
4. выполняет требуемую backend synchronization;
5. сохраняет primary error, а cleanup error прикладывает отдельно;
6. публикует success/cancel только после доказанного reconciled state.

## Общий backend-контракт

Ledger не должен содержать `match` по Cinnamon, X11, Wayland, KDE или uinput.
Backend предоставляет узкий интерфейс, эквивалентный следующим операциям:

```text
prepare_down(key) -> backend token
attempt_down(token)
attempt_up(token)
synchronize()
terminal_proof()
```

Точное Rust API определяется планом реализации и TDD, но сохраняет эти
семантические границы.

`BackendKeyToken` непрозрачен для ledger:

- для uinput это идентичность key в текущем virtual-device generation;
- для XTEST это фактический проверенный X11 keycode и X-server/session epoch;
- будущий backend определяет собственный token.

Общий terminal proof имеет один из трёх результатов:

```text
Reconciled
OwnerGenerationDestroyed
Unreconciled
```

- `Reconciled`: matching releases подтверждены.
- `OwnerGenerationDestroyed`: owner-scoped backend generation уничтожен,
  например uinput device закрыт.
- `Unreconciled`: cleanup предпринимался, но безопасное terminal state
  доказать нельзя.

`emergency_release` не является обязательным методом каждого backend. Это
дополнительная capability XTEST-подобного adapter. Backend с owner-scoped
teardown доказывает безопасность уничтожением generation и не обязан
поддерживать release из нового executor.

Общий conformance test с `FakeThirdBackend` обязан проходить без изменения
ledger. Это release gate архитектурной расширяемости.

## uinput adapter

uinput остаётся прямым backend writer-потока:

- operation ledger закрывает ошибки до/после `write_key` и `synchronize`;
- matching releases выполняются best effort;
- при terminal writer failure устройство уничтожается;
- закрытие fd остаётся kernel-owned backstop;
- новый backend не создаётся рядом с неподтверждённо живым writer.

В эту работу не добавляется отдельный uinput process и не меняется способ
создания виртуального устройства.

## XTEST executor-guardian

### Почему guardian выполняет мутации сам

Пассивный наблюдатель с сообщениями об intent имеет неустранимую гонку:

- intent до daemon-side down может привести к ложному release, если daemon
  умрёт до события;
- intent после down может не дойти, если daemon умрёт сразу после события.

Поэтому daemon больше не имеет права самостоятельно отправлять XTEST
`key-down` или `key-up`. Guardian является единственным XTEST executor.

### Размещение

Guardian работает как socket-activated `systemd --user` service в собственной
cgroup:

```text
open-switcher-xtest-guardian.socket
    -> /usr/bin/open-switcher-daemon --internal-xtest-guardian-v1
```

Это скрытый внутренний режим того же packaged binary, а не второй исполняемый
файл. Socket unit может быть активен вместе с daemon unit, но guardian process
запускается только после реального подключения Cinnamon/X11 XKB/XTEST strategy.
На Wayland и в X11 backend без XTEST дополнительный процесс не работает.

Отдельная cgroup обязательна. Официальная семантика `KillMode=mixed` не даёт
дочернему процессу надёжного cleanup-окна: после выхода main process systemd
имеет право отправить последующий `SIGKILL` всем оставшимся процессам его
cgroup. Поэтому guardian не является child процесса daemon и не включается в
`PartOf=` или `BindsTo=` daemon unit.

Источник решения — локальная документация установленного systemd:
`systemd.kill(5)`, параметр `KillMode=`.

Socket создаётся user manager внутри `$XDG_RUNTIME_DIR`:

- `AF_UNIX` + `SOCK_SEQPACKET`;
- каталог с mode `0700`;
- socket с mode `0600`;
- `RemoveOnStop=yes`;
- guardian проверяет daemon через `SO_PEERCRED`, а daemon проверяет фактические
  response sender credentials через `SO_PASSCRED` + `SCM_CREDENTIALS`;
- сетевой listener, D-Bus name и persistent файл вне runtime directory не
  создаются.

Асимметрия обязательна для socket activation. Согласно локальной
документации `unix(7)`, `SO_PEERCRED` возвращает credentials, действовавшие при
`connect(2)`, `listen(2)` или `socketpair(2)`. Поэтому на клиентской стороне
socket-activated соединения он идентифицирует user manager, создавший listener,
а не guardian, который позднее вызвал `accept(2)`. Daemon включает
`SO_PASSCRED` до подключения и для каждого непустого ответа требует добавленный
ядром `SCM_CREDENTIALS`; UID и `(st_dev, st_ino)` фактического sender
сравниваются с текущим packaged binary. Guardian до чтения первого request
проверяет UID и тот же executable identity daemon через `SO_PEERCRED`.

Guardian обслуживает одну versioned daemon session, очищает ledger при
disconnect и завершается. Socket unit остаётся доступным для следующей
активации. На Cinnamon/X11 guardian работает рядом с daemon в течение его
XTEST-capable session; на Wayland и в не-XTEST backend дополнительного процесса
нет. Один процесс на одну session упрощает ownership и уменьшает риск того, что
старый guardian переживёт обновление package.

Ручной запуск внутреннего режима без корректного systemd activation fd и
handshake немедленно завершается без XTEST connection и без мутаций.

### Startup

Порядок Cinnamon/X11 startup:

```text
open physical device
create uinput writer
start/verify guardian socket dependency
connect to private SOCK_SEQPACKET socket
systemd activates guardian in its own cgroup
guardian opens X11 and verifies XTEST
versioned private handshake
writer-ready ACK
prepare required watchers
acquire EVIOCGRAB
publish backend ready
```

Ошибка или timeout guardian до ready происходит до физического grab.
Стратегия не объявляется доступной по одному факту успешного `spawn`.

### Протокол

Протокол:

- versioned;
- bounded по размеру каждого сообщения;
- сохраняет границы сообщений через `SOCK_SEQPACKET`;
- принимает только известные key operations, operation/session id и deadline;
- не принимает shell command, произвольный путь, переменную окружения или
  неограниченную строку;
- использует монотонные deadlines;
- не пишет keycode, текст или содержимое коррекции в обычный лог.

Минимальные сообщения:

```text
Hello(protocol, session_epoch) -> Ready
PrepareKey(operation_id, sequence, logical_key) -> PreparedToken
ExecuteDown(operation_id, sequence, prepared_token, deadline) -> DownAck
KeyUp(operation_id, sequence, token, deadline) -> UpAck
Synchronize(operation_id, sequence, deadline) -> SyncAck
TransferToPhysicalDebt(operation_id, tokens, input_generation) -> TransferAck
PhysicalReleaseCommitted(input_sequence, token, input_generation) -> ReleaseCommitAck
CancelAndDrain(operation_id) -> Drained
ReleaseAllAndExit -> Stopped
Fatal(reason)
```

Guardian хранит собственный authoritative ledger. Daemon хранит зеркальный
набор `attempted/possibly-down` для аварии guardian.

Перед XTEST down:

1. daemon запрашивает немутирующий `PrepareKey`;
2. guardian проверяет logical key, текущую X11 mapping и server/session epoch;
3. guardian возвращает подписанный текущей session opaque token и фактический
   X11 keycode;
4. daemon консервативно записывает prepared token как possible debt;
5. guardian принимает `ExecuteDown` только для собственного prepared token;
6. guardian фиксирует локальное `AttemptingDown`;
7. guardian выполняет XTEST request, `check()` и `flush()`;
8. только после подтверждения возвращает ACK;
9. daemon отмечает down подтверждённым, но сохраняет долг до matching up.

Произвольный X11 keycode, присланный клиентом без `PrepareKey`, отклоняется.
Если guardian погиб после XTEST down, но до ACK, daemon уже знает точный token и
не повторяет down.

Перед XTEST up guardian не удаляет token. Он удаляется только после
подтверждённого release и round-trip. Потеря ACK оставляет token в одном или
обоих ledger и запускает release-only reconciliation, но не повтор down.

Существующие deliberate delays остаются в текущих местах операции. IPC не
добавляет новых `sleep` и не меняет значения 1/2/N ms.

Per-mutation ACK добавляет локальный IPC round-trip, поэтому wall-clock timing
не объявляется заранее идентичным. Перед merge обязательны:

- microbenchmark protocol round-trip в Mint VM;
- сравнение серии одинаковых реальных коррекций до/после;
- отсутствие transaction timeout на максимально допустимом correction plan;
- отсутствие заметной пользователю паузы на обычном слове.

Если p95 protocol round-trip превышает 1 ms либо медиана одинаковой
end-to-end коррекции ухудшается более чем на 10%, bounded пакетизация XTEST
mutations становится условием merge, а не откладывается как будущая
оптимизация. Пакетизация не имеет права менять общий ledger-контракт, input
trace или deliberate delays.

### Удерживаемые физические модификаторы

Успешная коррекция сохраняет нынешнюю семантику:

1. frozen modifiers временно отпускаются;
2. выполняются backspace и replay;
3. frozen modifiers синтетически восстанавливаются;
4. после `TransferToPhysicalDebt` эти down переходят из operation-temporary в
   session-scoped physical debt guardian;
5. debt остаётся у guardian, пока соответствующий реальный physical release не
   будет записан и синхронизирован writer;
6. только writer ACK позволяет daemon отправить `PhysicalReleaseCommitted`;
7. после `ReleaseCommitAck` guardian удаляет session debt без дополнительного
   XTEST up, потому что подтверждённый uinput release уже завершил состояние.

Для key с session debt действует строгий порядок одного writer:

```text
uinput physical release write
uinput synchronize
PhysicalReleaseCommitted
ReleaseCommitAck
следующая synthetic/physical mutation того же key
```

Следующий press того же key нельзя переслать до `ReleaseCommitAck`. Иначе
guardian cleanup старого долга мог бы отпустить уже новое физическое нажатие.
Потеря commit ACK переводит writer в terminal reconciliation, а не разрешает
ему продолжить очередь.

Простой operation-end commit, удаляющий restored modifier из guardian ledger,
запрещён. Пользователь мог отпустить modifier во время grab, а release мог
остаться в deferred queue. Если daemon погибнет до его writer ACK, kernel
ungrab не обязан заново доставить уже произошедшее событие X-серверу.

Если daemon погибает до `PhysicalReleaseCommitted`, guardian видит disconnect и
явно отпускает session-scoped debt. Это может временно снять физически
удерживаемый modifier, но не оставляет неподтверждённый XTEST down без владельца.

Daemon также зеркалит session-scoped debt до guardian ACK фактического physical
release. Input generation и physical event sequence не позволяют ACK от старого
backend удалить долг нового поколения.

Session-scoped modifier debt имеет собственные состояния:

```text
OwnedDown
TemporarilyReleased
RestoringPossiblyDown
ReleasedByPhysicalAck
```

Это необходимо для повторной коррекции, пока пользователь продолжает удерживать
тот же modifier. Подтверждённый временный XTEST up переводит `OwnedDown` в
`TemporarilyReleased`, поэтому аварийный drain не отправляет лишний release для
уже отпущенного key. Попытка восстановления переводит его в
`RestoringPossiblyDown`, а подтверждённый down возвращает `OwnedDown`.
Фактический physical release после writer ACK завершает долг.

Один и тот же token не может одновременно находиться в temporary operation
ledger и session ledger без явного transfer transition.

## Двусторонняя аварийная защита

### Смерть daemon

При panic процесса, `SIGTERM`, `SIGKILL`, writer crash или закрытии IPC:

1. ядро закрывает daemon-side socket;
2. guardian видит EOF;
3. запрещает новые down;
4. исчерпывающе отпускает только собственный ledger;
5. выполняет X11 round-trip;
6. выходит.

Это не зависит от Rust `Drop` в основном daemon.

### Смерть guardian

Daemon отмечает возможные XTEST tokens до отправки мутации. Если guardian
завершился, завис, потерял канал или не вернул ACK в deadline:

1. terminal gate запрещает новые synthetic mutations;
2. физический grab освобождается независимо от результата XTEST cleanup;
3. daemon передаёт только mirrored possible tokens изолированному
   release-only cleanup worker;
4. worker использует заранее открытое daemon-side X11 emergency connection,
   подтверждённое как та же server/session epoch во время startup handshake;
5. controller ждёт worker только до hard cleanup deadline;
6. при deadline весь daemon process завершается, поэтому зависший worker не
   удерживает physical grab и не переживает fail-stop;
7. writer считается terminally failed;
8. fallback на uinput и продолжение текущей операции запрещены;
9. процесс выходит с ошибкой для чистого systemd restart.

Новая X11 connection после guardian failure не открывается вслепую. Token
старого X-server epoch нельзя применять к новой сессии: там такой key-up может
вмешаться уже в другое физическое состояние. Если pre-established emergency
connection потеряна или epoch нельзя подтвердить, результат немедленно
`Unreconciled`.

Hard deadline ограничивает ожидание controller, а не обещает прервать
зависнувший X11 syscall внутри Rust thread. Безопасная граница достигается тем,
что grab освобождён до ожидания, а завершение daemon уничтожает весь cleanup
worker вместе с процессом.

Emergency release может отправить key-up для неоднозначного down, который
guardian не успел применить. В аварийной ветви это способно кратковременно
снять физически удерживаемый key с тем же X11 keycode. Область ограничена
точными tokens незавершённой операции; глобальное освобождение modifiers
запрещено. Такой результат безопаснее возможного залипания.

### Зависание X11

Ни daemon, ни guardian не могут гарантированно заставить зависший X-сервер
обработать release. Поэтому:

- active operation имеет абсолютный deadline;
- guardian проверяет EOF/cancel между backend-вызовами и interruptible waits;
- отсутствие XTEST ACK не удерживает `EVIOCGRAB`;
- daemon-side emergency connection создаётся и сверяется до physical grab;
- emergency cleanup получает один hard-bounded wait cycle;
- при `Unreconciled` не запускается второй backend в том же процессе.

Если сам системный X-сервер не отвечает, рабочий стол уже находится вне
контроля OpenSwitcher. Программа обязана прекратить захват физического
устройства, а не ждать X11 бесконечно.

## systemd stop/restart

Добавляются два package unit:

```text
open-switcher-xtest-guardian.socket
open-switcher-xtest-guardian.service
```

Daemon unit получает `Wants=` и `After=` только на guardian socket. Guardian
service намеренно не является частью daemon cgroup и не получает `PartOf=` или
`BindsTo=` на daemon.

При `systemctl stop open-switcher-daemon.service`:

1. основной daemon завершается, а kernel закрывает его socket;
2. guardian service продолжает жить в собственной cgroup;
3. EOF запрещает новые down и запускает cleanup;
4. после `Drained` guardian process завершается;
5. socket unit может остаться ожидающим следующего запуска daemon.

При daemon failure и `Restart=on-failure` новый экземпляр подключается только к
очищенной новой session. Если старый guardian ещё завершает drain, новое
соединение остаётся в bounded socket queue либо отклоняется как busy; второй
XTEST executor не начинает мутации до завершения старого epoch.

Явные stop/update/remove scripts обязаны соблюдать порядок:

```text
stop daemon
wait bounded guardian drain/exit
stop guardian service and socket
```

Guardian обрабатывает собственный `SIGTERM` как `CancelAndDrain`, поэтому
явная остановка guardian unit не пропускает обычный cleanup. Если guardian не
может завершить cleanup в выбранный stop budget, daemon уже не удерживает
физическое устройство, а outcome остаётся `Unreconciled`.

Явные timeout значения выбираются в плане реализации из существующего
максимального transaction deadline плюс измеренного cleanup budget.
Произвольные значения «на глаз» не вводятся.

После package update старый daemon сначала отключается. Его guardian очищает
старую session и выходит; следующая socket activation запускает уже новый
binary. Protocol version и session epoch дополнительно запрещают смешивание
старого guardian и нового daemon.

Все unit-копии в Debian source и `dist/systemd`, а также install/update/remove
scripts должны изменяться согласованно.

## Терминальная семантика

| Сценарий | Требуемый результат |
|---|---|
| Success | Прежний input trace; temporary ledger reconciled; desired physical state восстановлен; success после ACK |
| Soft cancel до мутации | Нет synthetic release; операция возвращает `Cancelled` |
| Soft cancel после мутации | Temporary keys отпущены; frozen physical state восстановлен; затем `Cancelled` |
| Backend error/timeout после down | Новые down запрещены; exhaustive release-only cleanup; writer fail-stop |
| Частично изменённый текст/layout | Не откатывается; фиксируется как допустимый остаточный эффект hard failure |
| Panic writer thread | Guardian handle/operation guard закрываются; guardian drain; controller освобождает grab |
| `SIGTERM`/`SIGKILL` daemon | Guardian очищает по EOF; kernel закрывает uinput/grab; systemd может перезапустить daemon |
| Guardian crash/timeout | Daemon делает точечный emergency release, освобождает grab и выходит с ошибкой |
| Потерянный ACK | Down не повторяется; possible debt очищается с обеих доступных сторон |
| Cleanup частично упал | Остальные releases продолжаются; outcome `Unreconciled`; ложного safe completion нет |
| systemd stop/restart | Guardian получает окно для drain; второй backend не пересекается со старым |

Primary error не маскируется secondary cleanup error. Оба должны быть доступны
в typed error/postmortem, но обычный лог не содержит восстановленного текста или
полной последовательности клавиш.

## Архитектурная расширяемость

Guardian не является новым обязательным слоем для каждого desktop. Это
containment adapter только для API, у которого synthetic key state не связан с
временем жизни fd.

При добавлении KDE Plasma или другого backend:

1. desktop-specific observation/layout adapter остаётся за пределами ledger;
2. если ввод по-прежнему идёт через uinput, новый guardian не нужен;
3. если новый API имеет собственное owner-scoped teardown, он реализует
   `SyntheticKeySink` напрямую;
4. если API повторяет небезопасную XTEST-семантику, его executor может
   использовать тот же guardian protocol;
5. общий ledger, operation finalizer и failure-at-N matrix не меняются.

Запрещается добавлять в ledger ветви вида `if Cinnamon`, `if KDE` или
`if Wayland`.

## Безопасность границы IPC

Guardian не расширяет системные права OpenSwitcher:

- работает под тем же пользователем;
- не открывает `/dev/input` и `/dev/uinput`;
- не принимает сетевые соединения;
- принимает только runtime `AF_UNIX` socket с mode `0600`;
- не выполняет внешние команды;
- не читает пользовательскую конфигурацию или clipboard;
- получает X11 доступ только из уже существующего session environment;
- internal mode без приватного handshake не выполняет мутаций.

Protocol parser обязан:

- отклонять неизвестную версию, operation id, sequence или message kind;
- ограничивать длину и количество tokens;
- запрещать stale session epoch;
- завершать с fatal error при нарушении порядка;
- не использовать небезопасную автоматическую десериализацию
  неограниченного ввода;
- не логировать message payload с key trace.

## Сохранение пользовательской функциональности

Неизменными остаются:

- ручная коррекция последнего слова через F12;
- автоматическое переключение раскладки;
- same-layout исправления Caps Lock и двух заглавных букв;
- переключение EN/RU и Cinnamon XKB group;
- коррекция слова с буквами, знаками и Shift;
- Copy/Paste selected text flow;
- Enter, Tab, pointer/focus и остальные причины сброса контекста;
- подобранные задержки коррекции и X11 focus behavior.

Нормальный XTEST event trace до и после изменения должен совпадать по press,
release и порядку. IPC-сообщения не являются input events.

H-06 не считается завершённым только по unit-тестам. Точная production-сборка
Debian package должна подтвердить эти функции в Mint/Cinnamon/X11 и обычный
uinput путь в Ubuntu/GNOME/Wayland.

## Стратегия тестирования

### Unit: общий ledger

Table-driven failure-at-operation-N проверяет:

- fail до mutation;
- событие применено и backend вернул error;
- событие применено, но ACK потерян;
- fail до/после up;
- fail synchronization;
- cancel и timeout между каждым переходом;
- cleanup failure с продолжением остальных releases;
- отсутствие мутаций после terminal state;
- отсутствие повторного down после ambiguous result.

Короткая репрезентативная матрица:

1. backspace и обычная буква;
2. temporary Shift и shifted symbol;
3. shortcut/layout modifiers;
4. synthetic separator.

Не требуется отдельный тест для каждой буквы алфавита: безопасность определяется
переходами ledger, а не символом.

### Unit: функциональная эквивалентность

Golden traces до/после общего executor должны совпасть для:

- switching correction;
- same-layout correction;
- Copy и Paste shortcuts;
- layout combo;
- separator.

Success и soft cancel заканчиваются согласованным physical snapshot. Hard
failure заканчивается пустым temporary synthetic set либо явным
`Unreconciled`, но никогда ложным success.

Отдельная state-machine matrix проверяет session-scoped modifier debt:

- restored modifier не исчезает из guardian ledger в конце операции;
- deferred physical release удаляет debt только после writer write+sync ACK;
- потерянный release ACK оставляет debt до guardian cleanup;
- daemon death между restore и deferred release приводит к guardian key-up;
- повторная коррекция при продолжающемся physical hold корректно проходит
  `OwnedDown -> TemporarilyReleased -> OwnedDown`;
- новый press того же modifier запрещён до `ReleaseCommitAck`;
- input generation/sequence старого backend не подтверждает release нового.

### Unit: расширяемость

`FakeThirdBackend` реализует общий sink и проходит ту же conformance matrix без
изменения ledger. Этот тест обязателен перед утверждением архитектуры как
готовой к будущему desktop/backend.

### Process integration без реального ввода

Отдельные tests используют настоящий socket-activated-compatible IPC lifecycle
и fake executor:

1. штатное закрытие daemon-side channel очищает guardian debt;
2. panic daemon surrogate закрывает channel и guardian выходит;
3. `SIGKILL` daemon surrogate после down ACK приводит к release и bounded exit
   guardian;
4. потерянный ACK не повторяет down и приводит к cleanup;
5. `SIGKILL` guardian после down приводит к daemon emergency cleanup;
6. guardian cleanup error не публикует safe completion;
7. stale epoch и нарушенная sequence отклоняются;
8. daemon death после `TransferToPhysicalDebt`, но до physical release ACK,
   приводит к guardian release;
9. guardian service session завершается, zombie/process leak не остаётся.

Эти tests не открывают X11, `/dev/input` или `/dev/uinput`.

### Debian package checks

Проверяется:

- hidden guardian mode находится в том же packaged daemon binary;
- отдельный guardian binary случайно не появился;
- guardian socket/service находятся в отдельных units и собственной cgroup;
- daemon зависит только от socket, но не связывает lifecycle guardian service
  через `PartOf=`/`BindsTo=`;
- все копии units содержат согласованную lifecycle policy;
- package install/update/remove scripts останавливают units в безопасном
  порядке;
- ручной запуск internal mode без handshake безопасно завершается;
- package build проходит существующие base, settings-ui, D-Bus и shell tests.

### Mint/Cinnamon/X11 package-first VM

На точном production DEB:

1. обычная матрица: F12, auto correction, Caps Lock, две заглавные, separator,
   shifted symbol и layout switch;
2. после каждой операции `XQueryKeymap` не показывает лишнего synthetic down;
3. daemon уничтожается после реального XTEST down; guardian отпускает key,
   пользовательский ввод восстанавливается, systemd restart не создаёт
   пересекающийся backend;
4. guardian уничтожается после реального XTEST down; daemon обнаруживает канал,
   выполняет emergency release и fail-stop;
5. normal stop/restart повторяется серией paced циклов без orphan guardian,
   лишнего процесса, fd или залипшей клавиши.

Точный момент аварии определяется внешним VM harness по X11 key state и PID
процессов. Production binary не получает публичный fault-injection API.

### Ubuntu/GNOME/Wayland package-first VM

Один полный функциональный smoke подтверждает, что общий ledger не изменил
uinput path. XTEST-аварии не выдаются за проверенные в Wayland.

### Хост

На рабочем хосте не запускаются аварийные input tests. После VM и package
verification пользователь постепенно проверяет обычное поведение установленного
DEB. Ручная проверка не заменяет автоматические safety gates.

## Release gates

Реализация готова к merge только если:

1. normal trace equivalence доказана;
2. failure-at-N ledger matrix проходит;
3. daemon panic/SIGKILL process tests подтверждают guardian cleanup;
4. guardian death подтверждает terminal emergency outcome:
   `Reconciled` либо явный `Unreconciled`;
5. lost ACK не повторяет down;
6. cleanup failure не публикует safe outcome;
7. fake third backend проходит общий contract;
8. physical grab release не зависит от успешности X11 cleanup;
9. точный DEB проходит Mint XTEST и Ubuntu uinput smoke;
10. package unit/install/update/remove checks проходят;
11. в отчёте явно перечислены остаточные непроверенные сценарии.

## Отклонённые варианты

### Только локальные cleanup-ветви

Не закрывают `SIGKILL`, потерянный ACK и новые синтетические пути. Сложность
растёт вместе с числом ручных `match`/`return`.

### Пассивный guardian

Неустранимая гонка между intent и daemon-side XTEST mutation. Guardian должен
владеть самой инъекцией.

### Дочерний guardian в cgroup daemon

Проще с точки зрения package, но не обеспечивает заявленную safety boundary.
При `KillMode=control-group` systemd посылает termination всей cgroup. При
`KillMode=mixed` начальный `SIGTERM` получает main process, однако после его
выхода последующий `SIGKILL` относится ко всем оставшимся процессам cgroup.
Следовательно, child guardian может быть уничтожен до подтверждённой очистки.

`KillMode=process` и `KillMode=none` также отклонены: они позволяют процессам
уйти из нормального lifecycle service manager и создают риск пересечения старого
guardian с новым daemon.

Socket-activated service в собственной cgroup — минимальная схема, которая
действительно переживает остановку или смерть daemon. Дополнительные package
units принимаются как оправданная цена подтверждённой XTEST-семантики.

### Выполнять XTEST в daemon и лишь дублировать ledger

Оставляет окно смерти между mutation и уведомлением guardian.

### Отказаться от XTEST и перейти на uinput в Cinnamon

XKB/XTEST strategy была введена после подтверждённой неработоспособности
Cinnamon D-Bus методов и прошла реальные package-first проверки. Замена
инъекции без отдельного доказательства per-device XKB semantics создаёт больший
риск возврата частичной или неправильной коррекции.

### Освобождать все модификаторы

Способно вмешаться в физически удерживаемые клавиши пользователя. Cleanup
ограничивается точными tokens текущего ledger.

### Повторять down после timeout или потери ACK

Может продублировать уже применённое событие. Неоднозначный down ведёт только к
release/reconciliation.

### Полностью перепланировать все операции как глобальные batches

Это слишком широкая перестройка хрупкого рабочего input flow. Общий ledger и
узкий sink позволяют закрыть H-06 без изменения алгоритмов коррекции.
Внутренняя пакетизация IPC остаётся допустимой будущей оптимизацией, если
измерения реально покажут необходимость.

## Остаточные риски

После реализации остаются честно ограниченные сценарии:

- одновременный `SIGKILL` daemon и guardian либо
  `systemctl kill --kill-whom=all -s KILL`;
- одиночный `SIGKILL` guardian вместе с отказом или зависанием независимой
  daemon-side emergency connection;
- kernel hang, power loss или завершение всего X-сервера;
- X-сервер, который применил событие и навсегда перестал принимать cleanup;
- кратковременное ложное отпускание физически удерживаемого key при
  консервативной очистке неоднозначного down;
- невозможность доказать все аппаратные и compositor варианты только двумя
  сохранёнными ВМ.

Добавление третьего watcher не устраняет одновременную смерть всех userspace
процессов и создаёт бесконечную цепочку guardians. Абсолютная гарантия возможна
только у owner-scoped/kernel-owned backend. Для существующей Cinnamon XTEST
ветви выбран практический single-fault-safe контракт:

- смерть daemon закрывает guardian;
- смерть guardian всегда прекращает новые операции и закрывает daemon;
- guardian debt либо подтверждённо очищается, либо получает явный
  `Unreconciled`;
- физический grab в обоих случаях освобождается;
- продолжение неизвестной synthetic state запрещено.

## Критерий завершения H-06

H-06 можно считать закрытым, когда для каждого production synthetic path
доказано одно из двух:

```text
success -> input trace завершён и terminal state согласован
failure -> новые mutations запрещены, synthetic debt очищен
           либо явно Unreconciled, physical grab освобождён,
           текущий процесс не продолжает работу
```

Механизм не объявляется абсолютно fail-safe относительно одновременного
уничтожения всех userspace участников. Он должен полностью reconciliate
одиночную ошибку операции, panic/thread death, гибель основного daemon и
штатный systemd stop/restart. При одиночной гибели guardian обязательны
немедленный запрет новых мутаций, освобождение physical grab и terminal
`Reconciled` либо честный `Unreconciled`; ложное продолжение работы запрещено.
