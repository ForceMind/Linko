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
