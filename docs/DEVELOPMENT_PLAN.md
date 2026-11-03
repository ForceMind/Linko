# Development plan

1. Reproducible M0 tests and native CI; fix compile/link failures.
2. Executable loopback probe and negative authentication checks.
3. Native input sessions, extended HID and shortcut mappings, permissions UX.
4. Two-way edge return, local reclaim, collision denial, held-key disconnect
   recovery on real Windows/macOS computers with Bluetooth/USB and mixed DPI.
5. Functional eframe control panel using the same runtime; explicit persisted
   public-certificate trust and independent permissions.
6. mDNS, authenticated pairing UI, backoff reconnect with fresh leases.
7. Clipboard/files with grants, size limits, separate streams and consent.
8. Installable preview after physical P0 acceptance; signing/notarization and
   Linux backend. No hosted service deployment.

Use incremental branches and PRs, fmt, clippy with denied warnings, tests and
native matrix builds. Do not force updates or change repository visibility.

## Input correctness gate (before M2 expansion)

1. Retain/retry failed native releases and fail the session visibly; add injected
   transient/permanent failure tests. Implemented in the recovery follow-up.
2. Preserve Windows high-resolution wheel remainder. Implemented; native runtime
   acceptance still pending.
3. Edge-ordering regression reproduced on protocol v2 and fixed in v3 using
   source-originated reliable release. Real desktop acceptance remains pending.
4. Define unknown-key/capability behavior; expand layout and extended-key tests.
5. Add explicit capture-thread shutdown and reclamation before in-process reconnect.
6. Replace GUI stdout-prefix state parsing with structured worker events; allow
   immediate disconnect-and-revoke.
7. Run physical Windows/macOS acceptance matrix #3. No date or completion claim
   substitutes for recorded real desktop results.

## 2026-11-01 additive execution baseline

See [PRODUCT_EVOLUTION.zh-CN.md](PRODUCT_EVOLUTION.zh-CN.md) for task IDs,
priorities, dependencies, test matrices, release slices and continuing-work rules.
The earlier steps and input correctness gate above are retained, not replaced.

Immediate work:

1. **C01 / #10:** restore macOS CI at baseline `5617438`. Run `41226383167`
   fails the GUI headless-page test with filesystem error 2; Windows/Linux pass.
   Diagnose fixture isolation and cleanup before assigning a root cause.
2. **C02–C05 / #3 #5:** unknown-key policy, capture-thread ownership and teardown,
   structured worker events/revocation, and ordered-return/recovery regressions.
3. **C06 / #3:** authorized physical acceptance with exact OS/hardware evidence.
   If hardware is unavailable, record blocked and continue only independent work.
4. **P01–P04 / #4, U01–U02 / #5:** trust/grants, discovery, authenticated pairing,
   fresh-lease reconnect, desktop lifecycle and topology slices.
5. **G01–G05 / #11:** optional local-only gestures and typed actions on the shared
   input runtime; no script engine and no remote gesture interception in G1.
6. **D01–D04 / #6, R01–R03 / #12:** separately permissioned data collaboration and
   target-bound semantic actions. Native OS file drag remains its own gate.
7. **S01–S03 / #7:** security, packaging/signing and measured release acceptance
   throughout; optional expansion must not block an accepted scoped core preview.

Source pointers: `crates/peerglide-platform/src/lib.rs`, native adapters,
`apps/peerglide-desktop/src/session.rs`, `apps/peerglide-gui/src/main.rs`,
`peerglide-core`, `peerglide-protocol` and `peerglide-transport`.
`peerglide-actions` now contains pure model/preflight logic (G01);
`peerglide-gestures` contains bounded trajectory recognition and synthetic regression
tests (G02). No action executor or gesture hook is enabled.

Each development PR states task IDs, changed behavior, protocol impact,
regression evidence, untested platforms and rollback. Keep physical acceptance
separate from CI. Do not log key/clipboard contents, introduce a second global
hook stack, silently enable imported actions, or use a docs-only change to claim
that a runtime failure is fixed. See [ADR-0002](ADR/0002-unified-input-actions.md).


## Implementation checkpoint: 2026-11-03

This table records engineering status; physical acceptance remains #3. Latest
runtime evidence and historical failures are in VALIDATION.md.

| Task | Status | Evidence / remaining gate |
|---|---|---|
| C01 | implementing | #14 fixture isolation + macOS stress passed; historical root cause #10 still open |
| C02 | implementing | #15 recovery + #19 physical/keypad mapping; full OEM/media pending; input capability handshake added |
| C03 | implemented-unverified | #16 stop/join/reservation CI passed; native resource counts/revocation pending |
| C04 | implemented-unverified | #17 structured events/revoke CI passed; native interaction pending |
| C05 | implementing | v4 reliable terminal cursor + cleanup/expiry regressions; physical fault acceptance pending |
| C06 | blocked | No authorized physical Windows/macOS desktop pair in this environment |
| P01 | implementing | Bounded material loads, key-pair and exact enrollment-intent checks; OS storage/ACL and independent grants pending |
| P02 | implementing | Opt-in untrusted IPv4 mDNS hints and explicit listener publication; actual LAN/multiple NIC/permission acceptance pending |
| G01 | implementing | Local action/rule/preflight/result coordinator tested; native Backend and global capture remain absent |
| G02 | implementing | Bounded direction-preserving logic and synthetic corpus; human corpus/physical accuracy remain pending |
| G03 | implementing | Pure ownership/cancellation/edge model tested; native callback/queue wiring and C06 remain pending |
| G04 | implementing | Offline lab, exact app-rule editor/preview, versioned settings with backup/recovery; native app IDs/global worker/overlay pending |
| D03 | implementing (isolated foundation) | Offline bounded manifests/chunks/disk resume/integrity/no-overwrite commit; network/grants/GUI/quotas absent, M4 not accepted |


Protocol v5 is the current development baseline; both peers must update. The v3
source-ordered return safety remains, with a reliable terminal cursor added.
[ADR-0003](ADR/0003-terminal-cursor-v4.md) records migration and failure semantics.

P01 input capability slice is implemented; see ADR-0004. Discovery, interactive pairing, grants beyond input and reconnect are separate gates.


P04 fixed-address trusted reconnect is implemented behind an opt-in setting.
Fresh sessions/capabilities/control state, pin-change rejection and cancellable
backoff have automatic tests. Real interface/suspend/input acceptance, address
rediscovery and interactive pairing remain pending; see ADR-0005. G1 has isolated
G01 action-model and G02 recognizer foundations; G2 remains planned. No gesture interception
is enabled by this connection work.
