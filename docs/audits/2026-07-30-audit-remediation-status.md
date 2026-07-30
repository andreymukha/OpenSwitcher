# Актуальный статус исправлений аудита OpenSwitcher

**Дата:** 2026-07-30

**Проверенный `master`:**
`2434c95a5d4acbe46dad624c7db3625fd729792d`

**Исходный аудит:**
`docs/audits/2026-07-15-openswitcher-deep-read-only-audit.md`

## Назначение документа

Этот документ заменяет устаревшее представление о статусах из первоначального
roadmap. Он сопоставляет каждое исходное замечание с текущим кодом, историей
исправлений и уже сохранёнными validation reports.

Это не новая полная runtime-кампания. При составлении статуса не выполнялись
операции с физическими устройствами, clipboard, layout, systemd, udev или ACL.

Статусы означают:

- **закрыто** — исходная подтверждённая первопричина устранена и имеет
  автоматические либо package-first runtime-доказательства;
- **частично закрыто** — опасная часть устранена, но один исходный сценарий
  остался без полного решения или доказательства;
- **открыто** — исходная первопричина по-прежнему присутствует в текущем коде.

## Сводка

| Исходная серьёзность | Закрыто | Частично закрыто | Открыто |
|---|---:|---:|---:|
| Critical | 1 | 0 | 0 |
| High | 6 | 1 | 1 |
| Medium | 3 | 0 | 7 |
| Low | 2 | 0 | 0 |
| **Всего** | **12** | **1** | **8** |

Полностью открытых Critical больше нет. Единственный полностью открытый High —
H-07. H-02 остаётся частично закрытым и требует отдельной проверки
pre-grab-очереди.

## Critical и High

| Finding | Статус | Текущее основание |
|---|---|---|
| C-01 — пользовательские пути достигают root mutation | **Закрыто** | Source-tree bootstrap стал read-only, privileged paths привязаны к установленному DEB. Проверено shell gates и двумя package-first VM-профилями. |
| H-01 — неограниченная writer-транзакция удерживает grab | **Закрыто** | Введены operation-wide deadlines, cancellation, interruptible waits и bounds persisted delays/correction schedule. Grab-critical loop использует подтверждённый snapshot вместо внешнего I/O. |
| H-02 — grab до готовности sink и события между open/grab | **Частично закрыто** | Writer и обязательные watchers теперь готовы до `EVIOCGRAB`; partial-init после grab устранён. Но keyboard fd всё ещё открывается до подготовки pipeline, а накопившаяся до grab evdev-очередь явно не очищается и не согласуется. |
| H-03 — writer продолжает ввод после shutdown | **Закрыто** | Shutdown имеет ACK и bounded join. При неподтверждённой остановке запрещён новый backend, grab освобождается, а daemon переходит в process fail-stop. |
| H-04 — abort/overflow теряет принятые физические события | **Закрыто** | Добавлены bounded deferred queue, acknowledge/reconciliation и X11 generation barrier. Проверены error/overflow/recovery paths и exact DEB в двух VM. |
| H-05 — D-Bus capture без owner/lease | **Закрыто** | Capture привязан к unique D-Bus owner, имеет soft/absolute lease, heartbeat, owner-loss cancellation и сбалансированный suppression debt. |
| H-06 — synthetic key sequences не exception-safe | **Закрыто** | Operation/session ledger, uinput/XTEST fail-safe contract и отдельный guardian покрыты failure-at-step, process и VM crash tests. |
| H-07 — legacy backend выдумывает US/RU | **Открыто** | `detect_legacy_layout_pair()` всё ещё вызывает `default_legacy_layout_pair()` при ошибке `setxkbmap`, отсутствии `layout:` или неполной паре, после чего backend объявляет `StrictPair`. |
| H-08 — blanket ACL между sessions/seats | **Закрыто** | ACL bridge удалён. Используются `uaccess`, проверка единственной active local graphical session, seat-bound identity и fail-closed session lease. |

### Остаток H-02

Текущая последовательность в `KeyboardController::prepare/activate`:

```text
open physical evdev fd
  -> create writer
  -> start watchers
  -> verify readiness
  -> EVIOCGRAB
```

Это закрывает наиболее опасную часть исходного H-02: grab больше не возникает
рядом с неготовым forwarding sink. Однако события, пришедшие между `open` и
`EVIOCGRAB`, уже были доставлены desktop и могли накопиться в очереди нового
fd. Если после grab OpenSwitcher прочитает и перешлёт их повторно, возможны
дублирование и перестановка первых символов при startup/recovery.

Точное поведение следует проверить в VM с управляемой pre-grab-инъекцией. До
runtime-доказательства этот остаток нельзя выдавать ни за подтверждённую
регрессию текущего DEB, ни за полностью устранённую первопричину.

## Medium

| Finding | Статус | Текущее основание |
|---|---|---|
| M-01 — clipboard не восстанавливается на ранних ошибках | **Открыто** | После установки sentinel ошибки `copy_selection`, чтения, записи converted text или `paste_selection` выходят через `?` без общего rollback guard. |
| M-02 — non-text clipboard теряется | **Открыто** | Snapshot хранит только `Text` либо `Unavailable`; MIME/image payload не сохраняется, а restore для `Unavailable` очищает clipboard. |
| M-03 — restore затирает конкурентное изменение clipboard | **Открыто** | После фиксированного settle restore выполняется без проверки текущего owner/content generation. |
| M-04 — конфигурация записывается неатомарно | **Открыто** | `AppConfig::save_to_path()` по-прежнему использует прямой `fs::write(path, content)` без temp file, `fsync` и atomic rename. |
| M-05 — stale D-Bus settings update теряет изменения | **Открыто** | API по-прежнему принимает полный `SettingsDto` без revision/CAS; внутренний lock сериализует только момент записи. |
| M-06 — stale PID file может получить чужой PID | **Открыто** | `manage.sh::is_running()` проверяет PID только через `kill -0`, после чего `stop_component()` может послать ему SIGTERM/SIGKILL без проверки executable/start time. |
| M-07 — keyboard poll игнорирует HUP/ERR/NVAL | **Открыто** | `wait_for_device_input()` по-прежнему возвращает только наличие `POLLIN`; положительный poll только с `POLLHUP`, `POLLERR` или `POLLNVAL` превращается в обычный timeout. |
| M-08 — риск владения fd в старом `uinput 0.1.3` | **Закрыто в исходном lifecycle scope** | Dependency зафиксирована exact version и локально patched: `Builder` и `Device` закрывают fd, передача ownership явная, repeated recovery проверен. Deprecated API остаётся maintenance/lint debt, но исходная утечка fd устранена. |
| M-09 — package lifecycle скрывает stop/ACL failures | **Закрыто** | Stop имеет deadline и postcondition; upgrade/remove/purge проверяют отсутствие старого process и детерминированно очищают manifest/ACL/tag state. |
| M-10 — неограниченный word buffer/correction cost | **Закрыто** | Word tracking ограничен `MAX_CORRECTION_KEYSTROKES = 128`; после overflow физический ввод сохраняется, а destructive correction отключается до следующей реальной границы. |

## Low

| Finding | Статус | Текущее основание |
|---|---|---|
| L-01 — небезопасные predictable debug paths | **Закрыто** | Общий bounded debug hub открывает sink с `O_NOFOLLOW`, проверяет regular file и effective UID, принудительно ставит mode `0600`; producer не блокирует input path. |
| L-02 — два источника udev rule | **Закрыто** | DEB устанавливает одно каноническое `70-openswitcher-input.rules`; старое `80-*` и ACL bridge удалены. |

## Уже подтверждённые общие свойства

На текущей линии исправлений имеются следующие evidence:

- полный Rust gate: 973 passed, 1 ignored;
- package shell gates и `git diff --check`;
- exact DEB `0.1.0-4` в Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland;
- active upgrade, reinstall, remove и purge;
- session deactivation/recovery и освобождение input backend;
- F12, auto correction, исправление двух заглавных и accidental Caps Lock;
- H-06 daemon/guardian crash evidence на предыдущем exact code slice;
- пользовательский smoke установленного `0.1.0-4` на host.

Этого достаточно для продолжения remediation без немедленного повторения всей
дорогой runtime-кампании. Это не заменяет финальную проверку после последних
поведенческих изменений.

## Приоритет продолжения

1. **H-07:** сделать legacy detection fail-closed. Неподтверждённая пара не
   должна разрешать destructive auto/manual correction.
2. **H-02 residual + M-07:** отдельный небольшой input-recovery slice:
   определить политику pre-grab queue и обрабатывать `POLLHUP/ERR/NVAL` как
   typed device loss.
3. **M-01..M-03:** одна транзакционная clipboard/selected-text граница с
   rollback guard, сохранением поддерживаемых форматов и условным restore.
4. **M-04 + M-05:** crash-safe config commit и revision/CAS для настроек.
5. **M-06:** безопасная process identity для development lifecycle.
6. После последнего изменения — одна объединённая финальная кампания.

После каждого slice выполняются только его regression tests, общий Rust/package
gate и минимальный релевантный VM smoke. Полная двухпрофильная кампания не
повторяется после каждого небольшого изменения.

## Граница финальной кампании

Финальный exact DEB должен пройти:

1. полный автоматический Rust/shell/package gate;
2. install/upgrade/reinstall/remove/purge в обеих VM;
3. обычную функциональную матрицу X11 и Wayland;
4. повтор ключевых H-06/H-08 single-fault сценариев на объединённом коде;
5. управляемый pre-grab queue test и unplug/replug для H-02/M-07;
6. clipboard failure-at-every-step и concurrent-owner сценарии;
7. suspend/resume, если VM/desktop даёт воспроизводимую границу;
8. ограниченный host happy-path acceptance без опасной fault injection.

Одновременная гибель всех userspace-компонентов, kernel D-state, отказ всего
X server и power loss остаются пределами userspace-гарантий, а не обещанием
OpenSwitcher.

VM-лаборатория, диски и evidence сохраняются до отдельной прямой просьбы
пользователя об удалении.
