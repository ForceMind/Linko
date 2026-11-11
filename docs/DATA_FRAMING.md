# Isolated data framing v1

codec::Frame reads/writes Unicode text, a file manifest offer, a bounded file
chunk or cancellation. Fixed 12-byte PGD1 header, closed kind/flags/reserved
fields and per-kind body limits are checked before allocating a body. Invalid,
unknown, truncated or oversized frames terminate the stream. Never resynchronize
by scanning for magic in payloads. Writer validates before emitting any bytes;
partial I/O failure is uncertain and must not trigger blind retries.
The validated header exposes a bounded kind and length to the transport. The
reader can reject a direction or exhausted budget before allocating or reading
the body. An async adapter can use the same header and exact-body decoder.

The codec uses Read/Write and handles fragmented/back-to-back frames. Tests cover
header-only oversize rejection, truncation at each byte, bad UTF-8/sequence/index,
flags, maximum-size chunks and a real codec-to-authorized-manager-to-disk flow.

This is an OFFLINE codec, not an activated network protocol or v5 negotiation.
Do not send it to the input stream or accept it automatically on existing peers.
A future ADR must define an independently negotiated authenticated data stream,
deadlines, concurrency/session-byte limits, bounded queues/backpressure, transfer
responses and per-offer UI before enabling it. TLS identity/session binding comes
from the connection, never from a Frame. Receiving an Offer is not local consent.
Cancellation must be bound to that authenticated session's known transfer ID.
Input v5/ALPN remains unchanged. Real QUIC scheduling and cross-host tests remain
open; synchronous hashing/disk/framing must stay off the input callback/runtime.
