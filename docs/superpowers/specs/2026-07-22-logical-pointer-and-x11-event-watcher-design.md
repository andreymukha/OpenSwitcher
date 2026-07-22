# OpenSwitcher Logical Pointer and Wakeable X11 Watcher Design

**Date:** 2026-07-22

**Status:** Draft for approval

**Scope:** Pointer-context invalidation and the X11 active-window watcher only

## Goal

Preserve the last typed-word context across accidental touchpad contact and
pointer motion, invalidate it for a real logical click, and remove the X11
active-window watcher's 5-ms idle polling without restoring the first-key race.

These are two independently testable changes. They must be implemented and
committed separately so either can be reverted without changing the other.

## Confirmed Problems

### Touch contact is classified as a click

`is_pointer_click()` currently accepts every Linux key code from `BTN_LEFT`
through `BTN_TOOL_DOUBLETAP`. That numeric range includes genuine mouse
buttons, but also `BTN_TOUCH`, `BTN_TOOL_FINGER`, pen/tool presence, and other
touch interaction codes. The pointer watcher therefore invalidates the word
context when a readable touchpad reports contact during ordinary cursor
movement.

### The active-window watcher busy-polls its X11 queue

The active-window watcher drains `x11rb::Connection::poll_for_event()` and,
when the queue is empty, sleeps for 5 ms. The interval was reduced from 50 ms
to prevent a reproduced partial-word correction immediately after an X11
focus change. It restored correctness, but increased measured daemon idle CPU.
Choosing another interval would only trade CPU usage against the same race.

## Required Semantics

Pointer context invalidation follows the desktop's logical action as closely
as the session permits:

- pointer motion, light touch, tool/finger presence, scrolling, and gestures do
  not invalidate the typed-word context;
- a physical primary, secondary, middle, or navigation-button press does;
- a touchpad tap that the X11 input stack converts into a logical button press
  does;
- duplicate observations of one physical/logical press are harmless and
  coalesce into one invalidation flag;
- no pointer device is grabbed and no pointer event is injected.

The context may be invalidated on logical button press rather than waiting for
release. Once a press begins, focus, selection, caret position, navigation, or
a context menu may change, so the old word context is no longer trustworthy.

## Change A: Correct Pointer Invalidation

### Raw-device classifier

Replace the numeric key-code range with an explicit allow-list of genuine
mouse buttons (`BTN_LEFT`, `BTN_RIGHT`, `BTN_MIDDLE`, `BTN_SIDE`, `BTN_EXTRA`,
`BTN_FORWARD`, `BTN_BACK`, and `BTN_TASK`). Explicitly reject `BTN_TOUCH`, all
`BTN_TOOL_*` codes, joystick/gamepad buttons, and all relative/absolute motion
events.

This raw path remains the session-independent fallback and detects physical
mouse, TrackPoint, and clickpad-button presses. It intentionally does not try
to infer a tap from raw touch coordinates or timing. Reimplementing libinput's
tap thresholds, palm rejection, click method, and gesture policy would be less
stable and could disagree with the desktop settings.

### X11 logical-click observation

On X11, extend the dedicated X11 monitor to select XInput2 raw button-press
events on the root window without a grab. Accept logical button details for
primary, middle, secondary, and navigation buttons; reject wheel-button
details and valuator/motion events. Emulated button presses must not be
discarded because tap-to-click may be represented that way by the X11 input
stack.

The X11 click signal and the raw-device signal feed the same atomic
invalidation state. XInput2 setup is additive: if the extension or event
selection is unavailable, active-window tracking continues and raw physical
buttons remain the fallback.

On Wayland there is no generic project backend that can observe every
compositor-generated logical tap. This change therefore provides best-effort
physical-button invalidation through evdev and deliberately does not guess
touch gestures. Compositor-specific logical-click support is outside this
slice.

### Staging

The first implementation commit adds the corrected classifier and X11
logical-click event handling while retaining the current 5-ms X11 queue poll.
This isolates click semantics from the wait-loop change.

## Change B: Wakeable Event-Driven X11 Wait

After Change A passes, replace the sleep loop with a wakeable blocking wait on:

1. the file descriptor of the monitor's dedicated X11 connection; and
2. a private shutdown descriptor owned by `InputTargetWatcher`.

The watcher thread remains the sole user of its X11 connection. Its loop is:

1. drain all events already buffered inside x11rb with `poll_for_event()`;
2. process and coalesce active-window and logical-button events;
3. check the stop flag;
4. block until either the X11 descriptor or shutdown descriptor is readable;
5. on X11 readiness, return to the drain step; on shutdown readiness, exit.

Draining before blocking is mandatory because x11rb can already hold a parsed
event while the underlying socket is no longer readable. The shutdown path
sets the existing atomic stop flag, signals the private descriptor, and then
joins the worker. A naive uninterruptible `wait_for_event()` is not acceptable
because it could hang daemon shutdown.

The wait should use a small safe OS polling wrapper. No second thread may read
the X11 connection. X11 EOF, hangup, or protocol errors preserve the current
worker-failure policy: record a bounded diagnostic, mark the required worker
dead, and allow existing controller health handling to release the input
backend rather than silently continuing without target invalidation.

There is no periodic timer on the normal X11 path. `_NET_ACTIVE_WINDOW`
remains the same source of truth; only the waiting mechanism changes.

## Failure Handling

| Failure | Required behavior |
|---|---|
| Raw touch/motion event | Ignore it; retain word context |
| XInput2 unavailable | Continue focus watching; use physical-button fallback |
| Duplicate raw and X11 click | Coalesce through the existing atomic flag |
| X11 connection error/disconnect | Worker becomes unhealthy; existing fail-safe controller path applies |
| Shutdown while X11 is idle | Wake the wait descriptor and join promptly |
| Shutdown notification already pending | Treat it as idempotent and exit |
| Unexpected X11 event burst | Drain and coalesce without losing target-change invalidation |

## Verification

Implementation follows TDD and does not exercise host input devices.

### Safe local tests

- explicit true/false table for every accepted mouse button and representative
  touch/tool/gamepad codes;
- XInput2 classifier tests for primary/secondary/middle/navigation, emulated
  primary, wheel details, motion, and unrelated events;
- duplicate raw/X11 press coalescing;
- buffered-event-before-fd-wait regression test;
- idle shutdown wakes and joins within a bounded deadline;
- X11-readiness, shutdown-readiness, EOF, and error paths;
- active-window changes remain observable without an interval constant.

### Retained Mint/Cinnamon X11 VM

Validate the installed Debian package, not only a developer binary:

- type a word, move the pointer with touchpad contact, then correct the full
  word successfully;
- repeat with accidental touch and with scrolling;
- verify physical click and tap-to-click both invalidate the previous word;
- verify mouse/TrackPoint primary, secondary, and middle buttons;
- focus a new window and immediately type/correct, including the reproduced
  `ыгвщ` + F12 case;
- repeat rapid focus changes and first-key input;
- stop and restart the user service while X11 is idle;
- exercise X11 disconnect/error handling through the VM's independent control
  channel;
- rerun ordinary switching, automatic correction, Caps Lock correction, and
  two-capitals correction;
- compare idle CPU with the current 5-ms package.

Change B is accepted only if the functional matrix, prompt shutdown, and
failure behavior pass. If it does not, Change A remains valid and the 5-ms
polling implementation is retained; the interval is not increased or tuned by
guesswork.

## Non-goals

- no redesign of keyboard grabbing, writer queues, or runtime snapshots;
- no changes to correction timing, layout polling, key mapping, or XTest
  replay parameters;
- no raw touch gesture recognizer;
- no compositor-specific Wayland integration in this slice;
- no VM-lab rebuild or cleanup.

## Technical References

- libinput tap-to-click maps qualifying touch sequences into button clicks and
  explicitly warns callers not to recreate tap recognition from lower-level
  gesture signals: <https://wayland.freedesktop.org/libinput/doc/latest/tapping.html>
  and <https://wayland.freedesktop.org/libinput/doc/latest/gestures.html>;
- XInput2 event selection is per client and does not require a pointer grab:
  <https://www.x.org/archive/X11R7.5/doc/man/man3/XISelectEvents.3.html>;
- x11rb's event-loop integration notes require draining its internal event
  buffer before waiting on the connection file descriptor:
  <https://docs.rs/x11rb/0.13.2/x11rb/event_loop_integration/>.
