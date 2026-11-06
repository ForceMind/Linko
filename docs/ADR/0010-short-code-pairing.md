# ADR-0010: Ephemeral code pairing before pinned mutual TLS

Status: implementation in progress. The network authentication library is being
qualified; GUI enrollment and two-peer durable commit are not wired yet. This is
not production short-code availability or an independent cryptographic audit.

## Decision and compatibility

Keep input v5 and `peerglide/5` unchanged. Pairing is explicit and separate: input
code endpoint is OPAQUE client and TLS client, display code endpoint is both
servers. Use fixed `opaque-ke =4.0.1`, Ristretto255 / TripleDh / SHA512 and Argon2id
v1.3 (65536 KiB, 3 iterations, parallelism 1). No network-negotiated KSF, algorithm
downgrade, custom application MAC/cipher, accept-any TLS verifier or TOFU.

An offer displays eight uniformly random decimal digits generated from OsRng,
expires after 120 monotonic seconds and admits at most five TCP attempts across
all addresses/certificates. Charge before parsing or expensive computation;
disconnecting after KE2 still consumes an attempt. Code entry is consumed by one
explicit connection attempt: never automatically try the code against a list of
servers. These bounds limit online guessing, not denial of service. New offers
must be explicit user actions; reconnection must not silently refresh budgets.

Canonical PAKE context has a fixed domain/version/suite/role/input-version prefix
and four fixed 32-byte fields: client certificate BLAKE3, server certificate
BLAKE3, random offer ID, random attempt ID. Both endpoints calculate their own
identity from their actual local certificate. OPAQUE Identifiers explicitly bind
the two role-specific certificate hashes; all registration/login finish stages
use the same identifiers and login context. Credential identifier is the context,
not an authentication token. Equal local/remote identities reject.

The displaying endpoint knows the code. It runs the standard OPAQUE registration
APIs entirely locally for each frozen candidate, then runs the standard online
KE1/KE2/KE3 exchange. No registration record, password, export/session key or
certificate private key crosses the bootstrap connection. This composition does
not claim password hiding from the displaying endpoint or full augmented-server
compromise benefits. A global computation permit stays inside the blocking task
until KSF ends, even when its async waiter is cancelled; no recognition/crypto is
added to native input callbacks.

## Same-connection TLS upgrade

Plain TCP carries only bounded public bootstrap fields and standard PAKE messages.
It does not confer trust. Frames are u32 length (tag included), one-byte expected
tag, and payload; unknown order/version, oversized/truncated/trailing fields fail.
The OPAQUE decoder must also roundtrip all bytes, rejecting prefix-only parses.
Bootstrap magic is `PGPAIR01`, then 32-byte attempt/offer ID, u32 DER size, exact
DER. Client/server roles use distinct tags. No unbounded or buffered pre-reader.

Client writes complete KE3 only after its standard finish succeeds, then upgrades
the SAME TcpStream. Server reads exactly KE3 and runs its standard finish before
TLS accept. Any pipelined TLS bytes remain in that socket. Errors close it; no
second completion connection, reusable authenticated state, plaintext success
fallback or retry. Each handshake has a 20-second cap inside the original offer/
entry deadline. Cancellation/expiry is checked again before returning a result.

TLS uses independent per-attempt config, normal rustls/ring signature and server
name checks, exact authenticated DER roots, required client certificate, TLS1.3,
and distinct mandatory `peerglide-pairing/1` ALPN. Resumption/tickets/early data are
disabled. After handshake, verify full leaf DER, TLS version and ALPN again.
Both sides send their role-specific ready frame containing the entire canonical
context inside TLS, flush and verify the opposite role/context before returning a
privately constructed temporary PairingSession. No PAKE key is exported or used
in a new crypto construction. The same socket removes an independent connection
association surface; it is not itself a cryptographic proof. Certificate private
key compromise, hostile local processes and physical proximity are not covered.

## Enrollment and remaining gates

Network authentication alone never writes disk or grants input/data/actions. A
temporary result retains its original deadline/cancellation and authenticated
identity/context. GUI must cancel it on identity/config/target changes and close
the offer when its flow ends. The code is never Debug/logged/persisted/discovered.

Next: wire the Chinese offer/code-entry wizard to actual network progress; use
ADR-0008 local transactions with a fresh live result, not fabricated manual
fingerprint confirmation. Bind two-peer commit messages to the same authenticated
context, preserve closed input/receiving/reconnect, and represent lost acknowledgments
as commit unknown. Local durable intent cannot be rolled back as if cancelled;
later cancellation is revocation. No claim of a distributed atomic commit.

Required validation includes actual TCP/PAKE/TLS success, wrong code, identity/
role/context substitution, replay, malformed/pipelined/fragmented boundaries,
wrong/missing ALPN, strict certificate rejection, exhaustion, cancellation/expiry
through KE3/TLS/ready and durable single-side-commit/lost-receipt recovery. These
are network/process tests, not physical C06 or full independent security review.

## Sources and review scope

- [Pinned upstream 4.0.1](https://github.com/facebook/opaque-ke/tree/75fe4cdddb7946440054da0c8e7cdd73828af3f9), MIT OR Apache-2.0, Rust 1.85.
- [RFC9807](https://www.rfc-editor.org/rfc/rfc9807.html), especially registration, identities/context and online guessing.
- [tokio-rustls 0.26](https://github.com/rustls/tokio-rustls/tree/v/0.26.4), upgrade takes the existing IO.

Scope review found no requirement to modify OPAQUE internals for this local
registration/identity binding and same-socket upgrade. It is source/composition
review only, not an independent audit or proof of this product composition.
The upstream 2021 NCC audit of older versions does not cover 4.0.1 automatically.
Locked transitive licenses, advisory checks, MSRV and platform CI remain required.
