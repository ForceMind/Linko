# Local G1 worker actor (library, product default off)

`platform::gesture_worker::LocalGestureWorker` composes the real existing Windows
capture receiver with GestureBroker, Recognizer, CompiledRules, native
PreparedLocalAction and sealed click recovery. It runs on its creating worker
thread. No shipped GUI/CLI constructs/starts this actor yet; there is no new
managed subprocess command, IPC version, global hook, network/peer prerequisite,
remote semantic action or script. macOS source/worker remains unsupported.

## Consent and lifecycle

Construction only compiles rules and records capabilities. Even enabled saved
rules never create a native owner or grant consent. The explicit trusted-local
start method creates a fresh LocalActions owner/consent and the ONE existing
capture lifecycle. A live owner cannot be moved to another thread. Poll on that
thread well within the existing one-second capture watchdog; a future supervisor
must construct it inside its child/worker, not move it out of the GUI.

Temporary focus/foreground/backlog failures cancel the candidate, invalidate the
old frame and suspend new gestures. Fullscreen/menu/capture/unknown scope also
suspends. Native permission/consent revocation is terminal for that instance and
enters stop/cleanup, including late freeze and action/recovery outcomes. Actual
Stopped requires successful capture shutdown and context cleanup. A timeout or
ownership uncertainty retains the capture/reaper and reports Stopping/pending;
stop requests and process existence are not OS cleanup confirmation.

## One original frame

A real native admission frame supplies the exact application ID, window DPI and
original expiry before arming. The capture publisher cannot replace a frame
while a Begin/End sequence is unretired, even if the physical pair already ended
and its queued Begin has not been read. This prevents associating old input with
new app/DPI metadata. Cancellation watermarks retire even skipped events, so an
unread cancelled Begin cannot leave admission permanently blocked.

Window DPI is part of Windows native target identity and the watcher's cached
binding; a DPI transition invalidates older native leases. The worker freezes
Prepared using the ORIGINAL trigger and a lifetime no longer than the
recognizer/capture/action bounds. Late native guards use that expiry, not a new
five-second deadline calculated after recognition.

G1 scope is conservatively read outside callbacks: enabled original window,
known idle GUI routing, unswapped mouse buttons and a non-fullscreen usable
window. DWM physical frame bounds and monitor bounds are queried in a temporary
per-thread DPI context which is restored; no display setting or focus changes.
A window covering the selected monitor pauses, including some maximized windows
with an auto-hidden taskbar. Unknown geometry rejects. This is a conservative
policy, not proof of exclusive/fullscreen app semantics or all overlay routing.
The same scope/DPI guard is repeated before a gesture-triggered window action;
ordinary non-gesture LocalActions retain their separate contract.

## Existing model, real native dispatch

Worker Core borrows the existing recognizer, owns a GestureBroker and immutable
CompiledRules, and holds at most one Prepared. Fixed native events are converted
to relative logical coordinates using the ORIGINAL DPI. No recognition or rule
selection is moved into callbacks. Model clocks are checked, the entire stroke
poisons on overflow/invalid input, and cancellation resets the worker model so it
does not wait for an Up which the native broker already drained.

GestureBroker's consuming ticket selects either short-click recovery or a
recognized gesture. Rules resolve against the original authenticated app ID;
exclusions/disabled rules and unsupported or unconfirmed actions do not arm.
Eligibility iterates the recognizer's actual template IDs, not a separate
hardcoded gesture vocabulary. Exact-app/group/global priority remains the
existing rule implementation. A saved rule enabled flag never grants execution.

Typed actions consume the real Prepared via LocalActions/Coordinator with a new
OS-random request ID; the ID is dedup metadata, not authority. A short click
consumes the real sealed native End receipt. Every cancellation/error/expiry/
replacement returns Prepared for native cancellation and retires the sequence.
Config validation happens before replacement; invalid/future/conflicting rule
data retains the active snapshot and no user file is written by this actor.
Successful replacement invalidates old native configuration tickets. No raw
points, keys, app IDs, context nonces or request IDs are emitted by worker status.
Submitted still means OS/local-hook submission, never app completion.

## Verification and remaining gates

Core tests are nonphysical: actual model/recognizer/rule composition, labelled
synthetic paths, original app/DPI/time, rejection/overflow, configuration rollback
and complete move-only Prepared ownership. Windows actor gate tests never start
capture: they prove enabled preference is not consent and late permission-loss
results stop rather than suspend. Native query seams cover fullscreen/unknown
geometry and target DPI changes. Existing KVM/process/GUI smokes remain required.

This does not complete real physical/native worker acceptance or the product:
managed child process/stop IPC and explicit Chinese enable/pause/recovery UI,
tray/overlay, Mac physical source, keyboard owned injection, human corpus and C06
remain incomplete. No host/runner mouse or key injection is part of ordinary CI.
The prior silent-hook-removal and unknown cleanup limits still apply. Do not
infer global G1 usability or #10 root-cause resolution from this library or CI.

Primary scope query contracts:

- [GetWindowRect / DWM physical frame bounds](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect)
- [DwmGetWindowAttribute](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmgetwindowattribute)
- [MONITORINFO](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-monitorinfo)
