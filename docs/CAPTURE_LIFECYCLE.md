# C03 capture lifecycle

Status: implemented-unverified for real native capture. The worker owns an
explicit stop flag, completion channel and reaper-owned JoinHandle. CaptureSession
shutdown disables suppression/restores the source pointer before requesting stop;
the caller waits at most 500ms per attempt and reports errors. Drop provides bounded
fallback cleanup. A still-running thread retains the process-wide reservation,
preventing another capture from installing competing hooks during a timeout.

Windows dispatches input immediately via PeekMessage and waits for input with a
50ms maximum idle wait. macOS runs the existing event tap in bounded 50ms run-loop
slices, disables the tap and releases its source/port before completion. Native
initialization failure or readiness timeout drops the thread owner and requests
stop too. A blocked OS call cannot be force-killed safely: the join may time out,
but the gate is disabled and new capture remains refused until the native thread
is reaped. The background reaper can remain blocked; the caller cannot.

Pure lifecycle tests cover 20 start/stop/join rounds, repeated shutdown, early
initialization exit and a deliberately blocked backend's timeout/recovery.
They are not native hook/handle-count measurements. Real permission-granted OS
start/stop cycles, resource counts, permission revocation and session reconnection
remain pending under C03/#3. For the subsequent C04 implementation and remaining
native UX acceptance, see
[WORKER_IPC.md](WORKER_IPC.md).


Task completion is not treated as thread completion: a thread-local destructor
may still block after the completion notification. Neither that notification nor JoinHandle::is_finished is a join
guarantee. A dedicated reaper joins the native thread; the caller only waits on
its acknowledgement with a timeout. The reaper retains capture ownership through
all native thread-local destructors. One reaper thread is added per active capture;
its resource overhead has not been benchmarked. A regression uses a deliberately blocked thread-local destructor and an independent rescue to
prove the previous implementation failed this boundary without hanging CI.

## G03 native short-path increment (default off)

`platform::gesture_capture` supplies a Windows-only library entry on the same
existing WH_MOUSE_LL/WH_KEYBOARD_LL lifecycle; no binary calls it. KVM capture
passes `None` and keeps its current dispatch/control behavior. The G1 branch
runs before the KVM BTreeSets/tokio sender and does no recognition, OS target
query, disk/network I/O or dynamic storage in callbacks. A preallocated 256-event
SPSC ring carries opaque Begin/Move/End records to one worker. Raw coordinates
are Windows physical pixels, not recognizer logical pixels or DPI acceptance.

The worker must independently obtain native `CaptureAdmission`, evaluate app
rules/exclusions and publish one-use admission. The proof binds the native
context epoch, random worker identity and exact independent action consent
revision and the original at-most-five-second expiry; heartbeat cannot renew that
admission. It is not an action grant. A single versioned atomic latch coordinates
publication, callback presence and one-use claim: an event overlapping publish
makes publish fail even when the event has already ended. Existing pressed keys
and all five Windows mouse buttons prevent arming. Known injected/tagged events
never update physical pair ownership or enter G1.

Passed right-down stays passed through its up, including startup-held and
repeated events. Suppressed right-down owns its physical up even after stop,
focus invalidation, other input, queue full, timeout, worker loss or revocation.
Cancellation drops the entire stroke rather than recognizing a queued prefix.
The one-second worker heartbeat watchdog and Ctrl+Alt+Esc are terminal for that
capture. Candidate duration is strictly below 2500ms. Candidate edges stay
blocked and cancellation/completion requests fresh edge rearming; this is not a
live KVM/G1 edge integration claim. New physical input cancels an unretired old
stroke, including delayed action/right-click recovery context.

Stopping cancels first but keeps the original hook alive while physical right-up
is owed. A 500ms caller timeout retains the old thread/reaper reservation and
reports failure. A known message-source failure latches ownership-lost; clearing
the local debt counter then does not prove the physical pair was recovered.
Windows can silently remove a slow low-level hook: that cannot be detected
reliably by this increment and remains a native acceptance/diagnostic limitation.

The existing Reservation now also holds a nonblocking OS file lock on the empty
`temp_dir()/peerglide-native-input-capture.lock`. It is acquired outside callbacks
and explicitly unlocked only after the real thread and its TLS destructors join.
Existing nonempty, directory or symlink objects reject without truncation,
replacement or removal. Newly created Unix locks are mode 0600. This coordinates
cooperating processes sharing that configured user temp root; different temp
roots and malicious local path replacement are outside this coordination claim.
Unlock failure reaches shutdown as an error and keeps the process guard poisoned.
The empty lock file persists; no user file cleanup is performed.

macOS G1 is not exposed by this entry. CGEvent source PID/state/user-data fields
cannot prove physical origin; even HIDSystemState may originate in a user-space
driver. No PID=0/state=1 heuristic or injected fixture substitutes for physical
provenance. Existing macOS KVM tap remains unchanged. A trustworthy physical
provider within the same lifecycle and native acceptance are still required.

Not implemented here: gesture worker/recognizer wiring, application rule dispatch,
owned-injection ledger, balanced short-click replay, native overlay and local G1
consent UI. Ordinary unarmed pairs pass; an armed short click is captured but is
not replayed by this library. Therefore product G1 stays disabled. Synthetic pair,
queue, allocation and lifecycle tests are not real right-click, driver, DPI,
permission, sleep or two-machine C06 acceptance. No new protocol/ALPN or grants.
