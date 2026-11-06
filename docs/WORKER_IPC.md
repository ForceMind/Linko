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

## Experimental pairing child (in qualification)

Hidden pair-offer/pair-connect commands require --managed and non-terminal stdin/
stdout. They load an existing positively matched identity read-only. No input,
enrollment, profile creation, receiving grant or settings write is reachable.
Code entry travels only through bounded private stdin, not argv or diagnostics.
The displaying child sends the code once over its own private stdout for the
supervisor to display; it must never enter the ordinary log reader/history.

Separate IPC-v1 pairing_event/pairing_command envelopes reject unknown fields,
versions and oversized records. Commands are capped at256 bytes before allocation;
public certificate events at72KiB before parsing, DER material at16KiB. Parsed
events are observations, not persistent-trust tokens. Supervisor must still bind
the exact owned child/channel, GUI generation and original identity/config to a
later live Session/commit exchange; it may not fabricate manual confirmation.

Input coalesces into one watch slot: Stop/EOF/invalid commands preempt pending
code, repeated code is invalid, consumed code is removed without overwriting a
racing Stop. Sensitive record types have no Debug and clear accepted code buffers
on drop; this is best-effort memory hygiene, not a secure-memory/allocator claim.

Output uses one bounded writer slot plus a two-second acknowledged write deadline.
Blocking pipe IO lives on std threads so it cannot pin the async lifecycle or the
Tokio blocking pool. The whole child retains one120-second deadline. Async panic
and computation-task failure become terminal failure, not authentication retry.
After temporary authentication, peer close/unexpected data ends the Session; it
is invalidated before further local use. There is no commit command in this batch.
Stop, EOF, expiry and process exit never mean a device was saved. Native C06 and
GUI pairing/enrollment remain separate, unfinished acceptance.
