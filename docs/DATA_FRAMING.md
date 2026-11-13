# Isolated data framing v1

The 12-byte PGD1 header carries a closed kind, reserved bytes and body length.
The codec checks these fields and the per-kind length before reading a body.
The QUIC adapter then checks the negotiated capability, current phase, local
direction permission and connection byte budget before allocating the body.
Malformed or interrupted frames close the connection.

| Payload | Purpose |
| --- | --- |
| Text | A bounded UTF-8 snapshot with a nonzero sequence |
| TextReceived | An admitted sequence waiting for a local clipboard decision |
| TextResult | The receiver's local import result for that exact sequence |
| Offer | A random transfer ID and bounded file manifest |
| FileAccepted | The matching transfer ID and first requested chunk index |
| Chunk | The matching transfer ID, index and bounded data |
| Cancel | Cancellation of the matching in-flight transfer |
| FileResult | Terminal committed, declined, failed or uncertain outcome for the matching ID |

The data endpoint uses pinned mutual TLS and the separate `peerglide-data/1`
ALPN. Input v5 and its ALPN are unchanged. TLS identity and local consent come
from the authenticated session. Receiving an offer alone neither opens a file
nor grants permission. The receiving controller still matches the transfer ID
and obtains explicit per-offer approval before it admits a chunk body or writes
to disk. A sender may use the accepted offset only if it fits the manifest for
that exact offer. Acceptance does not imply file publication.

A clipboard receipt indicates that the peer has a pending text snapshot. An
import result reports the local clipboard outcome separately. Neither allows
the peer to request system actions. Clipboard or file bytes stay out of logs.

Tests exercise bounded framing, fragmented and back-to-back input, wrong
direction and phase, and codec-to-storage behavior. File wire responses are a
foundation for the receiver and sender owners. Cross-host clipboard and file
acceptance require the matching application candidate and physical verification.
