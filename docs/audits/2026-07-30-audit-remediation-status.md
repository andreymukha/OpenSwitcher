# Актуальный статус исправлений аудита OpenSwitcher

**Дата:** 2026-08-11

**База `master`:**
`83e871009f8210b4b8bedfcc61ec391e640c41c4`

**Проверенный code candidate:**
`77e2c3af9fe548c89b9dfa15a69e550fad53cd27`

**Исходный аудит:**
`docs/audits/2026-07-15-openswitcher-deep-read-only-audit.md`

## Назначение документа

Этот документ заменяет устаревшее представление о статусах из первоначального
roadmap. Он сопоставляет каждое исходное замечание с текущим кодом, историей
исправлений и уже сохранёнными validation reports.

Это не новая полная двухпрофильная runtime-кампания. Статус обновлён по
сохранённым validation reports, включая целевую H-02/M-07 проверку,
двухпрофильную проверку clipboard-транзакции и package-first M-04/M-05 smoke.
Физические устройства, clipboard, layout, systemd, udev и ACL хоста не
затрагивались.

Статусы означают:

- **закрыто** — исходная подтверждённая первопричина устранена и имеет
  автоматические либо package-first runtime-доказательства;
- **частично закрыто** — опасная часть устранена, но один исходный сценарий
  остался без полного решения или доказательства;
- **принятое продуктовое поведение** — исходный технический риск сознательно
  принят в явно ограниченной форме, потому что иное поведение ухудшает основную
  пользовательскую функцию;
- **открыто** — исходная первопричина по-прежнему присутствует в текущем коде.

## Сводка

| Исходная серьёзность | Закрыто | Принято | Частично закрыто | Открыто |
|---|---:|---:|---:|---:|
| Critical | 1 | 0 | 0 | 0 |
| High | 8 | 0 | 0 | 0 |
| Medium | 8 | 1 | 0 | 1 |
| Low | 2 | 0 | 0 | 0 |
| **Всего** | **19** | **1** | **0** | **1** |

Открытых и частично закрытых Critical/High больше нет. Из исходного аудита
открыт только M-06. M-02 отдельно зафиксирован как принятое продуктовое
поведение, а не выдан за полное техническое восстановление произвольного MIME.

## Critical и High

| Finding | Статус | Текущее основание |
|---|---|---|
| C-01 — пользовательские пути достигают root mutation | **Закрыто** | Source-tree bootstrap стал read-only, privileged paths привязаны к установленному DEB. Проверено shell gates и двумя package-first VM-профилями. |
| H-01 — неограниченная writer-транзакция удерживает grab | **Закрыто** | Введены operation-wide deadlines, cancellation, interruptible waits и bounds persisted delays/correction schedule. Grab-critical loop использует подтверждённый snapshot вместо внешнего I/O. |
| H-02 — grab до готовности sink и события между open/grab | **Закрыто** | Runtime fd открывается только после подготовки writer/watchers. Bounded quiet handoff отбрасывает pre-grab пакеты, запрещает grab при held/continued input и повторяет попытку через lifecycle. Unit и package-first Mint/QEMU held-key + burst прошли без потерь и дублей. |
| H-03 — writer продолжает ввод после shutdown | **Закрыто** | Shutdown имеет ACK и bounded join. При неподтверждённой остановке запрещён новый backend, grab освобождается, а daemon переходит в process fail-stop. |
| H-04 — abort/overflow теряет принятые физические события | **Закрыто** | Добавлены bounded deferred queue, acknowledge/reconciliation и X11 generation barrier. Проверены error/overflow/recovery paths и exact DEB в двух VM. |
| H-05 — D-Bus capture без owner/lease | **Закрыто** | Capture привязан к unique D-Bus owner, имеет soft/absolute lease, heartbeat, owner-loss cancellation и сбалансированный suppression debt. |
| H-06 — synthetic key sequences не exception-safe | **Закрыто** | Operation/session ledger, uinput/XTEST fail-safe contract и отдельный guardian покрыты failure-at-step, process и VM crash tests. |
| H-07 — legacy backend выдумывает US/RU | **Закрыто** | Неподтверждённый setup теперь fail-closed; destructive correction требует подтверждённые setup/current group. Проверены normal, transient, extra-layout и X11 same-count change в Mint/X11 и Ubuntu/Wayland на exact DEB `0.1.0-5`. |
| H-08 — blanket ACL между sessions/seats | **Закрыто** | ACL bridge удалён. Используются `uaccess`, проверка единственной active local graphical session, seat-bound identity и fail-closed session lease. |

### Закрытие H-02 и M-07

На exact DEB `0.1.0-6` в Mint/Cinnamon/X11 подтверждено:

- held `Shift` не допускает grab;
- pre-grab пакеты обнаруживаются и отбрасываются;
- прямой ввод до grab и forwarded ввод после grab появляются по одному разу;
- 600-ms restart-burst не теряется и не дублируется;
- три `testkbd` unplug дали typed `poll events=0x18`;
- три replug восстановили тот же PID;
- event fd вернулись с 0 к стабильным 2, общий fd count остался 23,
  virtual backend — ровно один;
- после recovery прошли F12 и проверка освобождения модификаторов.

Полный отчёт:
`docs/audits/2026-07-31-h02-m07-input-handoff-validation.md`.

## Medium

| Finding | Статус | Текущее основание |
|---|---|---|
| M-01 — clipboard не восстанавливается на ранних ошибках | **Закрыто** | `ClipboardTransaction` фиксирует намерение до внешней записи и выполняет единый условный rollback на ранних `?` и при unwind через `Drop`. Failure-at-every-step/panic matrix и package-first smoke в обеих VM пройдены. |
| M-02 — non-text clipboard теряется | **Принятое продуктовое поведение** | Произвольные MIME не архивируются: картинка или другой невосстановимый payload может быть заменён. Это согласованная политика — операция не блокируется, а после успешной вставки в clipboard остаётся преобразованный текст. Служебный sentinel удаляется, когда владение им подтверждено. |
| M-03 — restore затирает конкурентное изменение clipboard | **Закрыто в практической границе** | Restore требует согласованного owner/value/owner-наблюдения, совпадения owner и точного значения операции. Unit-тесты покрывают смену owner/value, а Mint/X11 и Ubuntu/Wayland сохранили конкурентную запись. Атомарного clipboard CAS нет, поэтому короткое протокольное TOCTOU-микроокно остаётся документированным ограничением. |
| M-04 — конфигурация записывается неатомарно | **Закрыто** | Same-directory temp получает полную запись и `fsync`, commit выполняется atomic `rename`, затем синхронизируется каталог. Pre/post-commit ошибки, mode `0600`, symlink и cleanup покрыты тестами; exact DEB подтвердил целый TOML в обеих VM. |
| M-05 — stale D-Bus settings update теряет изменения | **Закрыто** | Типизированный field mask накладывается на последний committed config под единым gate; daemon возвращает фактический snapshot. Unit/UI/D-Bus tests и exact DEB `0.1.0-8` в Mint/X11 и Ubuntu/Wayland сохранили независимые изменения и подтвердили last-write-wins одного поля. |
| M-06 — stale PID file может получить чужой PID | **Закрыто** | Прямой dev-runtime с PID-файлами, process scan и сигналами удалён из `manage.sh`; старые lifecycle-команды fail closed. Также удалён внутренний `OPEN_SWITCHER_RUNTIME_MODE=dev`, отключавший managed watchdog/recovery. Regression test запрещает возвращение этой поверхности. Результаты: [`2026-08-11-m06-dev-runtime-retirement-validation.md`](2026-08-11-m06-dev-runtime-retirement-validation.md). |
| M-07 — keyboard poll игнорирует HUP/ERR/NVAL | **Закрыто** | Terminal flags имеют приоритет и дают typed `PhysicalKeyboardDeviceLost`, который переводит lifecycle в `Recovering`. Unit покрывает `HUP/ERR/NVAL` и mixed flags; QEMU hot-unplug трижды подтвердил runtime `POLLERR | POLLHUP` и восстановление без restart/fd growth. |
| M-08 — риск владения fd в старом `uinput 0.1.3` | **Закрыто в исходном lifecycle scope** | Dependency зафиксирована exact version и локально patched: `Builder` и `Device` закрывают fd, передача ownership явная, repeated recovery проверен. Deprecated API остаётся maintenance/lint debt, но исходная утечка fd устранена. |
| M-09 — package lifecycle скрывает stop/ACL failures | **Закрыто** | Stop имеет deadline и postcondition; upgrade/remove/purge проверяют отсутствие старого process и детерминированно очищают manifest/ACL/tag state. |
| M-10 — неограниченный word buffer/correction cost | **Закрыто** | Word tracking ограничен `MAX_CORRECTION_KEYSTROKES = 128`; после overflow физический ввод сохраняется, а destructive correction отключается до следующей реальной границы. |

Полный отчёт о закрытии M-04/M-05:
`docs/audits/2026-08-11-config-settings-transaction-validation.md`.

## Low

| Finding | Статус | Текущее основание |
|---|---|---|
| L-01 — небезопасные predictable debug paths | **Закрыто** | Общий bounded debug hub открывает sink с `O_NOFOLLOW`, проверяет regular file и effective UID, принудительно ставит mode `0600`; producer не блокирует input path. |
| L-02 — два источника udev rule | **Закрыто** | DEB устанавливает одно каноническое `70-openswitcher-input.rules`; старое `80-*` и ACL bridge удалены. |

## Уже подтверждённые общие свойства

На текущей линии исправлений имеются следующие evidence:

- текущий Rust/package gate без optional settings UI: 975 passed;
- текущий полный gate с `--features settings-ui`: 1040 passed, 1 ignored;
- package shell gates и `git diff --check`;
- exact DEB `0.1.0-5` в Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland;
- exact DEB `0.1.0-6` в целевой Mint/Cinnamon/X11 H-02/M-07 кампании;
- exact DEB `0.1.0-7` в Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland для
  M-01..M-03;
- exact DEB `0.1.0-8` в Mint/Cinnamon/X11 и Ubuntu/GNOME/Wayland для
  M-04/M-05;
- active upgrade, reinstall, remove и purge;
- session deactivation/recovery и освобождение input backend;
- F12, auto correction, исправление двух заглавных и accidental Caps Lock;
- H-06 daemon/guardian crash evidence на предыдущем exact code slice;
- H-07 fail-closed normal/transient/extra/same-count evidence;
- H-02 held-key/pre-grab и M-07 triple unplug/replug evidence;
- selected-text: восстановление прежнего текста, image/non-text fallback,
  конкурентный owner и `NoSelectedText` без оставшегося sentinel в обеих VM;
- atomic config mode `0600`, отказ старого settings signature и сохранение
  несвязанных tray/settings изменений в обеих VM;
- пользовательский smoke установленного DEB на host.

Этого достаточно для продолжения remediation без немедленного повторения всей
дорогой runtime-кампании. Это не заменяет финальную проверку после последних
поведенческих изменений.

## Приоритет продолжения

1. **M-06:** безопасная process identity для development lifecycle.
2. После последнего изменения — одна объединённая финальная кампания.

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
6. повтор ключевых clipboard-сценариев на итоговом объединённом DEB;
7. suspend/resume, если VM/desktop даёт воспроизводимую границу;
8. ограниченный host happy-path acceptance без опасной fault injection.

Одновременная гибель всех userspace-компонентов, kernel D-state, отказ всего
X server и power loss остаются пределами userspace-гарантий, а не обещанием
OpenSwitcher.

VM-лаборатория, диски и evidence сохраняются до отдельной прямой просьбы
пользователя об удалении.
