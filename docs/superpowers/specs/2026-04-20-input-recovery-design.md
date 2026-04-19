# OpenSwitcher Input Recovery Design

**Date:** 2026-04-20

**Goal**

Сделать Linux input access для OpenSwitcher устойчивым к раннему старту user-service, пересозданию `/dev/input/event*` устройств и временной потере desktop ACL, чтобы обычный пользовательский сценарий был ближе к "поставил и забыл" и не требовал ручных `bootstrap`/`systemctl restart`.

---

## 1. Problem Statement

Сейчас `open-switcher-daemon` зависит от успешного открытия клавиатурного устройства на старте. Если в момент старта:

- ещё не подхватились `uaccess` ACL,
- input device пересоздался,
- `grab` уже жившего устройства оборвался с `No such device`,

daemon выходит с ошибкой и systemd user-service уходит в `failed`.

Текущая recovery-модель слишком хрупкая:

- `dist/udev/80-openswitcher-input.rules` уже использует `TAG+="uaccess"`, то есть базовая системная модель правильная;
- `./manage.sh bootstrap linux-input` поверх этого делает same-session ACL bridge через `setfacl`, что удобно как emergency recovery, но не должно быть обязательной частью обычного install UX;
- `systemd install` устанавливает user units и бинарники, но не превращает runtime в self-healing system.

Для будущих `.deb` это особенно важно: пакет может поставить правила и units, но не должен требовать от пользователя знания про `doctor`/`bootstrap`.

---

## 2. Design Goals

- daemon не должен умирать насовсем на `KeyboardAccessDenied`, `KeyboardNotFound`, `No such device` и близких recoverable input errors;
- input backend должен уметь сам восстановиться без ручного `systemctl restart`;
- решение не должно зависеть от имени пользователя или ручного добавления в группу `input`;
- существующая `udev + uaccess` модель должна остаться основной;
- `bootstrap linux-input` должен остаться лишь recovery/dev helper;
- дизайн должен естественно переноситься в будущую `.deb`-поставку.

---

## 3. Non-Goals

- не писать новый input backend;
- не менять correction semantics, layout switching logic или selected-text semantics;
- не превращать daemon в system service;
- не делать группу `input` основным install path;
- не полагаться на user-specific `setfacl` как на продуктовую основу;
- не решать весь packaging слой в этой задаче.

---

## 4. Recommended Approach

Рекомендуемый путь — сделать runtime self-healing поверх уже существующей `udev + uaccess` модели.

Это состоит из трёх частей:

1. `udev` rule остаётся базовым install-time механизмом доступа.
2. `open-switcher-daemon` перестаёт считать input initialization необратимой точкой отказа.
3. install/runtime tooling перестаёт позиционировать `bootstrap linux-input` как обязательный normal-path шаг.

Почему этот путь лучший:

- одинаково подходит для dev install и для будущих `.deb`;
- не зависит от конкретного пользователя;
- переживает позднее появление ACL и пересоздание `event*` nodes;
- не требует от пользователя знать внутренности Linux input permissions.

---

## 5. Runtime Architecture

### 5.1 Core idea

Нужно отделить "жизнь daemon process" от "готовности input backend".

Сейчас `DaemonService::new()` требует успешного `KeyboardController::open()`, а значит любое recoverable input failure валит весь процесс. Вместо этого daemon должен уметь жить в одном из состояний:

- `Ready`
- `WaitingForInputAccess`
- `Recovering`

### 5.2 Ownership of the state machine

State machine не должна размазываться по `DaemonService`.

Владельцем состояний, переходов и retry должен быть один отдельный lifecycle-компонент input backend. На первом этапе он может жить внутри runtime-слоя, но логически это должен быть отдельный кусок ответственности с одним источником истины для:

- текущего состояния backend;
- последней recoverable причины деградации;
- retry/backoff;
- критериев переходов;
- атомарной замены старого backend новым.

`DaemonService` должен оставаться потребителем "готового или неготового input backend", а не местом, где размазаны transition rules.

### 5.3 State semantics

**Ready**

- keyboard controller открыт;
- virtual writer готов;
- все обязательные watchers подняты;
- можно реально получать и обрабатывать input events;
- event loop работает как сейчас.

`Ready` нельзя выставлять по условию "одно устройство однажды открылось". Состояние готовности наступает только после полной успешной инициализации всего input backend.

**WaitingForInputAccess**

- keyboard device ещё нельзя открыть, нет ACL или устройство временно отсутствует;
- daemon остаётся жив, логирует причину и по таймеру пробует re-open/re-init.

**Recovering**

- backend раньше был `Ready`, но в работе поймал recoverable I/O failure (`No such device`, повторный open denied, исчезнувший device и т.п.);
- старые input handles освобождаются;
- runtime уходит в controlled retry loop, не завершая весь процесс.

### 5.4 Transition rules

- start:
  - `open ok` -> `Ready`
  - `recoverable input error` -> `WaitingForInputAccess`
  - truly fatal configuration/programming error -> process exit
- runtime failure while `Ready`:
  - `recoverable input error` -> `Recovering`
- retry:
  - `reopen ok` -> `Ready`
  - `reopen still fails` -> stay in `WaitingForInputAccess` / `Recovering`

При переходах:

- `Ready -> Recovering`
- `Ready -> WaitingForInputAccess`

обязательно сбрасывается весь transient input state, чтобы не переносить stale correction context через восстановление backend.

Минимальный обязательный reset:

- active `buffer`
- `word_context`
- `pending_word_commit`
- `suppressed_separator_key`
- input-related latches / pending input-specific transient flags

### 5.5 Retry policy

Retry не должен жить в hot path и не должен busy-loop'ить.

Retry выполняется только в фоновом lifecycle path, а не в `handle_key_event` и не в любой latency-sensitive обработке input event.

Рекомендуемая политика:

- первый retry быстро, чтобы закрыть startup race;
- затем умеренный interval с upper bound;
- простая bounded backoff-модель достаточна, без сложной оркестрации.

Подходящего поведения достаточно такого:

- 0.5s
- 1s
- 2s
- затем фиксированные 2-5s между retry

Это даёт быструю реакцию после логина и не создаёт лишнюю нагрузку в деградированном состоянии.

---

## 6. Error Classification

Нужно явно отделить recoverable input failures от truly fatal failures.

### Recoverable

- `KeyboardAccessDenied`
- `KeyboardNotFound`
- `UinputAccessDenied`
- `Io` с текстом/кодом, соответствующим пропавшему device (`No such device`, `ENODEV`) в runtime path
- ошибки повторного grab/open после recreate input device

`UinputAccessDenied` трактуется как recoverable, но без предположения, что recovery обязательно быстро станет успешным. Retry loop должен уметь жить долго, если system setup реально ещё не готов, и не превращаться в aggressive infinite spin.

### Non-recoverable

- поломка конфигурации, несовместимая с продолжением процесса;
- internal invariants / logic errors;
- ошибки, не связанные с внешней доступностью input backend.

Эта граница важна: recovery loop не должен скрывать реальные баги кода и не должен "глотать всё подряд". Ошибки availability/input-device слоя recoverable; логические ошибки программы и сломанные инварианты остаются non-recoverable и по-прежнему валят процесс.

---

## 7. Input Layer Changes

### 7.1 KeyboardController lifecycle

`KeyboardController` сегодня выглядит как одноразовый объект. Для recovery нужно, чтобы runtime мог:

- попытаться создать новый controller;
- старый controller shutdown'ить и выбрасывать;
- подменять backend atomically на новый успешный экземпляр.

Это не требует нового backend-а, но требует выделить "input backend lifecycle" как отдельную responsibility.

### 7.2 Event loop integration

Главный daemon loop должен больше не зависеть от предположения "controller всегда есть и всегда жив".

Нужна примерно такая семантика:

- если input backend `Ready`, обрабатываем реальные input events;
- если backend не готов, loop живёт, публикует деградированное состояние и проверяет, не пора ли retry;
- при успешном reopen normal event processing продолжается.

### 7.3 Word/context safety

При переходе в `Recovering` или `WaitingForInputAccess` нужно сбрасывать input-derived transient state:

- active word buffer
- word context
- pending separator state
- input-related transient latches

Это prevents stale correction state after backend loss.

Reset должен происходить централизованно на lifecycle transition, а не выборочно в случайных местах event-handling логики.

---

## 8. Status and User-Visible Semantics

Текущий daemon уже публикует статус. Для recoverable input degradation нужно отражать это честно.

Минимально правильная семантика:

- daemon process жив;
- automation/input capture временно unavailable;
- после восстановления input backend daemon снова normal-ready.

На этом этапе не нужен новый большой UI, но runtime/status path должен хотя бы:

- логировать вход в degraded mode;
- логировать successful recovery;
- не притворяться, что всё работает, если input backend недоступен.

Tray и settings должны продолжать жить, даже если automation временно unavailable.

---

## 9. Manage Script and Install Semantics

### 9.1 `bootstrap linux-input`

Новый продуктовый смысл:

- recovery helper для dev/debug;
- полезен для текущей сессии, если `uaccess` ещё не подхватился;
- не считается обязательной частью normal install UX.

### 9.2 `systemd install`

На этом этапе можно мягко улучшить semantics без превращения его в полноценный package manager:

- не делать вид, что `bootstrap` обязателен всегда;
- при желании позже добавить warning/preflight, если `doctor` красный;
- но не смешивать install-time tooling с runtime self-heal логикой.

### 9.3 Future `.deb`

Эта архитектура прямо переносится в пакетирование:

- `.deb` ставит `udev` rule и assets;
- `postinst` делает reload/trigger `udev`;
- daemon как user-service всё равно умеет пережить race между установкой, login-session и появлением ACL.

То есть `.deb` становится delivery channel для уже правильной модели, а не способом скрыть хрупкий runtime.

---

## 10. Systemd Positioning

Сами user units не должны быть единственным механизмом recovery. Но есть смысл сделать их менее race-prone:

- старт ближе к графической user session;
- без превращения в сложную зависимость от desktop-specific targets.

Это optional hardening, а не основное решение. Основное решение — self-healing daemon.

---

## 11. Logging Requirements

Для отладки таких проблем обязательны явные transition logs:

- вход в `WaitingForInputAccess`
- вход в `Recovering`
- успешный переход в `Ready`
- `retry scheduled`
- причина (`error`) для каждого деградирующего перехода

Логи должны строиться вокруг lifecycle-компонента, владеющего state machine, а не размазываться бессистемно по всему daemon.

---

## 12. Testing Strategy

### 12.1 Automated tests

Нужно покрыть:

- recoverable startup failure -> daemon остаётся жив и enters waiting mode;
- late availability -> retry succeeds -> state becomes ready;
- runtime backend loss -> daemon transitions to recovering;
- successful reopen after loss;
- non-recoverable failures по-прежнему bubble up и не маскируются.

Где возможно, это лучше делать как unit/integration tests на lifecycle/state-machine уровне, а не через реальные `/dev/input` в тестах.

### 12.2 Practical verification

На живой Linux desktop нужно проверить минимум:

1. daemon стартует в плохом input environment без `failed`;
2. после `bootstrap linux-input` или после появления нормального ACL сам оживает без restart;
3. после recreate/disconnect/reconnect keyboard сам восстанавливается;
4. tray/settings остаются живы;
5. existing correction scenarios still work after recovery.

---

## 13. Rollout Plan

### Phase 1

Runtime self-heal:

- ввести input backend state machine;
- реализовать retry/reopen;
- научить daemon жить без ready input backend;
- логировать degrade/recover transitions.

### Phase 2

Install/tooling cleanup:

- уточнить semantics `manage.sh`;
- уменьшить роль `bootstrap` как "обязательного" шага;
- при необходимости добавить мягкий preflight/warning.

### Phase 3

Packaging:

- вынести `udev` install/reload в `.deb`;
- оформить package install path как основной product path.

---

## 14. Decision

Делаем сейчас именно runtime self-healing и считаем это правильной базой под будущие `.deb`.

Мы **не** вкладываемся дальше в user-specific ACL workaround как в основную модель и **не** считаем ручной `bootstrap linux-input` нормальным пользовательским сценарием. Он остаётся только fallback-инструментом восстановления.
