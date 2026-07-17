# Nonblocking Debug Logging Design

**Date:** 2026-07-17

**Status:** approved by the user

**Scope:** H01, logging portion only

## Context

OpenSwitcher exclusively grabs a physical keyboard with `EVIOCGRAB`. Its current debug helpers write synchronously to `stderr` and files from the calling thread. When debug is enabled, a full pipe, a stalled filesystem, a FIFO, or a blocking configured path can stop the input loop while the grab remains active.

The installed Debian package does not enable these logs by default. Development launches through `manage.sh` enable input, layout, and capture diagnostics by default, and the VM audit relies on them. Selected-text diagnostics are separately opt-in. The fix therefore must preserve useful runtime diagnostics without adding a normal-mode background service or allowing diagnostics to affect keyboard forwarding.

## Decision

Use one lazy, process-wide, bounded debug-log hub.

- It is initialized as the first operation of `daemon::run`, before any input device is opened or grabbed.
- Environment flags and output paths are read once during initialization.
- If every debug category is disabled, no channel and no worker thread are created.
- If at least one category is enabled, producers submit records with `try_send` to one bounded queue.
- Only the worker may write to `stderr` or files.
- Logging failures, a full queue, or a dead worker never propagate into daemon control flow.

The queue capacity is 256 records. Each encoded record is limited to 4096 bytes at a valid UTF-8 boundary. When the queue is full, the newest record is dropped and an atomic counter is incremented. Dropped payloads are never copied into another diagnostic path.

## Alternatives considered

### Disable diagnostics whenever a grab is active

This is the smallest change, but it removes precisely the evidence needed to diagnose writer timing, layout synchronization, capture ownership, and release failures. It also makes development and VM failure reproduction substantially weaker.

### One worker per existing logger

This isolates sinks but creates four queues, four lifecycles, and more shutdown states. A single debug workload does not justify that complexity.

### Chosen: one lazy bounded worker

One worker provides a single safety boundary and a globally ordered queue without imposing a thread in the default Debian-package configuration.

## Components

### `daemon::debug_log`

The new module owns:

- `DebugLogKind`: `Input`, `Layout`, `Capture`, and `SelectedText`;
- immutable startup configuration for enabled categories and paths;
- a cloneable producer containing only enabled bits, a bounded sender, and atomic drop counters;
- `DebugLogRuntime`, which owns the optional worker handle and stop flag;
- worker-only sink creation and writes.

Existing helpers (`log_input_debug`, `log_layout_debug`, capture logging, and `log_selected_text_debug`) keep their semantic prefixes but delegate to the producer. They no longer inspect environment variables, call `eprintln!`, open files, or write files.

The selected-text path continues to use `summarize_text`; raw selected or clipboard text is never put into a record.

### Initialization and lifetime

`daemon::run` creates `DebugLogRuntime` before configuration loading, D-Bus startup, writer creation, or input-backend preparation. The runtime lives through `finalize_daemon_run`, so normal input shutdown and grab release happen before logger teardown.

At logger teardown:

1. set an atomic stop flag;
2. make a best-effort nonblocking wake/shutdown enqueue;
3. join only when `JoinHandle::is_finished()` is already true;
4. otherwise detach the worker.

An unresponsive sink may lose remaining diagnostics, but it cannot delay `ungrab`, object destruction, or process exit.

## Data flow

1. A caller selects a debug category, stage, and already-redacted details.
2. The producer checks the cached enabled bit.
3. Disabled categories return immediately.
4. Enabled records are bounded and submitted with `try_send`.
5. `Full` and `Closed` increment counters and return immediately.
6. The worker receives records and writes the existing prefix to `stderr` and the configured category file.

The queue preserves enqueue order. Concurrent callers have scheduling-defined order, as they do today. Delivery is explicitly best effort.

## Sink security

Debug files can contain device names, key identifiers, layout state, timing, window metadata, and error details. The worker therefore opens files with these rules:

- `O_CLOEXEC` and `O_NOFOLLOW`;
- creation mode `0600`;
- the opened target must be a regular file owned by the effective user;
- permissions are forced to `0600` on the opened file;
- an invalid or unsafe target disables that file sink without a fallback path.

Configured paths and existing default filenames remain compatible. The worker may still emit to `stderr`; a blocked `stderr` can stall only the debug worker.

## Non-debug error output

Several direct `eprintln!` calls are reachable while the keyboard is grabbed. The implementation must audit them individually:

- diagnostics that are safe to lose move to the nonblocking hub;
- user-visible output that must remain synchronous is emitted only after the keyboard FD has been ungrabbed or dropped;
- the input-loop panic path records the reason nonblockingly, releases the backend through `finalize_daemon_run`, and never prints synchronously before release;
- release-error reporting cannot delay closing the grabbed device.

This design does not replace ordinary tray/settings UI messages or package-manager diagnostics.

## Failure semantics

- Disabled logging has no worker, queue, file access, or repeated environment lookup.
- Queue saturation drops newest records.
- A worker panic or sink error disables further delivery but does not fail the daemon.
- A blocked sink may retain one detached worker until process exit.
- Shutdown never waits for an unfinished logger.
- No logging path may acquire runtime, config, capture, writer, or input-backend locks.

## Tests

Implementation follows RED-GREEN TDD with local producers and fake sinks rather than global environment state.

Required tests:

1. A capacity-one queue preserves the first record, drops the second, and increments the correct counter without waiting.
2. A closed receiver returns immediately and never invokes synchronous fallback I/O.
3. A disabled category creates no record and no worker.
4. The worker routes all four categories to the correct prefixes and sinks in enqueue order.
5. A deliberately blocked fake sink cannot block a producer; the bounded queue saturates instead.
6. Dropping the runtime does not join an unfinished worker.
7. Records longer than 4096 bytes are truncated on a UTF-8 boundary.
8. Selected-text records retain the existing content-redaction guarantees.
9. Secure file tests reject symlinks and non-regular targets and verify mode `0600`.
10. A shutdown-order test proves input release precedes any synchronous postmortem output or logger teardown.

Static verification must confirm that post-grab producer paths contain no `OpenOptions`, `writeln!`, direct debug `eprintln!`, blocking channel send, or worker join.

## Success criteria

- With all debug variables unset, the installed daemon creates no debug worker and writes no debug files.
- With diagnostics enabled, a blocked sink cannot delay input forwarding or keyboard release.
- Queue memory is bounded and overload behavior is deterministic.
- Existing diagnostic categories and selected-text redaction remain available.
- No logger error changes daemon, writer, capture, or layout state.

## Non-goals

- A general application logging framework.
- Guaranteed delivery or durable log flushing.
- Remote logging, rotation, compression, or UI log controls.
- Solving the separate runtime/config/backend blocking calls in H01; those follow in the input-snapshot phase.
