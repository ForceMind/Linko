# Roadmap

## Planning extension — 2026-11-01

The existing M0–M5 identifiers below remain authoritative; none are renumbered
as gesture milestones. Read [the staged product/development baseline](PRODUCT_EVOLUTION.zh-CN.md),
[MouseInc/Aitiy research](RESEARCH_MOUSEINC_AITIY.zh-CN.md), and
[ADR-0002](ADR/0002-unified-input-actions.md) for the additive G1/G2 tracks.
These are plans, not implemented-feature claims.

Historical planning baseline `56174387563ba6a087c7db35c9c5e1e1031e593c` failed
macOS GUI test in run `41226383167`; release/smoke/package were skipped then.
This evidence is retained; #10's historical root cause is still unconfirmed.
The latest merged connection increment is #22 (`f5c8505fe8f161a946e12f684e593037df525f3e`),
with complete Windows/macOS/Linux CI in run `41393650378`. G01 pure action-model
work merged via #23 (`99245e3`); main run `41395302222` passed. G02 pure recognizer
work follows separately; current evidence belongs in VALIDATION.md.

| Milestone | Implementation | Acceptance still required |
|---|---|---|
| M0 workspace/protocol/security/state/CI | Implemented engineering foundation | Native matrix passes; historical macOS root cause #10 still open |
| M1 two-machine input | Prototype implemented | Real Windows/macOS sessions, Bluetooth/USB, DPI, recovery #3 |
| M2 peer collaboration | Partial: symmetric leases, input capabilities, opt-in fixed-address trusted reconnect | mDNS, interactive pairing, address/suspend/native acceptance, independent grants #4 |
| M3 GUI | Basic eframe GUI implemented | Native runtime validation; pairing codes, discovery, tray, full topology #5 |
| M4 clipboard/files | Planned | Unicode/images; resumable integrity-checked files; native drag separate #6 |
| M5 release | Planned | Performance/endurance, installers, signing/notarization, Linux input #7 |
| G1 optional local mouse enhancement | G01 action model + G02 pure recognizer implementing; no native execution | Human corpus, shared input broker, local-only runtime, app rules and physical acceptance #11 |
| G2 cross-device semantic actions | Planned, after G1 and trusted sessions | Separate grants, target context, ordering, deduplication and Win/Mac acceptance #12 |

The first supported release requires P0 real-machine acceptance. CI artifacts
are prototype binaries and do not prove desktop compatibility. G1/G2, OCR and
scripts do not block a correctly scoped, accepted core preview. M5 hardening
runs throughout; Linux and full topology remain open original milestone work,
not implied support in the first two-peer primary-display slice.

## M1 implementation/acceptance checklist

- [x] Windows/macOS capture and injection source with platform CI
- [x] Authenticated two-process communication
- [x] Primary display edge and keyboard-follow session code
- [x] Relative side and normalized resolution model
- [x] Recovery state machine and fault-path tests
- [ ] Physical Windows/macOS Bluetooth and USB control
- [ ] Real mixed-DPI, IME/shortcut, fullscreen and permissions checks
- [ ] Real two-machine fault/recovery acceptance

5/8 tracking gates complete (about 63%). This measures engineering gates, not
product readiness. No physical acceptance gate is checked without evidence.
Passing current CI does not establish the historical macOS failure root cause
or substitute for any physical acceptance gate.

## Dependency order

1. Continue #10 root-cause investigation, input correctness and #3 physical acceptance.
2. Finish the daily-use M2/M3 connection, lifecycle and permission slice.
3. Add G1 locally, with M4 text clipboard independently deliverable.
4. Add image/file collaboration and separately authorized G2 actions.
5. Release only the accepted scope through M5; retain deferred native drag,
   topology, multi-peer, Linux and optional OCR/workflows as explicit open work.

Pure gesture recognition and action-schema tests may run in parallel, but native
interception cannot bypass the existing correctness/physical acceptance gates.
