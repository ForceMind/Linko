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
Verified continuation #27–#30 delivers reference gesture arbitration, exact-ID
rule editor/preview, versioned configuration recovery, local action results/corpus
preflight, opt-in untrusted discovery and maintained fonts/advisory gates. Latest
merged baseline #30 is `759e61a1b9296c1f6c0b1bf50b529e2e6167947c`, candidate
run `41595532009` passed Windows/macOS/Linux and independent audit with identical
merged source tree. Current detailed evidence belongs in VALIDATION.md and each PR.
D03 offline storage follows as an independent foundation, without a network send UI.


| Milestone | Implementation | Acceptance still required |
|---|---|---|
| M0 workspace/protocol/security/state/CI | Implemented engineering foundation | Native matrix passes; historical macOS root cause #10 still open |
| M1 two-machine input | Prototype implemented | Real Windows/macOS sessions, Bluetooth/USB, DPI, recovery #3 |
| M2 peer collaboration | Partial: symmetric leases, input capabilities, opt-in reconnect and untrusted IPv4 discovery hints | LAN/NIC discovery acceptance, interactive pairing, address/suspend/native acceptance, independent grants #4 |
| M3 GUI | eframe devices, untrusted discovery, rule preview/editor, versioned config recovery | Native runtime validation; pairing codes, tray, full topology #5 |
| M4 clipboard/files | D03 offline chunk/storage/resume foundation; no network send or clipboard | Separate grants/data stream/consent/GUI/quotas; native drag separate #6 |
| M5 release | Planned | Performance/endurance, installers, signing/notarization, Linux input #7 |
| G1 optional local mouse enhancement | G01/G02 models, reference G03, offline canvas/rule editor, native foreground/focus sampler and permission diagnostics; no global gestures/action execution | Human corpus, native broker/continuous context invalidation/tokens/Backend, global offline worker, overlay and physical acceptance #11 |
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
