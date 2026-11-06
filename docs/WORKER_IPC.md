# C04 local worker state

Local supervisor events have schema version 1 and a finite state enum. They are
independent of network protocol (currently v5; originally v3). Only parsed stdout events may update GUI state;
stderr/human logs are diagnostics. Unknown schemas/fields are ignored. There is
no key, clipboard or file payload in these events. Each child has its own channel;
old child channels are replaced, and process exit/stopping overrides status.

GUI readers use a bounded 128-item nonblocking channel; a stalled GUI cannot
accumulate unbounded queued events or stop draining child pipes. The CLI emits
only a handful of lifecycle events, not per-input events. Diagnostic text is
bounded before queuing and the UI retains at most 100 entries.

Disconnect-and-revoke is available during a session. It requests graceful worker
stop, immediately clears the displayed connection/grant, removes pinned trust and
persists revocation. Persistence/removal errors are surfaced; a failed write is
not reported as successful. OS input cleanup still follows the session/lease
recovery paths; the GUI does not claim OS confirmation from a stop request alone.

Tests cover version/size/schema rejection, stale worker events, diagnostics that
look like old status messages, revocation persistence and real two-process
managed probe lifecycle. Native GUI permission/revocation acceptance remains
pending. C04 is not accepted as a complete real-desktop workflow yet.


## Persistent-worker extension

Lifecycle updates now use a fixed-size coalesced atomic snapshot per child;
diagnostics retain the bounded 128-entry nonblocking queue. Thus dropped logs or
a paused GUI cannot discard the newest disconnect/listen/retry state. Probe
success survives coalescing into Stopped, and process exit/stop still wins.
Reconnecting and Disconnected are finite IPC-v1 states; they never imply resumed
control. Tests cover a live helper child, a full log queue, stale authentication
after stop, probe retention and old-config defaults. See ADR-0005.

## Experimental read-only pairing child

Hidden pair-offer/pair-connect commands require --managed and non-terminal stdin/
stdout. They load an existing positively matched identity read-only. No input,
enrollment, profile creation, receiving grant or settings write is reachable.
Code entry travels only through bounded private stdin, not argv or diagnostics.
The displaying child sends the code once over its own private stdout for the
supervisor to display; it must never enter the ordinary log reader/history.

Separate IPC-v2 pairing_event/pairing_command envelopes reject unknown fields,
versions and oversized records. Commands are capped at256 bytes before allocation;
public certificate events at72KiB before parsing, DER material at16KiB. Parsed
events are observations, not persistent-trust tokens. Supervisor must still bind
the exact owned child/channel, GUI generation and original identity/config to a
later live Session/commit exchange; it may not fabricate manual confirmation.

Input uses a one-item data queue and separate terminal watch: Stop/EOF/invalid
commands preempt pending data. An atomic expected phase admits each Code/Approve/
LocalSaved/ReceiptStored once, with allowed state published before its prompt.
Sensitive record types have no Debug and clear accepted code buffers
on drop; this is best-effort memory hygiene, not a secure-memory/allocator claim.

Output uses one bounded writer slot plus a two-second acknowledged write deadline.
Blocking pipe IO lives on std threads so it cannot pin the async lifecycle or the
Tokio blocking pool. The whole child retains one120-second deadline. Async panic
and computation-task failure become terminal failure, not authentication retry.
After temporary authentication, peer close/unexpected data ends the Session; it
is invalidated before further local use. Expected EOF/reset is a disconnect;
unexpected application data or malformed TLS is Failed/Protocol, not exit0.
The dedicated pairing command replaces the default panic hook with typed
Failed/Internal reporting and a failing exit, avoiding raw panic arguments or a
blocked stderr hook; it does not suppress the failure outcome. Other CLI modes
and the GUI panic policy are unchanged. The c6 read-only worker initially supported
Code/Stop only. The v2 prototype now exchanges Approve/LocalSaved/ReceiptStored
claims over the private pipe; these are parent reports, not proof of disk writes.
The worker itself still never writes enrollment/config or enables input.
Stop, EOF, expiry and process exit never mean a device was saved. Native C06 and
GUI pairing/enrollment remain separate, unfinished acceptance.

Runtime c6aaf24 passed five platform/MSRV/audit jobs42236705671 and upstream
42236705654. Real private-pipe smoke ran on three platforms and the host Mac,
exit0/no forced cleanup/no owned children, no native input/enrollment/file changes.
The earlier Mac EOF failure is preserved: code-await could return InvalidCommand
between select polls. Stop is now a separate cancellation outcome, with a final
control snapshot check; Protocol/Internal errors remain failures. See VALIDATION.

The v2 commit exchange uses the same authenticated pairing/2 TLS connection.
Both user approvals/Prepared facts precede ReadyToSave; only LocalSaved for that
context emits network Saved. PeerSaved evidence survives normal remote EOF while
the parent stores its confirmation; Completed requires ReceiptStored. Missing
notifications never fabricate Saved/Completed. The new exchange smoke uses real
pipes/TLS with explicitly synthetic parent save replies, not filesystem or GUI
enrollment acceptance. Old IPC-v1 and pairing-v1 require matched new binaries;
input v5/manual enrollment are unchanged.
