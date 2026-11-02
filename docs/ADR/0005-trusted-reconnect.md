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
