# G01 action model foundation

`peerglide-actions` implements the pure, local-only preflight portion of
[ADR-0002](ADR/0002-unified-input-actions.md). No desktop binary depends on it yet.
It does not install hooks, dispatch OS input, execute actions or add a network RPC.
G01 remains implementing; this is not a usable gesture feature.

## Stable semantics

`ActionSpec` has a required schema version (currently 1) and a closed action ID.
Unknown fields/IDs and missing fields fail deserialization; unsupported schema
versions fail validation. There are no raw keycodes, shell commands, file paths
or arbitrary parameters. Configuration migration is a later G04 task.

| IDs | Meaning / policy |
| --- | --- |
| `copy`, `paste` | Semantic application operations; no Ctrl/Command substitution in this crate |
| `back`, `forward` | Navigation, not raw left/right keys |
| `close_tab`, `close_window`, `quit_application` | Distinct destructive operations, explicit confirmation required |
| `minimize_window`, `maximize_window` | Window operations; maximization is not fullscreen |
| `toggle_fullscreen` | Toggle, never automatically replay after an unknown outcome |
| `show_peer_glide` | Show the product's UI |

Risk is a product confirmation policy, not a sandbox or promise that other actions
cannot modify data. Even Copy may overwrite the clipboard. Capabilities default
to empty and describe adapter support only; they cannot enable local consent.
There is no conversion from KVM trust, grants or input capabilities.

## Context and dispatch boundary

`Candidate` freezes the device, opaque target, focus/configuration generations,
trigger sequence and local monotonic expiry. `validate` checks schema, local-only
target, consent, support, exact context equality, deadline and destructive consent.
It returns a non-cloneable `ValidatedAction`, not an execution result. Consuming
`revalidate` repeats all checks with current policy/capabilities before a future
adapter submits to the OS. At the exact deadline the candidate is expired.

Tokens are caller-supplied process-local data. This crate neither authenticates
them nor reads a real window/focus. A future adapter must mint/rotate tokens and
recheck focus/OS permissions immediately at submission; pure preflight cannot
make the check/dispatch interval atomic. The model never changes focus or follows
a new target. No context is serialized for remote use.

All Remote candidates return `RemoteUnavailable`, without local fallback. G2
needs independently authorized actions, authenticated context, deduplication,
ordering and honest results under ADR-0002. Replay classification is metadata;
there is no automatic retry, dedup cache or exactly-once claim in this crate.
OS submission must not be reported as application completion by a future backend.

## Verification and remaining work

Pure tests cover frozen IDs/strict schema, empty support/default denial, remote
denial even with full local consent, each stale-context dimension, exact expiry,
revocation after validation and destructive confirmation/replay distinctions.
They contain no real OS execution or gesture recognition.

Native action adapters, result reporting, application rule resolution, context
minting, human gesture dataset acceptance, shared input arbitration (G03), GUI,
offline worker and versioned rule migration (G04) are not implemented here.
No native gesture interception may be considered releasable before C06 real
two-computer acceptance. Physical acceptance #3 and historical macOS root cause
#10 remain open; this foundation does not resolve either.

G02 now has an isolated pure recognizer and synthetic corpus; see [GESTURES.md](GESTURES.md).
It does not execute these actions or establish real-user gesture acceptance.

`execution::Coordinator` now provides a bounded local request/result contract.
It uses the same preflight, a maximum 5-second lifetime, up to 128 live retained
results and no automatic replay. Same ID/body returns historical status; changed
body rejects. UnknownOutcome remains unknown; Submitted is only OS submission,
not application completion. Expired retention can be pruned, so this is not
cross-process or remote exactly-once delivery. Backend is a trait with mock tests;
no native backend is instantiated and remote requests still reject. Native
implementations must revalidate the OS target/permissions at dispatch and clean
up only their owned ActionInjected state. This is not a G2 protocol or grant.


The platform now provides real read-only foreground/focus observations and explicit
permission diagnostics; see [NATIVE_CONTEXT.md](NATIVE_CONTEXT.md). These snapshots
do not mint action Context tokens or LocalPolicy, cannot detect transient
away-and-back focus changes, and cannot authorize native execution. Backend and
continuous target invalidation remain required.


The platform's `context::actions::LocalActions` now implements an independently
consented native window Backend: Windows min/max/close, macOS public AX min/close.
It consumes the real native binding and rechecks original deadlines/target/policy
at submission. Default-off consent replacement/revocation invalidates old
triggers; no shipped binary enables it. Keyboard/other semantic actions, G03
input integration, worker consent UI and physical effects remain incomplete. See
[LOCAL_WINDOW_ACTIONS.md](LOCAL_WINDOW_ACTIONS.md) and candidate validation.
