# OpenSwitcher Input Runtime Snapshot Design

**Date:** 2026-07-17  
**Status:** Approved  
**Scope:** H01, runtime/config/layout snapshot portion

## Goal

Remove synchronous external work and potentially long runtime/config/layout
lock waits from the grabbed input path. A slow, failed, or hung layout backend
must be able to disable layout-dependent transformations, but must not prevent
physical input forwarding or timely input-backend release.

## Confirmed Problem

`DaemonService` currently performs synchronous runtime work while the physical
device can be held through `EVIOCGRAB`:

- layout-shortcut handling calls `RuntimeState::sync_with_backend()`;
- correction preparation and completion call backend sync and desktop layout
  observation directly;
- startup autocorrection retry calls `periodic_sync_tick()`;
- GNOME Wayland optimistic reconciliation reads desktop settings directly;
- input handling reads configuration through the same `RwLock` that an update
  can hold while persisting the configuration file.

The legacy backend ultimately executes commands such as `xset`; desktop
observation and redetection can execute `gsettings`, `xfconf-query`, or
`setxkbmap`. These calls have no input-path deadline. The structural defect is
confirmed by static control flow. The frequency and desktop-specific impact of
real command hangs remains a runtime-validation question.

## Safety Invariants

1. After `EVIOCGRAB`, `DaemonService` does not execute external layout commands,
   synchronously query a layout backend, or wait for config persistence.
2. Physical event forwarding never waits for snapshot refresh.
3. The input path uses only a service-local snapshot, atomics, bounded
   nonblocking notifications, and nonblocking reads from a publication cell.
4. A snapshot that is not recently confirmed cannot authorize a
   layout-dependent text mutation.
5. Stale, unknown, or provisional state fails open for physical input and
   fails closed for correction.
6. Every separator already suppressed for a pending correction is replayed
   exactly once when the correction is cancelled because its snapshot is no
   longer valid.
7. A configuration snapshot becomes visible only after the corresponding
   settings update has been successfully persisted.
8. Backend failure never extends layout freshness merely because an old value
   remains cached.

## Architecture

### Input snapshot

Introduce a cheaply cloneable `InputRuntimeSnapshot` containing only data
needed for input decisions:

- the last committed `RuntimeConfigSnapshot`;
- enabled state and relevant `FeatureAvailability` fields;
- session type and other context used by input invalidation;
- effective current layout state;
- layout confirmation state;
- monotonically increasing config and layout generations;
- the monotonic instant of the last successful layout confirmation.

The confirmation state is explicit:

- `Fresh`: successfully confirmed recently and eligible for layout-dependent
  decisions;
- `AwaitingConfirmation`: a local action may have changed the layout;
- `Stale`: the last successful confirmation is older than the freshness bound;
- `Unknown`: no usable confirmation exists.

The freshness bound is `max(1 second, 3 * polling interval)`. With the current
300-ms polling interval, the effective bound is one second. Any locally
observed action capable of changing the layout immediately makes the local
snapshot provisional, regardless of age.

### Publication boundary

Runtime producers publish complete snapshots through a dedicated publication
cell. Its lock is independent of backend ownership and configuration
persistence. A producer performs external work before entering the publication
cell and holds the publication lock only long enough to replace a value.

`DaemonService` owns its last successfully loaded snapshot. It uses
`std::sync::RwLock::try_read` at safe points between input decisions.
Contention, poisoning, or publication failure cannot wait in the input loop.
The service keeps its last committed config values, while layout eligibility
naturally expires according to the stored monotonic confirmation time.

### Background refresh coordinator

One background layout-refresh coordinator owns synchronous layout observation,
context redetection, and backend refresh calls. It performs:

- the existing periodic refresh, nominally every 300 ms;
- coalesced immediate refresh requests from input handling;
- retry on ordinary transient backend or desktop-command errors.

The request channel is bounded to one pending wakeup. Producers use `try_send`;
a full channel means a refresh is already pending and is treated as success.
A disconnected channel is recorded nonblockingly but never propagated as an
input error.

The coordinator publishes a fresh layout generation only after a successful
confirmation. Errors retain the previous descriptive value but do not update
its confirmation instant. A panic, spawn failure, disconnected coordinator, or
indefinitely blocked external operation therefore causes the snapshot to age
into `Stale`; input forwarding remains available.

The existing synchronous backend functions become private to the runtime
refresh implementation, or otherwise require a capability unavailable to
`DaemonService`. The input-facing API is restricted to nonblocking snapshot
loading and refresh requests. Startup may perform the initial synchronous
load before input-backend admission and grab.

### Configuration publication

Configuration persistence remains synchronous on the settings/D-Bus side, but
it no longer shares its wait with input decisions. After validation, the
updated configuration is saved first. Only a successful save advances the
published config generation and enabled/config fields. During validation or
save, input processing continues with the previous completely committed
snapshot.

Concurrent settings updates must preserve the existing serialization and
must not publish a configuration different from the file that won the update.
The publication lock is never held across filesystem I/O.

## Input Data Flow

### Startup

1. Load and validate configuration.
2. Detect the initial runtime/layout state before input-backend admission.
3. Publish the initial input snapshot.
4. Start the background refresh coordinator.
5. Prepare the writer and watchers, then admit and grab the physical input
   backend under the existing H02 ordering.

If no initial layout confirmation is available, startup still admits a healthy
forwarding backend, but layout-dependent transformations begin disabled.

### Normal event handling

At the start of an event batch, and before a delayed/pending correction is
committed, the service nonblockingly adopts the newest published snapshot. A
fresh snapshot preserves current correction behavior. Pure configuration
decisions continue using the last committed configuration even if the layout
portion is stale.

### Stale or unknown layout

For `Stale`, `Unknown`, or `AwaitingConfirmation`:

- ordinary key events and separators are forwarded exactly once;
- automatic layout correction is not scheduled;
- a manual correction hotkey does not mutate text;
- a physical system layout shortcut is forwarded to the desktop, invalidates
  word context, and is not used to guess the resulting layout;
- a coalesced immediate refresh is requested.

Case-only fixes follow the same fail-closed rule in this slice because their
keystroke interpretation currently depends on the effective layout kind.

### Locally initiated layout changes

A layout shortcut, writer correction that reports a layout switch, selected
text switch completion, or any other local operation capable of changing the
layout immediately marks the input snapshot `AwaitingConfirmation` and requests
a refresh. D-Bus/tray layout status remains at the last confirmed value until
the refresh succeeds; provisional state is not published as confirmed.
Successful background observation returns the state to `Fresh` and publishes
the resulting status change.

### Pending correction generation

A pending word correction records the config generation and layout generation
that authorized it. Before suppressing or replaying its separator and before
executing its text mutation, the service revalidates both generations and
freshness. If validation fails, it cancels the mutation, commits the word as
uncorrected state, and replays any suppressed separator exactly once.

## Failure Handling

| Failure | Input-path behavior | Recovery |
|---|---|---|
| Backend/command returns an error | Keep forwarding; do not refresh confirmation time | Periodic or requested retry |
| Backend/command hangs | Keep forwarding; snapshot becomes stale | External validation determines later command timeout/isolation work |
| Refresh queue is full | Keep forwarding; coalesce request | Existing pending refresh |
| Refresh channel disconnects | Keep forwarding; corrections expire | Nonblocking diagnostic; restart daemon if required |
| Publication cell is contended | Use service-local copy | Retry on next safe point |
| Publication cell is poisoned | Keep last config; treat layout as unconfirmed | Nonblocking diagnostic; restart daemon if required |
| Config save is slow | Use previous committed config | Publish after successful save |
| Config save fails | Use previous committed config | Report update failure; no generation change |
| Generation changes during pending correction | Cancel mutation and conserve events | Subsequent input uses new snapshot |

This slice guarantees input fail-safety when the background operation hangs; it
does not claim that an indefinitely hung external child is reclaimed or that
layout-dependent functionality automatically recovers from a permanently hung
coordinator. Those are separate bounded-command/process-lifecycle concerns and
must remain visible in validation results.

## Testing

Implementation follows TDD and uses no real host input or clipboard devices.

Unit and fault-injection tests cover:

- a fake backend blocked behind a barrier while event forwarding completes;
- configuration persistence/write lock held while input decisions complete;
- fresh-to-stale expiry without a successful confirmation;
- backend errors preserving value but not freshness;
- full and disconnected refresh channels;
- publication contention and poisoning;
- layout/config generation changes before pending correction commit;
- separator and physical-event conservation on cancellation;
- shortcut and correction behavior in every confirmation state;
- a source/API boundary preventing synchronous refresh calls from
  `DaemonService`;
- coordinator spawn failure and panic/disconnect degradation.

After focused and full local safe suites pass, build the repository's primary
Debian package and validate it in the retained Linux Mint VM. The VM campaign
uses controlled fake desktop commands, including a deliberately hanging
`xset`, and verifies through an independent SSH/control path that the installed
package continues forwarding input and releases its backend on stop. No such
hang injection is run against the host session.

## Observability

Snapshot transitions, refresh failures, coalesced/dropped refresh requests, and
skipped layout-dependent actions use the previously implemented bounded debug
logger. Production behavior does not require debug logging, and the input path
never waits for a diagnostic sink. Logs identify generations, state, age, and
reason but do not contain typed or selected text.

## Non-Goals

- Rewriting layout backends or changing desktop-specific layout semantics.
- Guaranteeing termination of every external command inside this slice.
- Changing clipboard transaction safety.
- Expanding the VM-lab framework or deleting the retained lab.
- Refactoring short, pure capture-session synchronization unless a regression
  test shows it participates in this specific blocking path.

## Acceptance Criteria

- No synchronous backend sync, desktop observation command, or config
  persistence wait is reachable from grabbed input handling.
- A hung fake backend cannot delay physical forwarding or backend release.
- Normal fresh-snapshot behavior preserves current correction semantics.
- Stale/provisional behavior conserves every physical event and performs no
  layout-dependent text mutation.
- The complete safe test suite passes locally, with environment-only failures
  distinguished explicitly, and passes in the Mint VM.
- The installed Debian package passes the selected VM hang/failure scenarios
  with retained evidence.
