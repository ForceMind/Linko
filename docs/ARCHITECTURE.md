# Architecture

[Cloud foundations](CLOUD_FOUNDATION.md) specify the G03 ownership model, exact
local rules and versioned GUI persistence. No native gesture hook/action is enabled.

G01's isolated semantic preflight library is documented in [ACTIONS.md](ACTIONS.md).
It is not wired into capture or desktop execution and introduces no wire protocol.

G02's isolated direction-preserving recognizer is documented in [GESTURES.md](GESTURES.md).
Its bounded collector and synthetic corpus do not enable native interception.
The GUI also has a [window-local gesture lab](GESTURE_LAB.md) for recognition and
explicit labelled exports; it cannot dispatch actions or install an input hook.

`peerglide-protocol`: messages, bounded decode, USB HID vocabulary.
`peerglide-core`: receiver-authoritative lease, pressed input tracking, motion
sequence filtering, edge gate. `peerglide-transport`: identities, QUIC/TLS, exact
enrolled certificate verification, bounded stream framing. `peerglide-platform`:
native tagged capture/injection and fail-open watchdog. `peerglide-desktop`:
executable session orchestration, enrollment and probe.

Both peers authenticate with explicitly exchanged certificates, TLS 1.3, ALPN
`peerglide/5`. No 0-RTT input or insecure verifier. A fingerprint must be checked
over an independently trusted channel; the same untrusted channel is insufficient.

The listener accepts one connection and the connector opens a reliable stream.
Both exchange versioned Hello. Either may request input control regardless of
connection direction. Receiving nodes grant only when explicitly authorized and
idle. Simultaneous acquisition requests are safely denied; deterministic
multi-peer arbitration is future work.

Keys/buttons/scroll use the ordered stream. Motion uses absolute normalized QUIC
datagrams with per-lease sequence filtering. Loss does not permanently lose
displacement; stale/early packets are ignored and repaired by the next position.
Bulk file transfers will use separate streams and scheduling; none exist yet.

The initial coordinate model covers primary displays with differing resolution:
Windows uses DPI-aware physical capture, macOS CoreGraphics display units. Mixed
DPI correctness still requires real-machine acceptance. Full topology is pending.
HID positions are forwarded; Unicode text/IME composition is not transmitted.
Ctrl/Command remain distinct; configurable shortcut remapping is pending.

Callbacks never wait for network IO. Bounded nonblocking channel overload disables
capture. A monotonic 1-second gate watchdog is checked for every event independently
of the async session. Receiver lease: 1.5 seconds. Timeout, disconnect, error,
escape, local activity or session drop release tracked remote keys/buttons.
Tagged injected events are excluded from forwarding. Acquisition waits until
physical keys/buttons are released, avoiding local-down/remote-up splitting.

OS permissions, Secure Input, UAC and privilege boundaries are respected. Failure
terminates control rather than bypassing system security.

## Desktop control panel

`peerglide-gui` is an eframe process supervising the same `peerglide` CLI worker.
It copies only a public peer certificate into its data directory and persists its
confirmed fingerprint. Changed certificate bytes require explicit reenrollment.
It records explicit input grants, side/appearance and address settings. Starting,
probing and disconnecting invoke real workers; a private local stdin stop command
cancels the session and runs recovery destructors. The GUI has a bounded visible
log history and never receives key payloads. Transfers is explicitly unavailable.

This process boundary isolates native hooks from GUI stalls. A graceful stop is
attempted before a bounded forced-stop fallback. OS key-store/ACL provisioning,
interactive pairing codes, tray and mDNS remain subsequent work.

Reliable discrete events include a validated cursor snapshot. The receiver
positions the pointer before applying a click/scroll/key, so a dropped motion
datagram cannot leave a click at the previous location. Snapshot anchors never
lower the datagram replay watermark. Return across the entry edge is allowed on
the first reverse motion; no artificial minimum travel distance is required.

The source cursor is parked inside its display while forwarding; this avoids
Windows clipping hook coordinates at the screen edge and losing outward motion.
The saved source entry position is restored on release or session drop. The
source cursor is currently visible while parked; cursor hiding remains UX work.


### Ordered normal return (protocol v3)

The source owns normal edge-return detection. It stops local suppression and
sends Release on the same reliable stream as preceding discrete inputs. Motion
datagrams only update the receiver cursor and never terminate the lease. This
prevents an edge datagram from overtaking a click/key event and revoking its lease.
Receiver timeout, emergency and physical local preemption remain independent:
safety interruptions may discard pending remote input intentionally.

ALPN/version v3 rejects older peers rather than mixing the old receiver-driven
return semantics. Both computers must update together. This is a behavioral
protocol change despite unchanged message shapes.


Protocol v4 extends normal reliable Release with a terminal cursor snapshot.
See [ADR-0003](ADR/0003-terminal-cursor-v4.md) for validation/cleanup order and
mandatory two-peer upgrade. v3 source-ordered release semantics remain intact.


Opt-in fixed-address trusted reconnect uses fresh endpoints/control state and a
non-retryable native/security failure boundary. See [ADR-0005](ADR/0005-trusted-reconnect.md).
