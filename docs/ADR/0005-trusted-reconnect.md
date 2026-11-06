# ADR-0005: explicit trusted reconnect and fresh control state

Date: 2026-11-02. Status: implemented; native/network-change acceptance separate.

Reconnect is opt-in (--reconnect or the GUI setting); old configurations default
to false. It repeatedly creates a new endpoint/session for the same configured
address and exact enrolled identity. Identity and peer certificate bytes are
cached at startup and checked on disk before every attempt. Replacement/deletion
stops retry; it cannot silently enroll a new identity. GUI revocation also stops
the worker; checking files between attempts is not an active-session file watcher.

Only known transient network/time-out errors or a clean peer disconnect trigger
retry. Authentication/protocol/permission/native failures stop. Normal session
exit explicitly propagates pressed-input cleanup and capture-thread shutdown
failure before retry classification. Cancellation still has Drop recovery as a
fallback and does not enter the retry loop. No peer permission is inferred from
capabilities or a previous successful connection.

Backoff is 1/2/4/8 seconds and stays capped at 8 for this worker lifetime. Attempts
are sequential; opt-in retry persists until stopped or a non-retryable failure.
These are wait intervals, not end-to-end reconnect latency guarantees. Managed
stop/EOF and Ctrl+C cancel connect, listen and backoff. Probe always remains
one-shot. This slice targets a fixed peer address; discovery, address-change
tracking, interactive pairing and suspend/native acceptance are not completed.

Each successful connection negotiates capabilities again, starts fresh receiver,
lease counters, capture gate and held-state sampling. No old control is restored.
Switch-key autorepeat already held at startup is not a fresh switching intention,
including Windows startup VK samples; real physical input is required to acquire
control again. Stuck cleanup retains the existing capture reservation and stops
retry rather than installing another hook stack.

GUI lifecycle state uses a fixed atomic latest snapshot, separately from the
bounded diagnostic queue. GUI pauses cannot leave an old Authenticated event
behind a dropped Disconnected/Listening state. Probe success is retained as a
sticky result, while child exit/stopping overrides connectivity. Every child has
its own snapshot, preventing old readers from updating a new child.

Protocol remains v5. Local IPC v1 adds optional retry/disconnected states; packages
contain matching GUI/worker versions. No new remote RPC, clipboard/file permission
or arbitrary command is introduced. Reconnect smoke uses real loopback processes
and valid independent certificates, but no OS input. Physical Wi-Fi/USB/Bluetooth,
sleep, DPI/IME and native permission/cleanup verification remain #3.

## Bounded first-connection wait (0.3.9-dev)

A matched GUI may explicitly request private `--wait-for-peer-secs` (1..120,
managed connector only, mutually exclusive with reconnect and forbidden for
listen/probe). Ordinary CLI defaults and probe remain one-shot. The GUI requests
60 seconds for a connector without the user's separate reconnect preference.
This is one initial negotiation budget, never refreshed by attempts/backoff.
Only known transient connection errors retry with the existing bounded backoff.

The timer owns only pre-capture TLS/Hello negotiation and backoff. Actual pinned
TLS/Hello plus final identity-file verification retire it before session/capture
installation. No timeout wrapper surrounds an active native session or its
cleanup tail. A successful first session does not enable later reconnect;
permission/native/protocol/auth failures still stop. Stop/EOF wins over the
deadline, and cleanup time is additional to the negotiation budget.

Verify the actual local certificate/key match and exact enrolled peer bytes
before every attempt and again after negotiation. Missing/invalid/key-mismatched
identity files are non-retryable even if a filesystem error resembles a network
timeout. This is not an active filesystem watcher or a new enrollment mechanism.

Local IPC1 adds WaitingForPeer/PeerWaitExpired; neither means authentication or
an input grant. The GUI names the other computer, offers Stop and distinguishes
expiry while preserving recovery priority. Readiness/explicit native consent,
receiver permission, fresh physical switching intention, v5 and ALPN remain.
Real startup order, Wi-Fi/suspend/permission and native cleanup are still C06.
