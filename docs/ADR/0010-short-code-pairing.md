# ADR-0010: Ephemeral code pairing before pinned mutual TLS

Status: experimental network foundation and GUI durable commit are wired and
automatically qualified in PR58; physical cross-host acceptance is still pending.
Prerelease dependency admission and production
review remain open. This is
not production short-code availability or an independent cryptographic audit.

## Decision and compatibility

Keep input v5 and `peerglide/5` unchanged. Pairing is explicit and separate: input
code endpoint is OPAQUE client and TLS client, display code endpoint is both
servers. The initial `opaque-ke =4.0.1` candidate failed dependency audit. The
experimental repair pins official revision `027abbc91115baa38b70ad3af7f5a22f072f978b`
(package label 4.1.0-pre.2, different from that published crate), with
Ristretto255 / TripleDh / SHA512 and Argon2id
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
Bootstrap magic is now `PGPAIR02`, then 32-byte attempt/offer ID, u32 DER size, exact
DER. Client/server roles use distinct tags. No unbounded or buffered pre-reader.

Client writes complete KE3 only after its standard finish succeeds, then upgrades
the SAME TcpStream. Server reads exactly KE3 and runs its standard finish before
TLS accept. Any pipelined TLS bytes remain in that socket. Errors close it; no
second completion connection, reusable authenticated state, plaintext success
fallback or retry. Each handshake has a 20-second cap inside the original offer/
entry deadline. Cancellation/expiry is checked again before returning a result.

TLS uses independent per-attempt config, normal rustls/ring signature and server
name checks, exact authenticated DER roots, required client certificate, TLS1.3,
and distinct mandatory `peerglide-pairing/2` ALPN. Resumption/tickets/early data are
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

After dependency/upstream and network qualification: wire the Chinese offer/code-entry wizard to actual network progress; use
ADR-0008 local transactions with a fresh live result, not fabricated manual
fingerprint confirmation. Bind two-peer commit messages to the same authenticated
context, preserve closed input/receiving/reconnect, and represent lost acknowledgments
as commit unknown. Local durable intent cannot be rolled back as if cancelled;
later cancellation is revocation. No claim of a distributed atomic commit.

PR58 implements those GUI/worker/guarded local transaction and same-context
Prepared/Saved barriers (see VALIDATION.md for exact commits and test scope).
The subsequent 0.3.6 discovery slice uses a separate, opt-in
`_peerglide-pair._tcp.local.` hint with TXT pairing version2/input version5.
Selection only fills an address and clears any previous code; it never trusts
the hint's fingerprint, changes the PAKE context, retries a code against a list
or refreshes deadlines/budgets. Public advertisement is a second explicit action
limited to the actual worker's Listening phase and withdraws on cancellation.
No code is included in DNS. See DISCOVERY.md for privacy, bounds and cache limits.

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

The first locked CI (898d33c, run42225212344) passed Linux/macOS workspace tests
and MSRV, but failed the unchanged audit gate: mandatory elliptic-curve0.13 brings
crypto-bigint0.5.5, affected by
[RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387.html), fixed only
in >=0.7.6. No ignore or downgrade of the gate is permitted. Do not wire this
candidate into GUI/production or describe it as qualified. Check a compatible
upstream repair or review an alternate standard PAKE before proceeding; do not
force incompatible bigint types or invent cryptographic repairs locally.

The official fixed revision above changes elliptic-curve to0.14.3 (bigint>=0.7.8),
dalek to5, Argon2 to0.6 and voprf to0.6.0-pre.2; it also refactors shared key
exchange helpers. This is not a manifest-only backport, and voprf remains a
prerelease. Scope review found the selected ordinary TripleDh transcript/DH order/
labels and Ristretto encoding semantics retained, with official RFC9807 and frozen
4.0.1 compatibility fixtures. Upstream exact-revision CI42108218606 is successful;
it does not replace PG validation or independent cryptographic review.

Qualification adds a separate exact-source upstream test job with Rust1.95 and
only ristretto255/argon2, plus a PG65536/3/1 output fixture generated using old
Argon2 0.5.3. Keep the new git revision immutable, record its tested transitive
lock, rerun unchanged strict audit/MSRV/platform checks, and keep GUI closed.
Never silently substitute floating main, the same-name published prerelease, or
SPAKE2.finish: that API returns a key without confirming equal codes; a direct
replacement followed by certificate TLS would not prove the short code.

PG runtime3f7d57b passed all five platform/MSRV/audit jobs in42228351335 and the
separate upstream test job42228351494. Fixed KSF compatibility, actual network
authentication and negative/cancellation cases passed. This qualifies the scoped
experimental network foundation only, not GUI enrollment or a production security
review. See VALIDATION for exact evidence and unfinished integration requirements.

## Durable GUI integration constraints (planned, not yet enabled)

The parent supervisor is a separate owned-child channel. Its Authenticated
observation is not a save capability. Pending enrollment must block real session
start paths, not just receiving/native/reconnect preferences or disabled buttons.
The final snapshot/identity checks and one-shot save claim belong inside the
settings-locked ADR0008 operation, after backups and before creating the journal.
Cancel and claim need one atomic ordering point; accepted intent cannot be undone
as if cancelled. There is no globally instantaneous cross-machine cancel order.

Both users approve; both children exchange role-specific Prepare before asking
their parents to save. Only a fully successful local transaction may emit Saved.
Pending receipt metadata should be journaled within settings; restart must not
reconstruct Session or auto-confirm. Lost LocalSaved notification stays unknown.
Peer Prepare/Saved may arrive before local approval/save and needs bounded storage;
local commands remain phase-specific, with allowed state published before events.

Confirmed means this machine durably recorded evidence of both Saved messages,
not that the peer also confirmed or both children exited. A later normal EOF must
not erase verified PeerSaved evidence; later revoke/replacement must invalidate it.
Version/context/identity inconsistencies remain read-only. The commit channel now
uses explicit v2 bootstrap/context/ALPN rejection of experimental v1; input v5 and
the manual fallback remain unchanged. Both devices need the matched v2 candidate
for this experimental pairing feature, with no fallback to v1 authentication or
fabricated manual confirmation. Existing input identities are not regenerated.

The experimental channel transfers the same live Session without resetting its
original deadline, uses one persistent frame reader with a two-item queue, and
binds role-specific Prepared/Saved frames to the entire authenticated context.
Cancelling a receive does not discard a partially read length/payload. Saved
requires local prepare and receipt of peer prepare; duplicate/order/context/role
violations reject. This messaging primitive does not verify a local filesystem
save or confer trust: worker approval/ack state, parent save-claim, receipt journal
and GUI wiring still require implementation and tests before GUI trust writes.
