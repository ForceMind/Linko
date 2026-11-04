# Windows sealed short-click recovery (G03 increment)

This is a library path from a real capture End receipt to native input submission.
No product binary enables/calls it. It does not complete the recognition/rules
worker, native G1 enable UI, keyboard owned injection, macOS provenance or C06.
Existing Chinese UI and protocol v5/MIT/data grants remain unchanged.

## Receipt and target

Only the existing capture broker creates a non-Clone `RecoveryReceipt`, after an
entire uncancelled native right pair. Public Event coordinates cannot construct
or modify that receipt. Its private original frame contains the random worker/
consent identities, native epoch, original trigger start, capture instance,
sequence, endpoint, window-DPI sample and maximum displacement across the WHOLE
trajectory. The callback only accumulates a fixed geometric envelope, not a
recognition result. Recognition/geometry thresholds are evaluated outside it.

`LocalActions::recover_click` consumes both this receipt and the real original
PreparedLocalAction. Their frame/start must match; the same native owner consumes
and rechecks its real target lease. The click must last at most 400ms and stay
strictly within 12 window-logical pixels at the frozen window DPI (48..768 is a
project bound). Moving far and back does not become a short click. Dispatch must
start before both the original context expiry and release+250ms; queueing and
heartbeat do not renew them. Timestamps bind the original trigger; they are not
authentication. No fixture IDs/PIDs/static tags grant production authority.

Preflight confirms the original foreground/focus/process instance and desktop,
enabled window, unchanged window DPI, physical cursor exactly at the sealed
endpoint, physical hit/root with matching owner, and idle foreground GUI routing
(no known capture/menu/move-size). Swapped mouse buttons reject until their exact
semantics are implemented/accepted. GetPhysicalCursorPos/WindowFromPhysicalPoint
avoid mixing a caller's virtualized logical coordinate space with hook pixels.
Hit testing skips hidden/disabled windows; this does not enumerate or exclude all
overlays or all other routing conditions. Window DPI is not monitor/hardware DPI.
The final native guard checks the original consent/frame/deadline again after
queries, immediately before submission. Queries and submission are not one OS
atomic operation; native acceptance remains required.

## Bounded event ownership

One in-flight recovery per capture uses fixed atomics. Idle/Initializing/Active/
Finishing prevents the capture loop consuming a stale terminal slot while the
worker initializes it, or a new worker overwriting a slot still being cleared.
Cancel during initialization is preserved separately. Tags and completion are
published before the slot returns to Idle. Tests exercise reuse interleavings.

Two OS-random per-transaction tags route separate Down/Up INPUT records in ONE
SendInput call. No cursor move, arbitrary key, script or remote RPC is involved.
Tags are routing clues; sealed native context plus independent G1 consent is the
authority. The existing hook routes these tags before generic injected filtering.
Only an eligible owned Down at the original event physical point is allowed;
physical input that arrived after preflight can still block it. The corresponding
Up is allowed only after that Down was locally allowed, even if focus/consent/
deadline later changed: it is cleanup, not a newly authorized action. Duplicate,
wrong-kind or inconsistent receipts never authorize another Down.

A physical right Down passed before the owned Down makes the owned pair reject.
If owned Down was already allowed, an overlapping physical pair is suppressed
and its physical Up remains owed, including through cancellation. Motion is
frozen while an owned Down is outstanding to avoid a partial-failure drag.
Foreign injected input does not feed recognition or the physical ledger. It
cancels a future owned Down; foreign Motion also follows the freeze decision.
Foreign right-button interference after owned Down makes ownership uncertain,
marks ownership uncertain and never guesses a cleanup Up into that other holder.
Already queued owned Up is blocked after this conflict; the hook remains installed
until registered input receipts are accounted for (and owed physical Up drained),
then exits with an error. Unresolved ownership is retained, not declared restored.

## Partial insertion and cleanup

SendInput returns a count, not a documented identity/prefix of a partial subset.
Each hook receipt records Observed and Allowed/Blocked separately. With count=1,
observing this transaction's unique Down identifies that inserted event; only an
AllowedDown proves a cleanup debt and that the original Up was not inserted.
Observing only Up blocks it and never retries Down. A BlockedDown creates no
cleanup debt. Count=2 with missing receipts, count=1 without an identifying
receipt, contradictions and source loss remain pending/Unknown; they do not
replay the click or assume an immediately reread key state proves delivery.

Only a proven-not-inserted Up may be submitted separately. Zero inserted cleanup
may retry at most three times; an accepted cleanup without a receipt is NOT
retried. Even after successful partial cleanup the result stays UnknownOutcome.
A complete pair inserted and allowed by this hook is Submitted, not app execution
or an exactly-once app guarantee. Later hooks may still block it.

The worker or existing capture loop services cleanup outside native callbacks;
the loop releases the STATE RefCell borrow BEFORE SendInput. Stop cancels new
Down and retains the lifecycle/reservation while acknowledgements or owned debt
are unresolved. Caller waits remain bounded and report failure. Explicit source
loss stops guessing new Up, preserves uncertainty and reports ownership lost;
clearing bookkeeping never proves an OS state was restored.

Windows may silently remove a slow low-level hook. Thread/heartbeat existence
cannot prove continuous physical observation; this adapter cannot promise that
separate cleanup never races an unobserved new physical hold. There is no bypass
of that OS limitation, UAC, desktops or security tools. Product G1 stays disabled,
and real continuity/latency/driver/partial-failure acceptance is NOT RUN. Keyboard
cleanup cannot inherit this right-button policy; keyboard actions stay unsupported.
macOS CGEventPost is void and private source state is not physical provenance or
global key-up isolation, so Mac recovery remains unsupported.

## Evidence

Synthetic tests cover slot reuse/cancel interleavings, point/full-envelope/time
checks, physical-before/after injected Down, foreign movement/interference,
partial subset receipts, bounded cleanup, missing/late/contradictory receipts,
source loss, stop and zero allocation after construction. Windows tests construct
INPUTs and inject query seams only. No host/runner input is synthesized by tests;
real native effects and physical C06 are separate gates. Exact candidate results
are recorded in VALIDATION.md and the PR.

Primary contracts:

- [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
- [LowLevelMouseProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc)
- [GetPhysicalCursorPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getphysicalcursorpos)
- [WindowFromPhysicalPoint](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-windowfromphysicalpoint)
- [GetDpiForWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdpiforwindow)
- [GUITHREADINFO](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-guithreadinfo)
- [CGEventPost](https://developer.apple.com/documentation/coregraphics/cgevent/post(tap:)?language=objc)
