# Validation record

2026-10-22, isolated cloud Linux development environment. No user Mac/PC changes.

| Check | Result |
|---|---|
| cargo test --workspace | Initial 8 tests passed; expanded session/GUI/security suite recorded in CI: framing, lease authorization/conflict/replay/expiry, pressed release, edge gate, authenticated QUIC/datagrams and untrusted-client rejection |
| cargo fmt --all; cargo clippy --workspace --all-targets -- -D warnings | Passed |
| platform clippy/check, x86_64-pc-windows-gnu | Passed (cross compile; no desktop run) |
| platform clippy/check, aarch64-apple-darwin | Passed (cross compile; no desktop run) |
| Initial native Windows/macOS/Linux GitHub CI | Passed: run 39729892588, commit 0fa2316; fmt, clippy, tests, release/link and artifacts on all three OSes |
| Real Windows ↔ macOS with Bluetooth/USB | Not run: no physical desktop sessions available |

Pending physical checks: all four edges and return; held keys/buttons; Ctrl/Alt/
Command mappings; simultaneous local input; IME layouts; 100–200% Windows scaling
and macOS Retina; permission denial; Secure Input; elevated windows; fullscreen;
packet loss/jitter; process/network failure while dragging or holding modifiers;
sleep/wake. Full topology, numpad/media/OEM key coverage, native drag, clipboard,
files, full GUI/tray, discovery, auto-reconnect, signing and installers remain incomplete.

Cross compilation proves source typechecking only. Native CI proves compilation,
linking and noninteractive tests only. Neither proves physical desktop behavior.

Second implementation adds GUI headless page rendering in both themes, enrollment
confirmation/pinning tests, stream-size and wrong-server rejection, graceful
GUI worker stop and native modifier/hotkey mapping tests. Two real CLI processes
passed authentication/probe/clean-exit smoke locally. Current cloud container
blocks display sockets, so native GUI-window smoke and screenshots run only on
the isolated Linux CI runner. Windows/macOS desktop interaction remains untested.

License metadata is screened with allowed SPDX alternatives (including MIT
instead of optional LGPL choices). Bundles collect upstream notices and include
published-source fallbacks for archives lacking separate license files. This is
not a completed legal audit; inspect those notices before a stable release.

Expanded CI caught a Linux GUI runtime dependency missing from the runner
(`libxkbcommon-x11`). The workflow now installs the documented X11/EGL/Mesa
runtime libraries before launching its virtual display. The correction is being
verified on the final PR head; the failed checkpoint is not counted as passing.
Protocol v2 adds reliable cursor anchors for discrete input and first-motion
edge return, with dedicated loss/replay and real-session return tests.


## Recovery hardening follow-up

Pending-release fault tests cover transient injector failure, permanent failure
with a bounded three-attempt flush, retention for final cleanup, and continued
release of other inputs when one release fails. Session recovery now propagates
cleanup failure and stops the session; Drop performs another bounded cleanup
attempt and reports incomplete cleanup without logging key contents.

Windows wheel deltas now retain sub-detent remainder independently on each axis.
A regression test covers accumulation, direction reversal and multi-detent input.
These tests do not prove OS acceptance: persistent OS rejection or process death
can still prevent synthetic release. Physical acceptance remains issue #3.


## Ordered return regression (protocol v3)

A live QUIC regression sends an edge datagram before deliberately delayed reliable
button input. The v2 implementation fails: it immediately sends Release instead
of retaining the lease. In v3, the edge datagram does not revoke the lease; delayed
input and heartbeat remain valid, and a subsequent reliable Release allows a new
lease. Source boundary tests cover all four edges and immediate reverse motion.

Local workspace tests (22 Linux tests), strict clippy and fmt passed. Real OS
input acceptance remains pending. Updating one peer alone is unsupported; v3 ALPN
intentionally rejects v2. Safety timeout and local preemption are still allowed
to interrupt input without draining it.


## C01: fixture isolation investigation (2026-11-02)

Historical main run 41226383167 failed only the macOS GUI render test with an
uncontextualized os error 2. Runs 41321441089 and 41321555098 passed without a
runtime fix; neither proves the race cause. PR #14's initial run 41324173162
failed rustfmt on all platforms, before any test executed.

Follow-up reserves each fixture with exclusive create_dir, adds per-process
sequence names, RAII fallback cleanup, explicit contextual cleanup errors and a
32-thread fixture ownership/cleanup regression. macOS CI executes the render
test alone, then all GUI tests 20 times with eight test threads, recording OS and
compiler. No test is skipped and failed tests are not retried to mask a failure.
Only fixture-name reservation retries AlreadyExists, never test execution.

The historical timestamp collision remains a hypothesis: the original log did
not identify the failed I/O operation or preserve allocated paths. A green final
matrix and stress run demonstrate the new fixture behavior, not proof of the
original failure's root cause. Issue #10 remains open pending that evidence.
Physical Windows/macOS input acceptance #3 is unaffected.


C01 fixture candidate `3260af9b26cea2c0696c6341ef5c4050cdf84f02` passed complete
Windows/macOS/Linux CI run 41324690497, including the isolated macOS test and all
20 parallel iterations; merged via #14. Historical root cause remains open #10.
C02 first slice `bbb34d8acf85fc07e4475db8a1967114ab6de745` passed native matrix
41324857672 and merged via #15. See INPUT_COMPATIBILITY.md for remaining C02 gaps.

## C03: cooperative capture ownership

CaptureSession now has explicit shutdown; source suppression is disabled before
bounded join. A process-wide reservation survives timed-out owners until the
native thread really exits, preventing overlapping hooks. Windows message waits
and macOS run-loop slices bound idle shutdown checks to 50ms; cleanup unhooks or
disables/releases the native tap before reporting completion.

Local workspace tests, strict clippy, Windows/macOS target clippy including tests,
Debug build and two-process QUIC/graceful-stop smoke passed. Lifecycle tests use
fake loops: repeated joins, partial initialization exit, bounded timeout/recovery;
native platforms additionally test exclusive reservations. Real native thread and
handle counts/repeated permission-granted capture remain unverified. C03 remains
implemented-unverified for those gates. See CAPTURE_LIFECYCLE.md. C04 structured
worker events and C05 final-pointer/recovery regression expansion remain pending.


## Subsequent C03/C04 validation and C05 boundary regressions

C03 candidate `fd1948eb1c8c9cc2736b9f1c87108269e13f7fd4` passed full native
matrix 41325579471 and merged via #16. C04 candidate
`193c067aef49ef7f36dc4376771b99993784f4dd` passed native matrix 41325829626 and
merged via #17. Earlier statements that C04 was pending describe the C03
checkpoint, not current implementation. The integrated main dbef1d3eecb9d4a1cd21771650cdf8172766f956
passed local 29-test workspace/clippy and managed two-process smoke.

C05 now adds exact-deadline tests: heartbeat and cursor anchors cannot revive an
expired lease, pressed modifiers/buttons still produce balanced cleanup, repeated
cleanup is empty, and delayed old-lease reliable/datagram events cannot contaminate
a new lease. Final-pointer delivery and full fault/session regression expansion
remain pending; these tests do not claim those capabilities or physical acceptance.


C03 bounded-join follow-up: a completion notification can precede blocked TLS
cleanup. The new regression failed before the fix (shutdown returned success
only after rescue released the destructor despite a zero budget). The first is_finished-based fix also failed all three platforms in run
41327242373; it was not merged. A dedicated reaper now owns join and the capture
reservation through all thread-local destruction. The caller waits only on a
bounded acknowledgement, so timeout never enters join on the caller thread. This does not establish real OS handle-count acceptance.


## C02 physical scan / keypad slice

Adds physical Windows scan capture/injection with E0 flags, distinct keypad Enter
and navigation keys, startup VK sample replacement, and macOS keypad/ISO mappings.
Local workspace tests/strict clippy plus native-target platform clippy passed.
Native CI and Apple SDK constant verification are required on the final candidate.
Real keypad, NumLock/layout/IME and mixed-keyboard behavior remain #3 acceptance;
C02 is still implementing, with media/full OEM/capability work pending.


## C05 terminal cursor / protocol v4

Protocol v4 carries a final cursor in normal reliable Release. Tests cover total
motion loss, invalid/foreign/expired anchors, pointer failure with balanced key
cleanup, retained injector failures, late/duplicate leases, wire roundtrip, real
session release and v3 ALPN rejection. Safety interruption does not require an
anchor. Local workspace tests (39 Linux), strict clippy, Debug build and managed
smoke passed; native matrix and physical dragging/DPI/fault acceptance remain
separate. See ADR-0003 for the mandatory two-peer upgrade and unchanged trust.
C02 physical-key candidate 84d06d2b05a19ccba2171881c4108700da435a1a passed all three
platforms plus active Apple SDK constants in run 41329928375, merged via #19.


## P01 input capability slice (protocol v5)

Local workspace/strict clippy, Windows/macOS platform cross-clippy, Debug build
and managed two-process smoke passed. Tests cover bounded metadata/reserved bits,
required Hello metadata, real-session unsupported input rejection and diagnostic
endpoint denial despite allow-control. Native hook tests cover local delivery of
unsupported presses, button/wheel fail-open and unavailable-target hotkeys. Native
CI and real OS/input/permission acceptance are separate. Full OEM/media, clipboard/
file/action capabilities, pairing/discovery and reconnect are not completed here.


P01 final candidate dfe53ce01f44c1e259318ea62cefb04d21c8c008 passed complete
Windows/macOS/Linux CI run 41391030792 and merged via #21. This validates compiled
metadata/callback policy, not real hardware or OS permission availability.

## P04 fixed-address trusted reconnect

Local tests cover capped backoff, non-retryable security/native failures, pin
replacement/deletion, held-switch-repeat rejection and old-config default-off.
Real loopback processes authenticate, lose the server, reconnect to the same
identity, reject a different valid replacement identity despite a modified pin
file, and stop during backoff. GUI tests keep a real helper child alive while
logs fill their queue and latest state overrides stale authentication. Probe
success survives state coalescing. Native CI, real desktop input/network/suspend
acceptance and resource/latency measurements are separate gates. No old lease or
suppression state is resumed; no new pairing/discovery mechanism is claimed.


P04 final candidate `3c5b82f8ead0a27b65cd5d5dc9b050ae466ba7e1` passed the complete
Windows/macOS/Linux matrix in [run 41393650378](https://github.com/ForceMind/PeerGlide/actions/runs/41393650378),
including reconnect/pin-replacement/stop smoke and platform package construction.
Squash merge `f5c8505fe8f161a946e12f684e593037df525f3e` has the identical tested tree.
This does not validate native physical input, interface changes or suspend.

## G01 pure semantic action model

`peerglide-actions` has closed schema-1 IDs, independent default-empty capabilities,
explicit local consent, destructive confirmation, conservative replay metadata and
frozen local context/deadline preflight. Tests reject changed target/device/focus/
configuration/trigger, exact expiry, revoked consent/support, remote targets and
unknown action/fields/schema. No binary integrates it; no OS action, gesture hook,
remote action execution or configuration migration is implemented. See ACTIONS.md.
Native CI for this new crate is a separate gate; pure tests do not establish native
window/focus/permission correctness. G01 remains implementing; G02 remains planned.

G01 local fail-fast checks passed: fmt, 56 Linux workspace tests, strict workspace
Clippy, Debug build, managed two-process smoke and reconnect/pin-swap/backoff-stop
smoke. No new external dependency was added; Cargo.lock registers the new workspace
crate using existing Serde dependencies.


## G02 bounded pure recognition (2026-11-03)

G01 final candidate `23fee47002e59890e489730bf3eaa74b367325d3` passed three-platform
run 41394743671, merged as `99245e3adf4830535b0852dd64d1352b599ea383` with identical
tree; main run 41395302222 also passed. Native action execution remains absent.

G02 adds direction/order-preserving arc-length matching, distinct-ID ambiguity
rejection, finite configuration/coordinates, duration/length/sample/template bounds
and a consuming collector that poisons cancelled/invalid/overflowed candidates.
Its 9 regression tests plus the previous workspace tests passed locally (65 total),
with fmt, strict Clippy, Debug build and real managed/reconnect/pin-swap/stop process
smokes. A literal fixture has 8 synthetic positives and 8 negatives; fixed seed
0x504720261103 adds 512 positives (512 correct, 0 wrong/rejected) and 512 selected
negatives (0 matches). These are synthetic regression results, not human accuracy,
physical mouse-rate/DPI acceptance or dangerous-action misexecution measurements.
The straight-drag test deliberately matches: input intent needs G03 arbitration.

Complete native CI on the final G02 candidate is a separate gate. No binary uses
this recognizer, no input hook or action execution is added, network stays v5.
The planned 800 human positive / 2,000 human negative recordings and real Win/Mac
C06 acceptance remain pending. See GESTURES.md for scope, limits and reproduction.


G02 final candidate `36ee7e8dcf8c02b4c07c3d14133a198406e4b82e` passed complete
Windows/macOS/Linux CI [41578198350](https://github.com/ForceMind/PeerGlide/actions/runs/41578198350),
including explicit synthetic metrics, macOS GUI stress, process smokes and packages.
Squash merge `e3656fe7d01cd5e815fb518d9139577b7be9ab6f` has the identical tested tree.
This does not change any human/physical acceptance gate.

## P01 bounded identity material and exact enrollment intent (2026-11-03)

A new mismatched-key load regression failed on the existing implementation:
Identity::load accepted a generated certificate paired with another generated
identity's private key. It now passes with required positive rustls/ring SPKI
consistency at load, before GUI identity display or endpoint construction.
A second GUI regression failed before the confirmation fix: replacing valid
certificate A with valid B at the same path after checkbox consent enrolled B.
Confirmation now freezes A's fingerprint and enrollment independently re-reads
bounded bytes, rejecting B until explicitly confirmed. Failed/oversized imports
preserve the existing enrolled bytes/fingerprint; normal explicit reenrollment
still works. Consent is process-local, not persisted or restored from config, and revocation
invalidates any pending confirmation before filesystem changes.

Local fail-fast fmt, 70 Linux workspace tests, strict Clippy, Debug build and
managed/reconnect/pin-swap/backoff-stop process smokes passed. Additional tests
cover malformed/empty/oversized certificate/private key, directories, byte-limit
boundary, legitimate identity roundtrip, non-overwrite and Unix private key mode.
Complete native CI on the final P01 candidate remains a separate gate.
Network stays v5; neither parsing nor matching keys grants control permissions.
OS keystore/ACL, atomic identity-file transaction and malicious-local-filesystem
hardening are still pending; see SECURITY.md. No hardware acceptance is claimed.


P01 final candidate `c5b0fad08aa29fd00aa5ed72504544acf73dbaf0` passed complete
Windows/macOS/Linux CI [41579175459](https://github.com/ForceMind/PeerGlide/actions/runs/41579175459).
Squash merge `302cecee256696620844a2011d7ba21d7cb7695e` has the identical tested tree.
This includes revoke invalidation of pending confirmation; OS keystore/ACL and
physical acceptance remain separate.

## G04 independent offline canvas lab (2026-11-03)

The GUI now uses G02 inside its own explicit drawing canvas, with no pairing,
identity, network or OS input-permission prerequisite. Right-button drawing
recognizes/rejects; expected labels never bias recognition. Only Save creates a
new labelled JSON file; cancelled/incomplete prefixes and repeated saves reject.
No sample is automatically uploaded or written. Existing device sessions disable
recording; focus/page/geometry/compound-input loss and no-motion timeout cancel.
This adds no global hook, ordinary-right-click replay or OS action execution.

Five new tests cover frame-delivered egui pointer events, recognition, focus/idle
expiry/worker cancellation, geometry/late release, label independence and explicit
nonduplicate exports. All pages render in both themes including Gesture lab.
Local fail-fast fmt, 75 Linux workspace tests, strict Clippy, Debug build and
managed/reconnect/pin-swap/stop smokes passed. Python GUI smoke syntax checks passed.
Native matrix and Linux Xvfb launch/screenshot evidence for Home and --gesture-lab
remain a separate gate. Windows/macOS real desktop drawing is not verified here.

These GUI samples identify their source as egui_canvas with logical coordinates
and frame-clock elapsed time; they do not prove hardware polling/DPI acceptance.
No human corpus was collected, and 800/2000 human and C06 gates remain pending.
G04 overall is implementing: global offline worker, broker, overlays, app rules
and config migration are not delivered. See GESTURE_LAB.md for user workflow.


## Cloud foundation continuation (2026-11-03)

Local fmt/strict Clippy, 90 workspace tests, Debug build and process smokes pass.
G03 ownership is a worker/reference model; native callback integration is absent.
Rules have exact-ID specificity/exclusions and compile-time conflict rejection.
GUI rule editing/preview, versioned configuration, byte-exact backups, reset,
cooperating-writer hash checks/OS locks and a single-GUI guard are implemented.
Changing the peer clears the old input grant. See CLOUD_FOUNDATION.md for bounds.

A parallel test exposed inherited/duplicated file descriptors retaining a lock
after the original File dropped. Explicit unlock in RAII guard fixes it; the
regression retains a cloned descriptor while reacquiring, without retries. The
previous persistence-failure test now injects an invalid settings.json directory
instead of obsolete fixed settings.tmp; the failure/denial contract is preserved.
Native CI/real UI/focus and hardware remain separate gates; no root-cause claim #10.


Cloud foundation candidate 6ebeda55b22a3edba159c8f88f34f09784134fa8 passed complete
Windows/macOS/Linux CI run 41587053713, merged as d8b6c54239da741e64a706fa087fa0f9a0fa7509
with the identical tested tree. Original #10 root cause and physical #3 remain open.

## Action results, corpus evaluation and local preflight

Local fail-fast checks pass through tools/local_preflight.py: fmt, strict Clippy,
96 workspace tests, Debug build, managed probe and reconnect/pin-swap/stop smokes.
The local report explicitly leaves native desktop/GUI/Bluetooth/USB/DPI not_run;
no raw input collection, installation, OS permission change or upload is performed.
Action result tests cover unknown outcome/no replay, duplicate-ID conflicts,
remote/stale/expired/destructive denial, 128-entry bounds and excessive lifetimes.
Corpus tests keep independent labels and fail on bad input; the GUI CI smoke now
evaluates an explicitly saved canvas sample rather than only reading JSON.
Final native CI/GUI smoke for this candidate is a separate gate. No real OS action
backend, human corpus or full-product acceptance is claimed. See LOCAL_CODEX_HANDOFF.zh-CN.md
for current verified work, native wiring, and genuinely unimplemented later software.


Action/evaluator/preflight candidate d3875b8b8dfa473031bc396960fa868145eab32b passed
complete three-platform CI run 41589355999, merged as
7d79094cb9808b6fb9d434900bb656bec77b7800 with identical tested tree. Linux GUI smoke
actually exported its canvas sample and the offline evaluator reported the expected
negative-labelled false match. No human or native acceptance was inferred.

## P02 opt-in untrusted discovery

Local preflight (100 tests), strict Clippy, Debug build and real process smokes
passed. Allowed-expression/notice screening passes for 354 locked dependencies.
New source includes bounded IPv4 LAN hints, explicit public listener advertisement,
async error/stop handling and GUI address-only selection without authority change.
No-interface daemon lifecycle is tested without publication/query. macOS package
contains Bonjour/local-network purpose keys; actual OS prompts are unverified.
Cross-host multicast/NIC/firewall/sleep and daemon-cache flood acceptance remain
pending. Final native matrix is a separate gate; see DISCOVERY.md.

## Maintained font stack and advisory/notice regression

The eframe/egui 0.37 migration keeps the framework/rendering strategy and raises
GUI Rust minimum to 1.95. App logic continues worker/discovery polling independently
of visible UI. Headless tests explicitly consume unused texture deltas while keeping
rendered-shape and interaction assertions; actual Linux GUI/export smoke stays on.

A fixture reproduces omitted OFL/UFL/Hack/icons font notices when another license
filename was present. Collector now includes bundled fonts/*.txt; two Python tests
cover complete notices and SPDX AND/OR requirements. Native CI includes them.
A separate pinned cargo-audit job denies advisory warnings and uploads JSON. Final
candidate checks are recorded on its PR; an old local run is not a merge gate.
No native hardware, human corpus, #10 root-cause or full release acceptance claimed.

Restored upgrade worktree preflight `preflight-20261103T071701648399Z-3` passed
fmt, strict Clippy, all 100 Rust tests, Debug build and both process smokes. Python
notice tests: 2 passed. Resolved license screening: 355 package expressions; actual
0.37 font notices include OFL/UFL/Hack/icons. Local cargo-audit found 0 known
vulnerabilities and 0 warnings with advisory DB commit
`263a4d951a0867d4bddc29dddd6d59427c78f9a7` (updated 2026-11-02). Full final
cross-platform candidate validation and Linux rendering/export remain PR gates.

## D03 isolated storage foundation

`peerglide-transfer` implements bounded chunk manifests, source rechecks, disk-backed
out-of-order receiving, exact peer/transfer/manifest resume binding, rehash-on-resume,
non-overwriting full-hash commit and a crash terminal marker. Twelve local tests
pass (eleven portable + one Unix symlink test); no QUIC/GUI/drag/drop path exists.
These are actual temporary-file tests, not real hardware or network acceptance.
The marker prevents resume mutating a previously hard-linked/moved final inode.
Full workspace/candidate CI are separate gates; see TRANSFER_STORAGE.md for missing
quotas/grants/disk-failure/native filesystem acceptance and partial cleanup limits.


Font/audit final candidate 26377bce12a81d9ae4f846877092a717e4905448 passed all four
CI jobs in run 41595532009, merged as 759e61a1b9296c1f6c0b1bf50b529e2e6167947c
with identical tested tree. GUI screenshot artifact 13212840762 was inspected;
Home and saved rightward canvas render correctly, export/evaluator smoke passed.
Audit artifact 13212780715 reports 0 vulnerabilities/0 warnings with DB commit
263a4d951a0867d4bddc29dddd6d59427c78f9a7. This supersedes the pending-candidate
font checkpoint above; no hardware or historical #10 conclusion follows.

D03 local full preflight `preflight-20261103T072707329898Z-6` passed fmt, strict
Clippy, 112 Rust tests, Debug build and real process probe/reconnect smokes.
Windows excludes the Unix-only symlink test; final native candidate matrix is
still required before merge. No native input/file RPC has been added.

D03 initial candidate 2c05895c85e30c8e850d86c79148b7bc87eb6b9b failed Windows
strict Clippy in run 41596675300: the directory builder was mutable only under
cfg(unix). Replaced it with a cfg-specific builder expression; warnings remain
denied, no test disabled. Added a real Rust 1.95.0 all-target workspace check to CI
rather than inferring minimum-toolchain compatibility from metadata or stable.
Final replacement candidate must pass the complete matrix.

Local Rust 1.95.0 workspace/all-target locked check passed after the Windows cfg
fix. The local preflight now includes the Python notice regressions and license
screen; optional --audit requires an already installed cargo-audit and never installs
one. Its final candidate full checks remain separate from physical acceptance.

Final handoff hygiene: ignore generated Python bytecode, disable it for local
notice checks, and describe Cargo build/cache writes accurately in preflight help.
ROADMAP now reflects #27–#30 and the isolated D03 foundation, preserving all
physical gates and genuinely unimplemented product features.

## D01–D03 offline collaboration increment (2026-11-04, #32)

Adds independent session-bound per-direction data consent; bounded UTF-8 text
backend coordination; real-file admission/cancellation/history and late commit
permission checks; and an isolated bounded data framing codec. Seventeen new
regression tests cover pure policy/backend failures and real temporary-file
pipelines. The new codec is not negotiated or activated on input ALPN v5.

Development uses GitHub APIs/hosted CI only after identifying that the available
terminal belongs to the user Mac. No local build, installation, capture or OS
clipboard access was performed. Early candidate CI failed formatting; runner
diffs were applied. Final full matrix is pending at this checkpoint and must be
recorded from the exact candidate before calling this increment validated.

Automatic Codex code review could not run: repository review usage limit reached.
No credits/purchases/review-gate bypass were requested. CI does not replace that
review. Independent security review, native OS backends/network integration,
persistent/global disk accounting and real Win/Mac acceptance remain open.


## Local takeover / durable receive-root reservations (2026-11-04, WIP)

Verified main is 5d9476376b218567330cce0b5c74c9fb2e8c70a7 with no open PR.
Main run 41784077840 and #32 candidate run 41783406794 both succeeded; the latter
head is 7f0fd40890988da80e2d602e9d6a2dd2e00bf160. This does not close C06 or #10.

On Darwin 25.2.0 arm64, `python tools/local_preflight.py` cannot start because
`python` is unavailable. `python3 tools/local_preflight.py` records rustc/cargo
unavailable, fails starting fmt with missing cargo, and leaves subsequent checks
not_run. Report: target/local-validation/preflight-20261104T064341094380Z-34835.
No installation, native input, clipboard access or OS permission change occurred.
Local GUI, actual worker/probe/reconnect and physical input remain unverified.

New regressions exercise durable quota across cancelled/published/moved files,
legacy receipt import, exact resume without double charge, consent reset, lower
budget refusal, root-lock lifetime, failed staging, altered manifests and preserved
malformed/future/oversized records. Candidate tests/CI are pending at this checkpoint;
no new passing checks or persistent detailed-history claims are made yet.


PR #33 initial candidate 1bd0f759a232af40ed47f4659cf00beb941db627 passed the
Rust 1.95 all-target check in run 41788117975; three platform jobs stopped at
rustfmt, before Clippy/tests/build. Those are failures, not completed validation.
A focused independent read-only agent review found incomplete legacy storage,
cwd-dependent root paths and root-entry capacity boundaries. Each is being fixed
with filesystem/process regressions; this review is not an independent security
audit or an automated repository review-quota workaround. User explicitly declined
local Rust installation; continue source development and hosted CI only.


D03 durable-reservation code candidate `bc304939f06ba06920c0c7bdac739c8524752b98`
passed all five jobs in run [41788814220](https://github.com/ForceMind/PeerGlide/actions/runs/41788814220):
Windows/macOS/Linux fmt, strict Clippy, workspace tests, Release build, real process
probe/reconnect, notice checks and packages; isolated Linux GUI smoke; macOS
isolated/20-parallel GUI stress; Rust 1.95 all-target check and strict dependency
audit. Linux workspace has 140 tests (transfer crate 40, including 11 new groups;
the subprocess cwd helper and explicit corpus replay are additional invocations,
not extra unique tests). Windows omits Unix-only symlink regressions.

The focused independent static reviewer rechecked all three fixes without new
findings. Additional write/file-sync/directory-sync failure injection and physical
power-loss/media acceptance remain unrun. Final documentation follow-up changes
no Rust source, manifest, lockfile or workflow; its exact-head CI remains the PR
gate. No C06/#10/M4 product checkbox is closed. Persisted aggregate inventory and
cumulative admission quota are implemented; detailed persistent lifecycle history,
explicit cleanup/reclamation, free-space policy and network/UI remain future work.


## Explicit isolated GUI profile (2026-11-04, WIP)

Local main preflight on Darwin 25.2.0 arm64 cannot execute Cargo because Rust is
absent. The user explicitly declined installation; hosted CI remains the build
path. Existing macOS GUI always uses the user's Application Support directory,
so it cannot safely isolate native launch validation inside the approved workspace.

Add `--data-dir DIRECTORY` using the workspace's existing Clap dependency, retain
`--gesture-lab` and default profile behavior, and reject malformed/unknown options
before opening the window. No identity, trust, grant, discovery or input permission
is copied from another profile. A regression uses an isolated Unicode/space path
and preserves a future-version original profile while asserting independent
settings/default-deny permissions. Linux GUI smoke now passes and checks the
explicit directory; canvas drawing remains synthetic isolated-display evidence.

Candidate fmt/Clippy/workspace tests/build, native matrix and actual local launch
remain pending. No toolchain installation, native capture, system clipboard,
security-policy change or physical desktop acceptance is claimed.


Native Mac process baseline (no Rust installation): downloaded macOS artifact
13300483368 from main run 41784077840, package darwin-arm64, SHA256
`da9f5a55aabd5848616006284df2bea1d0175b4363d928ddb559ed1b130caac7` verified
against the bundled checksum. `file` reports a Mach-O ARM64 executable. The initial
restricted execution failed loopback bind with Operation not permitted; rerunning
with the authorized loopback capability passed both existing tools/smoke.py and
tools/reconnect_smoke.py: independent identities/authenticated QUIC/probe/clean
exit, managed worker stop, trusted reconnect after server loss/restart, pin-change
refusal and stop during backoff. No --native/--allow-control, discovery, clipboard,
user-profile access or security bypass was used. Local metadata report:
target/local-validation/baseline-main-5d947637/report.json. These results test main
5d947637's prebuilt worker, not a local source build or the new GUI candidate.

GUI candidate cb9d2be passed Rust 1.95 check, but platform jobs stopped at one
rustfmt difference in run 41789377283. Applied the exact emitted difference; final
candidate full matrix and local GUI launch remain pending.


Focused independent static review found the first Linux smoke used the same
explicit path as XDG's default, so it could miss a disconnected main argument.
The smoke now separates explicit-profile from default-data/peerglide, asserts the
explicit instance lock and refuses any fallback profile creation. No production
permission/isolation flaw was found by that review; execution CI remains required.


### Joint candidate / real Mac isolated launch evidence

Combined source candidate `c933ba2f32794223faff0fa2e912b97cbaeb1b4b` passed all
five jobs in [41790024756](https://github.com/ForceMind/PeerGlide/actions/runs/41790024756),
including strict workspace checks/tests, Release build/link, process smokes,
Linux explicit-profile GUI/export smoke, macOS GUI stress, Rust 1.95 and strict
audit. Storage-only final candidate `2d2bed5c42361f6c6ea940e2876a1fcad744648f`
also passed all five jobs in run 41789889924. Neither PR has been merged.

Downloaded exact-candidate macOS artifact 13302738707; verified inner ARM64 ZIP
SHA256 `aee4eac05dcdbeff7a322e78c40340a13e36fd2addf066ee3fc8a850aeb313b4`
against its bundled checksum. On macOS 26.2 (25C56), Darwin 25.2.0 arm64:

- Candidate `--help` reports `--data-dir` and `--gesture-lab` without opening a window.
- Both existing real-process smoke tools passed using this candidate's worker:
  mutually authenticated probe, clean/managed stop, reconnect, changed-pin refusal
  and cancellation during backoff. Loopback and disposable independent identities
  only; no native capture or injection.
- Actual candidate GUI launched with an absolute workspace-local --data-dir.
  Its window/screenshot showed rendered Home, Offline, Create an identity,
  No enrolled peer, and diagnostic native capture/injection off. Only this test
  app window was inspected; no screenshot was saved/uploaded to the repository.
- Closing the window ended the launch process with exit code 0. The profile
  contained only gui-instance.lock: no identity, peer certificate or settings.

One desktop-control getApp call stalled for about 51 minutes; subsequent scoped
window observations returned normally. This is a tool limitation, not measured
application startup latency, performance, resource or endurance acceptance.
Report: target/local-validation/gui-c933ba2/report.json. Rust was not installed;
source-build preflight remains blocked. Physical keys/buttons, Bluetooth/USB,
DPI, permissions, network-interface/sleep faults and two-machine C06 remain not_run.
No native clipboard, data network, global G1, or #10 root-cause claim is made.
The final documentation-only follow-up preserves this tested source tree; its
exact-head CI status is reported on PR #34.


### Simplified Chinese GUI correction (2026-11-04, implementing)

User requires the interface in Chinese. All current navigation, Home/Devices/
Displays/Transfers/Settings, offline lab/rules/discovery, lifecycle/permission/
configuration recovery labels now use Simplified Chinese. Stable configuration
keys, action IDs, protocol v5, exported JSON and user-supplied names remain intact.
Raw diagnostic details retain original text under a Chinese-labelled log view.

Embeds the unmodified Google Fonts Noto Sans SC variable TTF from pinned commit
a85815a42757630ce188fdad368c2dfc444d4773, verified Git blob
fb0637bafbcd804fe32152370a1225990745b4bc (17,772,300 bytes). Its original OFL-1.1
notice and provenance accompany every package; no system font loading/install is
enabled. A headless glyph-coverage check exercises both proportional/monospace
families, and page rendering uses the same bundled font. Python notice tests now
include the non-Cargo font; all three pass locally. Source build remains blocked
by the explicitly declined Rust installation. Earlier English GUI evidence does
not verify this new Chinese rendering; final candidate CI/screenshots/native launch
are pending. No physical input acceptance or global G1 enablement is claimed.


Chinese source candidate `d3891c6108590bc438dda9723abfa27944547342` passed all
five jobs in [41798545549](https://github.com/ForceMind/PeerGlide/actions/runs/41798545549):
three-platform fmt/strict Clippy/workspace tests/Release build/process smokes/
packages, Linux actual Chinese GUI/export smoke, macOS isolated/parallel GUI
stress, Rust 1.95 and strict audit. Downloaded and inspected Home/lab screenshots
from artifact 13306631744: Chinese navigation, headings, status, recognition and
saved-sample feedback render without missing-glyph boxes. Sample export/evaluator
and explicit-profile isolation checks passed. This is CI synthetic canvas evidence.

Exact-candidate macOS artifact 13306104386 has verified inner ZIP SHA256
`eb648b3e266ddfbff1a3dd56dfc84afcf39932efb922cdc652c413c09853be45`; the font OFL
notice is present. On macOS 26.2 (25C56), ARM64, its actual native window titled
“屏跃 · PeerGlide” displayed the Chinese Home/navigation correctly, Offline,
no identity/peer and native capture/injection off. The process is deliberately
left open for the user in a new workspace-local profile, containing only its
instance lock. No system font/toolchain install, input grant or user identity
change occurred. Report: target/local-validation/gui-zh-d3891c6/report.json.

The focused static review confirmed stable keys/protocol/JSON/action IDs are
unchanged, and corrected SampleCount's Chinese explanation to cover both too few
and too many samples. The font's variable axis is explicitly Regular (400), not
its upstream Thin default (100). Three Python notice regressions pass locally.
Final documentation/README follow-up changes no runtime/build/test source; its
exact-head candidate CI is reported on PR #34. Physical C06, global G1/actions,
production data network, persistent detailed history and #10 remain open.


## G1 native read-only context increment (2026-11-04, WIP)

Branch native-context follows ready/unmerged #33/#34. Main remains 5d947637;
#33 head 2d2bed5 and #34 head 69736f6 were rechecked open/unmerged. Local worktree
was clean before branching. User's no-Rust-install decision and Chinese GUI stay.

Adds real Windows/macOS foreground/focus providers, explicit diagnose-context
CLI without identity/network/input flags, and an on-click Chinese GUI process
permission snapshot. IDs follow the existing exact 256-byte rule contract; raw
window/control references stay private. Point-in-time consistency is explicitly
not a focus generation, execution ticket or action grant. Linux stays Unsupported.
See NATIVE_CONTEXT.md for OS APIs, privacy and remaining native worker gates.

Python syntax, notice tests (3) and whitespace checks passed locally. Native
compile/tests/diagnostic smoke, full final matrix and actual local diagnostic
are pending. No permission prompt, capture, injection, native context read on the
user Mac or toolchain installation has been performed by this new increment yet.
C06/G1 execution/#10 root cause remain open; no acceptance boxes closed.


### Native context candidate results

Initial 52e1a83 passed minimum Rust but failed formatting. One format-log EOF hunk
was initially missed and then applied; superseded intermediate 9d63f27 was cancelled.
Candidate 51997e7 reached strict Clippy and identified two actual integration
issues: CLI test-module placement, and conflicting const/mut CFRelease declarations
between old/new Mac adapters. Moved the tests to EOF and matched CFTypeRef's const
signature without changing ownership or ignoring lints. This is unrelated to #10.

Mac candidate 8f3847a passed its complete native job. Linux reported Unsupported
correctly, but the smoke omitted the corresponding unavailable recheck status;
fixed the harness to require unavailable focus/recheck on all unavailable foreground
results and retain native result codes only when a foreground exists. The output
field allowlist was strengthened, not replaced with a fake success or skipped test.

Final runtime/test candidate `31ae8be796fc32f5a3dd2f44974e525d88aa67b2` passed
all five jobs in [41806308409](https://github.com/ForceMind/PeerGlide/actions/runs/41806308409):
Windows/macOS/Linux fmt/strict Clippy/workspace tests/Release/process probe/reconnect/
read-only context smoke/packages; Linux GUI; macOS stress; Rust 1.95; strict audit.
The focused static reviewer rechecked NSString bounds, FFI ownership/signature and
CLI-module fixes without new findings; this is not an independent security audit.

Downloaded 8f3847a macOS artifact 13308579428 (complete Mac job run 41805129361),
verified inner ARM64 ZIP SHA256
`22538d688b2c2fa5c1ec36f8ffcb1d0939c4bd3f32f3946d3d1beaa6d79ff1d9`.
Its runtime/build sources match 31ae8be; only the corrected Python smoke differs.
On macOS 26.2 (25C56), the current smoke passed against that binary. Default
stdout omits identifiers; the explicit-ID variant was checked only in memory.
Foreground application information was available; focus/native successful access
was not accepted where current OS access was unavailable. Personal permission
values and application IDs are not published. Local metadata report remains
at target/local-validation/context-8f3847a/report.json and was not staged/uploaded.

The exact runtime's isolated GUI opened in Chinese. Settings → 检查当前系统权限
was actually clicked; Chinese statuses and GUI/worker/snapshot caveats rendered,
without enabling input/control, opening OS settings or requesting trust. Its
profile still contained only gui-instance.lock; no identity/certificate/settings
was created. The test window remains open for the user. No native Mac screenshot
or local report was uploaded. A suspected pre-existing dark-checkbox/native-theme
mismatch was observed separately and left for independent investigation; no theme
logic changed here.

Native focus success under granted/revoked permissions, transient away-and-back,
Windows real interactive desktop/UAC/session behavior, process/handle counts and
physical C06 remain unverified. Continuous notification/generation/token lifecycle,
Backend/owned action injection and local worker consent remain unimplemented;
query success enables none of them. Final follow-up updates docs/README and an obsolete rule-preview footer; native
provider, CLI and permission-button logic are unchanged. Its exact-head CI remains
the PR #35 gate. Main and prior PRs stay unmerged.

## Native invalidation / consuming context binding increment (WIP)

focus-invalidation branches from verified/unmerged #35 head 1a648f4. Adds actual
read-only Windows/macOS notification adapters and worker-owned context bindings.
The independent static review found a ready/epoch ABA; replaced separate readiness
reads with one versioned atomic state and added a controlled interleaving through
the actual mark clock path. Review then found no new deterministic defect.

Pure regressions cover coalesced away/back, trigger epoch/cache handoff, original
queue deadline, single consume/conflicting token, configuration/worker/close,
128-slot accounting including consumed native leases, expiry/clock/counter closure,
and unsupported Linux. Windows tests cover callback detach/poison, cached target,
session and notification classification. These do not simulate physical acceptance.

The explicit diagnose-context --observe-ms path starts no input capture and mints
no context, reports true unavailable/native errors, and treats cleanup failure as
failure. CI adds standalone CLI dependency-feature checking to avoid workspace GUI
feature unification masking native bindings. Python syntax/notice and whitespace
checks are local; Rust/native compilation, whole-workspace checks, real observer
smoke, final five-job candidate CI and local observer outcome are pending. No
system permission/toolchain change or new local native observation has been run.
Input v5 and global G1 remain disabled; C06 and #10 remain open.


Initial d4b6db2 passed minimum Rust but failed formatting; 6bbfcce still omitted
format-only changes in two parent modules. Those parent changes are included in
the next candidate. Windows lifecycle validation now uses injected cleanup calls
through the actual shutdown path: partial installations, each cleanup failure,
repeated shutdown, signal closure/TLS detach before native cleanup, and permanent
thread poison after failure. Fake handles never reach OS functions, even if a test
assertion fails. CleanupFailed is distinct from environment unavailable and is a
failing smoke outcome; constructor/pump cleanup failures preserve that result.


### Native observer/binding runtime candidate

Candidate `70bba447302d12c4168c583c5e1c01ef5f28114b` passed all five jobs in
[41818463792](https://github.com/ForceMind/PeerGlide/actions/runs/41818463792):
Windows/macOS/Linux format, standalone CLI feature check, strict Clippy, workspace
regressions, Release build, probe/reconnect/explicit observer smoke, GUI/packages,
minimum Rust 1.95 and strict audit. Initial strict lint on current stable rejected
deprecated fetch_update; replaced it with bounded CAS supported by the minimum
compiler, with no warning ignore or minimum-version increase.

Git transport twice failed commit_refs/ref updates. Commit objects were present;
a read-verified, expected-old-SHA, force=false GitHub ref API fast-forward restored
the exact local commits, followed by remote readback/fetch. No force push, history
rewrite or protected/main update occurred. Transfer failures are not code-check passes.

Downloaded Mac artifact 13315668039, verified inner ARM64 ZIP SHA256
`c42dc4556c3152c031d76c18e9d3e0930b034afbf99e616650c9549586e44d2c`.
On macOS 26.2 (25C56), local context_smoke passed against that runtime. Explicit
100ms observation returned a real access-unavailable rejection, with no minted
context, native capture or action support, and delivery_verified=false. This is
NOT successful native notification delivery or focus acceptance. Personal OS
permissions/IDs remain local at target/local-validation/focus-70bba44/report.json,
not staged or uploaded. No synthetic input, permission request/change or toolchain
installation was used.

Final follow-up adds explicit same-worker queued-context cancellation and its
resource test, plus records: cancelling while stale requires no native access,
releases only the matching full context, is idempotent and rejects foreign-worker
contexts. Observer adapters/diagnostic runtime remain the verified candidate; the
new cancel API/test still require final exact-head CI. Actual callback delivery,
Windows interactive suspend/session/fault recovery and granted/revoked AX paths
remain unverified. Independent static review is not full security audit. Global
G1, Backend/owned injection, shared G03 input wiring and C06 remain open.


## Local native window Backend (WIP)

local-window-actions branches from verified/unmerged #36/c6bc593 after rechecking
main5d947637 and a clean worktree. Adds a real peerglide-actions::execution::Backend
adapter using the existing Coordinator/native context bindings and independent
process-local default-off consent. Consent revision is fixed before context mark;
revocation/replacement cannot revive a queued trigger. Native effects are only
Windows min/max/close and macOS public AX min/close; unsupported semantics stay
unsupported, with no keys, focus override, process kill, input hook or network.

Injected tests cover policy/target/deadline/dedup/one-attempt and native preflight/
late guard/failure mapping, without operating any OS windows. Apple SDK review
confirmed AXFullScreen is not an established public attribute, so fullscreen is
not claimed or emulated with Zoom. Original deadlines and uncertain effects are
preserved; successful OS calls are Submitted, never app execution completion.

Rust/native compilation, final CI and further static review are pending. Local
Rust installation/OS permissions and global G1 remain unchanged; no actual window
action has been attempted on the user's Mac. Positive effects/AX access, C06 and
independent security review remain unverified. See LOCAL_WINDOW_ACTIONS.md.


Independent static review found that style/close-button identity alone did not
respect disabled native close controls. Windows now requires the borrowed system
menu's SC_CLOSE to be present and enabled; macOS requires a typed AXEnabled=true
and owning AXWindow match. Missing/unknown/disabled/query errors reject before
the late guard/effect; injected regressions assert zero effects. CF parameters are
prepared before the late guard, so no allocation/query is inserted after it.


### Native window action candidate results

Initial dfa96a2 passed minimum Rust but failed formatting; applied the exact
complete runner rustfmt patch including parent modules. Final runtime candidate
`86b4e7175e02df293bc642e7b643a860113fda75` passed all five jobs in
[41826266699](https://github.com/ForceMind/PeerGlide/actions/runs/41826266699):
Windows/macOS/Linux format, standalone CLI features, strict Clippy, workspace
regressions, Release builds, probe/reconnect/readonly observer smoke, GUI/packages,
minimum Rust 1.95 and strict audit. All declared native bindings compile/link.

Independent static review's disabled-close defect was fixed and rechecked:
Windows SC_CLOSE must be present/enabled, macOS AXEnabled typed true with matching
AXWindow. No later native query/allocation occurs between the final guard and
submission. Tests use injected drivers/WindowOps; no host or runner-native window
was minimized/maximized/closed for this increment. Successful native effects,
application semantics, modal/privilege boundaries and latency remain unverified.

Existing CLI/GUI binaries do not create/enable/dispatch LocalActions. Context
observer/privacy/transport baseline stayed intact in CI. Default-off consent,
trigger revision races/replacement/revocation/overflow, late original deadlines,
destructive/remote/stale/unsupported rejection, consumed-target one-attempt and
cached UnknownOutcome/no replay are automatically verified. These are logical
and native compilation results, not C06 or physical right-click acceptance.

Final docs-only follow-up preserves runtime/build/test source; its exact-head CI
is reported on PR #37. Global G1, keyboard owned injection, G03 input wiring/local
worker consent UI, native positive effects, C06 and #10 remain open. No local
Rust/system font/permission installation/change, actual action or stable release
was performed. Protocol v5/MIT and data permission separation remain unchanged.

### Native G03 short-path candidate (native-gesture-broker)

The optional Windows library path reuses the existing capture hooks, with no
product binary enable/dispatch. The existing KVM None path, readonly observer,
probe/reconnect and GUI checks remain required in the full five-job candidate CI.
The local preflight again reports missing Rust; the user's no-install decision
stands. Candidate format/Clippy/tests/build/MSRV/audit results are tracked on its
PR, including failed checks and fixes, rather than claimed as local runs.

New and retained nonphysical regressions cover ordinary/repeated/startup-held right pairs,
stale admission, held modifiers/buttons, injected classification, cancellation,
queue saturation, completed-but-unretired work, timeout/revoke/escape/source loss,
bounded shutdown debt, publication/claim interleavings and zero callback-path
heap allocations after construction. SPSC tests exercise non-Copy transfer on
real threads, endpoint closure, wraparound and exactly-once payload destruction.
OS file-lock tests use separate current-test-exe processes and isolated files,
prove explicit unlock with a clone still open, preserve rejected objects and
retain ownership through blocked TLS destruction. Injected unlock failure must
propagate through shutdown and keep that isolated child process poisoned. These
do not install hooks.

Windows actual hook delivery, silent-hook-removal behavior, callback latency,
DPI/driver semantics and all physical C06 items remain NOT RUN. macOS G1 rejects
by absence of this entry until physical provenance is established; CGEvent
PID/state is not used as proof. Existing macOS KVM behavior is unchanged. No
balanced click replay, recognition worker, UI consent or owned-injection ledger
is delivered here. Product G1 remains disabled; no host permissions or Rust were
installed, no real window action/capture or protocol-v5 change was attempted.

Candidate `feb3f7ac7371aef1265475e283f00f15ff9fcfd9` passed the complete five jobs
in [41836066218](https://github.com/ForceMind/PeerGlide/actions/runs/41836066218):
Windows/macOS/Linux format, standalone CLI features, strict workspace Clippy,
workspace tests, Release builds, probe/reconnect/readonly-context smoke, package
checks, Linux GUI runtime and macOS parallel GUI stress, minimum Rust 1.95 and
strict dependency audit. Linux ran 193 workspace tests; macOS ran 206. Windows ran 222 workspace tests. The native Windows adapter compiles and its mouse
message classifiers are tested. Injected-origin cases belong to the retained pure
G03 model; the unchanged native flag/tag filter was statically compared/reviewed,
not exercised through real hook delivery or host injection.

Earlier candidates failed formatting and the overflow fixture's redundant
constant expression. Both were fixed without disabling checks. Targeted
independent static review found and rechecked publication/claim ABA, shutdown
unlock-failure propagation and original-expiry conversion; deterministic
interleavings, isolated-child failure injection and delayed-clock regressions
cover them. This is a focused code review, not a complete security audit.

Final follow-up only records results, preserving all tested runtime/test source;
exact final-head CI and readiness are recorded on PR #38. No host capture/native
action, permission change or Rust install occurred. Physical/native delivery and
all previously listed G1/C06/#10 limitations remain open.

### Sealed local click recovery (owned-local-input candidate)

A real Windows library recovery path now consumes a native End receipt and its
original target lease, performs physical-point/window-DPI/routing and late consent
checks, and submits a separately tagged Down/Up in one batch. The existing hook
records Allowed/Blocked receipts; partial count is never assumed to be a prefix.
Bounded cleanup only concerns a proven missing owned Up. See
[LOCAL_CLICK_RECOVERY.md](LOCAL_CLICK_RECOVERY.md) for failure/unknown semantics,
foreign-input rules and the unavoidable silent-hook-removal observation limit.

Synthetic protocol/capture/native-query/INPUT-construction regressions and full
candidate CI are required. Local Rust is still absent by user choice; no actual
input injection, host native permission change or window manipulation was run.
Keyboard and macOS recovery, worker/recognizer/rules dispatch and explicit enable
UI remain unfinished. G1 product entry stays closed, all C06 boxes stay unchecked,
and #10 historical root cause is still unknown. Green CI does not change these.

Recovery runtime/test candidate `ceecaa96e9b87c7dcf088bd5cfa192a0b9ec0ff9` passed
all five jobs in [41849905620](https://github.com/ForceMind/PeerGlide/actions/runs/41849905620):
three-platform format, standalone CLI features, strict workspace Clippy,
workspace tests (Linux 208/macOS 221/Windows 243), Release builds, probe/reconnect/
readonly context smoke, notices and packaging, Linux GUI runtime, macOS parallel
GUI stress, minimum Rust 1.95 and strict dependency audit.

Actual CI failures were fixed: full formatter output, the Windows IsWindowEnabled
binding namespace, and the final repeated-down geometry fixture format. Review
found and rechecked slot initialization/finish reuse races, callback-point/foreign
motion protection, and queued owned Up after a foreign right ownership conflict.
Interleaving regressions preserve pending/Unknown rather than claiming cleanup.
A zero-insertion cleanup retry resets its in-flight result before exposure, and
repeated native Down positions contribute to the sealed displacement envelope.
Two superseded runs were explicitly cancelled to release queued Mac resources;
no final-candidate check was skipped or relaxed.

This focused independent static review is not a complete security audit. Every
new input/effect test remains synthetic/query-seam/INPUT construction; no actual
host/runner injection, permission change or Rust install occurred. The adapter
cannot prove continuity after silent hook removal or completion beyond its hook.
All native/physical/worker/macOS/keyboard gates listed above remain unfinished.
Final docs-only head preserves runtime/tests; its exact CI/readiness is on PR #39.

### Actual Chinese GUI migration (0.2.0-dev, candidate)

The actual eframe GUI now uses the approved five-page design and real enrollment,
communication/probe, primary-screen direction, independent receiving preference,
readonly permission diagnostics, rules/canvas/config recovery and stop flows.
Unimplemented global G1/data/pairing-code/tray features stay explicitly unavailable;
no demo devices, files, grants or success states are used in the running GUI.

New local input IPC is independent of connection/authentication, with a final
terminal cleanup observation after the supervisor has ended. GUI drains final
stdout without read errors, rejects cached nonterminal Local/Disabled as proof,
and never auto-restarts Native from unknown cleanup. Platform restore_checked
propagates pointer failures while still attempting thread shutdown. Managed stop
cooperatively awaits session cleanup rather than dropping its future.

Candidate checks must cover schema/terminal/sticky failure, actual command flags,
cooperative real-loopback stop without native input, full desktop/dialog render,
no render-only writes, future/invalid rules preservation, explicit sample export
and real widget geometry for isolated Xvfb. Full matrix is pending, not local
Rust success. No host input/clipboard/file sharing or OS permission change has
been run. Physical C06, native app semantics and #10 root cause remain unverified.


The runtime candidate c0a188277d9646c5889727f19490030ab684347b passed all five
jobs in [run 41883855509](https://github.com/ForceMind/PeerGlide/actions/runs/41883855509).
This includes strict checks/tests/Release/process smokes on all three platforms,
macOS repeated parallel GUI regressions, Linux Xvfb actual drawing/explicit export,
Rust 1.95 and the strict dependency audit. Early fmt/test API/Clippy failures were
fixed; the process smoke now separates input records while preserving the exact
connection sequence and asserting Disabled through the final terminal receipt.

The SHA-verified macOS artifact also ran on this host with an explicit isolated
profile. Actual Chinese navigation, disabled rule save, pasted Chinese peer name
and certificate path, identity/enrollment, default-off receiving preference,
loopback managed communication, stop, another probe, persisted layout direction,
light/dark settings, readonly permission checks and window-close cleanup passed.
GUI/worker/loopback peer exit codes were 0; actual managed child arguments omitted
native/receiving flags. No host capture/injection/action/data sharing was started.
AX/Input Monitoring were denied; no grants changed. Canvas drawing is proven by
isolated Xvfb, not host right-button sampling or Chinese IME acceptance.

The first sandboxed host launch exited 134 without logs; a WindowServer-accessible
launch succeeded. This is not a diagnosis of historical #10. CUA App observation
can relaunch a closed app; that default window was closed without editing it.
The isolated process exit codes, rather than a subsequent App query, prove cleanup.
See GUI_INTERACTIONS.zh-CN.md for details. This final documentation follow-up leaves
runtime/tests/tools/workflow sources unchanged; exact-head CI is read back on #42.
Physical C06, global G1, native effects and production data sharing remain pending.


### 基础Win/Mac KVM收敛，0.2.1-dev（2026-11-05）

用户指定Windows + macOS、每台一个主显示器；本版优先基础输入，G1/数据后置。
main重新核对仍5d947637，以下增量未合并，不发布稳定版。本机cargo/rustc仍缺失，
按用户决定不安装；local_preflight失败于工具缺失，不能写成本机构建通过。

PR43 / 49379f1c695777c97b7229b5cd4f437d2932dedd通过完整五项
[CI41959403280](https://github.com/ForceMind/PeerGlide/actions/runs/41959403280)：
正常Up在定位/注入成功前保留归属，未知Up不定位/注入；接管事件不立即反向Acquire，
hand-over/恢复后先interior再跨边。key/button定位/Up失败和不重放回归通过。

PR44源码5cedae946bb36e83300948224c4c698cd1232069通过完整五项
[CI41964917548](https://github.com/ForceMind/PeerGlide/actions/runs/41964917548)：
三平台fmt、独立CLI、严格Clippy、工作区测试、Release、真实无原生输入probe/重连/
只读上下文、Linux实际GUI、Mac多轮GUI、最低Rust1.95与审计全部通过。
新增捕获域/epoch、固定pair、满队列/禁用/stop归属、原thread等Up、异步drain及
source-lost回归均为软件证据，不是物理键鼠或OS效果证据。Readonly窄审发现新增
incoming fence退休后Ping可续租的问题；已在每轮select前协调释放，回归确认不再
续活，cleanup失败仍保留owned Up并报告Unknown。审查也撤销了“send无界”误报，
因为原session本已有250ms timeout。Rust stable99的deprecated fetch_update已用
兼容1.95的CAS替代；fmt/Clippy实际失败已修，没有豁免或删断言。

最后文档跟进不改源码/测试/tools/workflow；精确文档head CI回读PR44。Phase2的
Receiving首物理事件保护、独立Native loop串行接收/恢复仍在实现；仅域隔离不能
防止先放行本地C再清远程Ctrl，也不能作为整版已可日常工作。后续软件候选需要
匹配的Win/Mac包及同样完整检查，再由真实两端完成C06。当前未开启宿主捕获/注入、
未改系统权限，双机、DPI、物理接管、拖拽、睡眠、权限变化均未运行；#10未定位。


### 基础接收闭环最终软件候选，0.2.1-dev（2026-11-05）

PR45运行候选746b07e1022c2939627f09ff048cca89955e7dd6完整五项
[CI41974906260](https://github.com/ForceMind/PeerGlide/actions/runs/41974906260)全部
通过。三平台除fmt/独立CLI/严格Clippy/workspace test/Release外，新增普通
cargo build --workspace --locked（Debug）也通过；真实无原生输入probe/reconnect/
只读context、Linux实际GUI与显式样本导出、Mac多轮GUI、最低Rust1.95和审计均通过。
Engine20项fake、Parent配对/源健康/滚轮/新鲜FAILED逃生等软件回归已实际运行。
最终检查点无跳过断言或warnings豁免；Linux测试只移除未使用的Native别名导出，
Engine实现及其Linux fake测试仍编译执行。

两平台artifact绑定同一746b07e，外层/内层ZIP的SHA-256已核对，安全路径/无symlink/
无用户key/profile、MIT、Mac中文plist与0.2.1版本、Win双PE程序均已检查。候选为
未签名开发包，Windows amd64、Mac arm64，不是稳定发布；不得关闭OS防护打开。
Mac同包在本机执行smoke/reconnect/context脚本全过：真实独立进程和身份、认证
QUIC probe、pin替换拒绝、cooperative stop和可取消backoff，均未开启原生输入。

Mac GUI用原0.2.0隔离测试profile实际启动，显示0.2.1中文；既有peer可读而不会
自启Native。键鼠确认默认未勾选、确认按钮禁用，新首次物理动作只接管不回放的
说明可见；取消后检查GUI权限为AX/输入监控未授予、SecureInput未检测到。没有
申请/更改权限。关闭窗口后exit0，无task GUI/worker残留；原测试identity两个
文件字节哈希与启动前一致，native/allow仍false。退出后不再查询App以免CUA
自动重开默认窗口。宿主截图/配置/密钥不提交，样本与报告留ignored target。

| 分类 | 本版状态 |
|---|---|
| 已实现并自动验证 | Core松键事务、capture域/配对、Receiving/Native单owner、独立watchdog/恢复、取消/截止、源观测、中文GUI与无Native进程闭环 |
| 已实现但原生未验收 | 真实Win↔Mac输入、首动作Consumed实际效果、混合DPI、拖拽/滚轮、物理接管、权限/睡眠/故障与SDK请求到OS/app效果 |
| 尚未实现 | G1全局、生产剪贴板/文件、配对码、托盘、多显示器精细拓扑；本版不扩 |
| 客观受阻 | 当前只有本机Mac可操作，AX/输入监控未授予，Windows双机受控环境与用户手动原生权限/操作待提供 |

用户已确认Win+Mac、各一个主显示器。匹配包与步骤已准备，已请求实际双机试跑
可用性；C06所有物理框仍未关闭，不能因此说已可稳定日常工作。最后文档跟进不改
apps/crates/tools/workflow/Cargo源码；精确最终head平台CI与PR就绪以PR45回读为准。
#10历史根因未确认，针对性只读复核不是完整独立安全审计，不购买审查额度。


### 公开连接信息登记便利增量，0.2.2-dev（2026-11-05，待CI）

本批不改KVM原生输入，仅新增GUI公开连接信息复制/粘贴与受限编解码；移除新配置
示例IP预填，保留既有身份/设置/高级文件登记。原生双机C06仍未验收。读取不落盘或
授信、全文修改失效、自身/未来/超大/无效信息拒绝、返回角色、多网卡选择及双端独立
确认后登记的回归已写，尚未编译。local_preflight于d8557c5因cargo/rustc unavailable
停于fmt；没有安装工具链，不代表代码fmt失败。待精确提交五项CI与对应产物验证。
浏览器未验证；本机新GUI交互和Windows实机未运行。短码/一键配对尚未实现。


运行源码7d3e035be3784c4b1cedda1cec1c4ca1b9a5677d已通过完整五项
[CI 41987328608](https://github.com/ForceMind/PeerGlide/actions/runs/41987328608)：
Windows/macOS/Linux fmt、严格Clippy、锁定Debug/Release构建与工作区测试、无原生输入
probe/reconnect/context、GUI smoke/打包、最低Rust1.95和依赖审计。新增8条受限材料
回归、3条GUI登记状态回归、1条长文本小窗口高度回归实际运行通过。输入协议仍v5，
CLI/平台原生源码没有变化。PR46叠加未合并PR45，main未合并，不是稳定版。

macOS包artifact 13396158475外层/内层摘要与安全路径核对通过，无用户身份/配置。
宿主隔离目录实际启动0.2.2，复制公开信息获得网卡地址；复制后粘贴本机信息被拒绝；
粘贴独立测试对端信息只生成草稿、多网卡不预选、完整指纹与该测试对端CLI独立输出
一致。首包f7ff780发现TextEdit随全文长高，新增滚动限高后7d3e035宿主复验按钮保持
可见。独立CLI仅绑定回环随机端口且native/allow为false，managed停止exit0。
实际双机试跑身份未修改，旧0.2.1试跑GUI及首个0.2.2隔离GUI均正常exit0。

宿主锁屏后无法继续GUI，未执行最后选择/勾选/登记，隔离测试窗口留在未登记草稿；
此最后流程由自动回归覆盖，不写作宿主GUI验收通过。不绕过锁屏，不请求系统权限，
未创建真实输入会话。浏览器未验证，Win↔Mac物理输入C06、实际跨机连接仍未验收。
仅有针对性只读复核，不是完整独立安全审计；#10历史根因仍未确认。


### GUI空闲6.83GB事故（2026-11-05，诊断中）

用户报告macOS屏跃6.83GB，状态为只打开未操作。当前进程已退出143且无残留；没有
退出前的进程采样，不能判定Rust堆/纹理/驱动或压缩内存的具体责任。0.2.2候选五项CI
只含短时GUI，不是长驻内存证明，暂缓继续双机试跑。旧0.2.0启动ips与本事故不等同。

静态依赖检查未找到确定根因；常规FullOutput、文本缓存、font atlas和重绘窗口队列有
替换/回收路径，不能直接认定100ms重绘意味着泄漏。新增受内存阈值保护的隔离CI驻留
诊断待运行，两个profile均无native/权限/真实连接。512MiB绝对RSS及64MiB预热后增长
是诊断中止阈值，不是已达成的产品预算；RSS不能完整反映压缩/GPU内存。未在用户Mac
重启复现，不把新检查或历史CI当修复成功，不关闭C06或#10。


首轮运行结果：b8d490f普通五项[CI42017317157](https://github.com/ForceMind/PeerGlide/actions/runs/42017317157)
及隔离[驻留CI42017317320](https://github.com/ForceMind/PeerGlide/actions/runs/42017317320)通过。
两场景各180秒：Mac空身份/已有身份峰RSS116320/120512KiB、增长0/3904KiB；Linux
峰134636/136360KiB、增长0/0。Mac CI系统26.6.2/25G83与宿主26.2/25C56不同；测试
只限RSS，仍未解释用户6.83GB。12条初始脚本回归实际在CI通过，未运行宿主新GUI。

随后新增17条完整采样器回归（含vmmap解析/缺测/物理阈值）本地通过，只用fixture与
小Python child。Darwin physical footprint采用同512/64MiB诊断cap，不以缺测零值或
RSS成功冒充全内存验证。此增量尚待对应CI，应用源码仍与0.2.2相同，不声称修复。


### 真实 Windows ↔ macOS 无原生输入通信通过（2026-11-05）

用户授权恢复连接试跑，Mac改用已有0.2.2-dev CLI，不重新启动存在内存事故的GUI。
两端使用7d3e035/CI41987328608对应分发源码。用户通过可信聊天交换公开连接信息；
Mac实际CLI解析Windows公钥并与此前独立提供的完整BLAKE3指纹逐字一致，本机原身份
也保持一致。未交换私钥；候选材料/真实地址/完整日志仅保存在ignored本机试跑目录，
不提交到仓库或Issue。没有修改代理、路由、防火墙或系统安全策略。

真实Mac连接用户Windows监听端，mutual TLS、v5协议协商与可靠Ping/Pong往返均通过，
事件顺序Authenticated → ProbePassed → Disabled(terminal) → Stopped。实际进程退出0，
约0.63秒，采样峰RSS10688KiB；外部guard为15秒/128MiB，仅作用于本次自有CLI子进程。
native=false、allow_control=false，未启动全局捕获或注入。此为真实两机网络通过，
不是回环或CI模拟；探针结束后已断开，不表示持续会话或键鼠控制已开启。

实际Mac worker随后只读diagnose-context：Accessibility/Input Monitoring均denied，
Secure Input inactive，native_capture_started=false。下一步原生输入依赖用户明确手动
处理权限，当前未运行四边/键盘/拖拽/滚轮/DPI/物理接管/断网/睡眠等C06项目。CLI
低RSS不证明GUI6.83GB事故已修复，该事故仍开放；#10历史根因也不作结论。


### Mac授权归属与正常应用启动复查（2026-11-05）

用户已在系统设置为屏跃添加辅助功能和输入监控。直接由自动化宿主启动的CLI仍报
Denied；只读TCC日志确认该调用的responsible为宿主，而非屏跃。因此不能把这个结果
解释为用户未授权，不要求用户扩大宿主权限，也不重置TCC。通过正常应用启动路径，
屏跃GUI自身已显示两项Granted、Secure Input未检测到；这仍不是实际Native worker
自检或物理输入通过，后者待本次明确会话同意及Windows接收端准备。

正常应用首次启动被测试profile所在Documents目录访问提示阻塞，未接受该额外授权。
使用0700私有临时目录保存同身份本机试跑副本，各文件0600，原资料保持不变；Windows
公钥仍为先前核验的实际pin，接收和native偏好均false。没有把私钥复制到其他电脑或
公开日志，没有改变应用代码/签名或系统防护。临时路径只在本机ignored状态指针中。

有界GUI检查采用256MiB绝对RSS、10秒启动预热后64MiB增长与3分钟时限；本次采样峰
118352KiB、无Native child，到期停止。启动前的低RSS不得作为增长基线。时限结束后
CUA的getApp可能重开默认配置窗口；已关闭该窗口，没有创建/替换身份或修改其配置。
短时低RSS不证明此前6.83GB事故已修复。下一步仅计划Mac→Windows短时物理试跑，
Mac接收控制仍关闭，等待用户明确开始；C06清单保持未验收。


### 首次真实Native尝试：worker授权失败，0.2.3修复候选（2026-11-05）

用户明确同意本次仅Mac→Windows短时试跑并在Windows启用Native/接收许可。Mac通过
正常应用启动、同身份私有profile、明确Native确认后spawn实际worker；参数native=true、
allow_control=false。双端认证成功，但Mac worker在AXIsProcessTrusted前置检查失败，
exit1；未进入原生捕获，没有物理鼠标/键盘验收。GUI与worker随后均结束，无强杀活跃
Native。只记录状态，不收集/公开真实按键或用户内容，C06仍不勾选。

只读TCC记录：GUI自身按bundleID授权允许，helper的responsible虽为GUI，但subject落到
GUI可执行文件路径并拒绝。原包仅linker ad-hoc，main identifier由链接器生成，Info.plist
not bound且无资源seal。这些是明确差异，完整app签名是修复候选，尚非已验证根因结论。

0.2.3-dev候选仅修打包身份/实际worker只读诊断，不改v5/输入路径/设备证书。7条打包
边界测试通过；另在本机新临时副本真实执行helper→app ad-hoc签名/严格验证/metadata
检查通过，原artifact未变，没有GUI/TCC/系统权限操作。CI将额外解压ZIP验证并要求
修改plist/helper的独立副本拒绝通过。新GUI诊断的格式/模式/输出界限/进程截止回归待CI。
新版仍无DeveloperID/公证，默认DR不放宽；旧授权不保证沿用。真实新worker授权、
物理C06及GUI6.83GB事故均未解决/未验收，不把签名或CI等同原生成功。


运行候选c9307aa44b79f21298d4eb48ad0afb9a7f263811通过完整五项
[CI42039285409](https://github.com/ForceMind/PeerGlide/actions/runs/42039285409)及两平台
[驻留诊断42039285155](https://github.com/ForceMind/PeerGlide/actions/runs/42039285155)。
新增worker诊断5条回归（Unix含真实子进程超时/关闭回收；Windows为3条纯/命令回归）
与7条打包回归实际通过，Mac解压签名复验和plist/helper篡改拒绝通过。针对性复核发现
的诊断线程退出孤儿风险已用cancel+join及PID消失回归修正；测试等待可解析PID而非
仅文件存在，避免空文件竞态。该复核不是完整安全审计。

Mac artifact13419264044外层sha256为f0bb70138528893d1f847903ffa0ecc20ae7ce96821e23dbf0417a08f3f19221，
内层摘要、安全解压路径、无用户身份/配置及本机codesign strict/metadata复验已过：
main identifier=com.peerglide.PeerGlide、Info.plist entries=11、资源seal version2；helper
作为嵌套代码密封。此本机检查只验证完整性，不等于DeveloperID、公证或TCC授权。

同身份私有profile曾保存用户前次明确选择的native偏好true，但allow=false；普通启动
不会因此开启Native，诊断命令也不读取该偏好来授权。本次新候选只读启动遇到宿主锁屏，
未点击实际worker诊断；监测到3分钟结束停止空闲GUI，采样峰RSS126400KiB，无Native。
等待用户手动解锁后继续实际helper/正常App责任链复查，旧授权不保证沿用。现阶段未
完成物理输入，签名是候选修复，GUI6.83GB事故仍未定位；Windows旧会话可停止保留配对。


### Oink完整参考与按活动刷新候选，0.2.4-dev（2026-11-05，待CI）

Oink固定3ca82127源码的15个Rust crate、UI、FileProvider/音频插件、构建/测试入口已建立
模块覆盖索引并核对关键状态链，见REFERENCE_RESEARCH。用户确认作者许可原创代码按MIT
集成分发，第三方许可另计。本批没有移入Oink源码/资源/依赖，没有执行Oink或浏览器测试；
参考不等于逐行安全审计或其CI/实机验收，也不把其full-trust默认共享/无界总线等迁入PG。

独立实现PG现有GUI活动节拍：无异步工作/画布deadline时去掉应用自身固定10Hz重绘；
会话、停止/重连、发现、worker诊断、画布录制和显式layout-report仍有100ms轮询；UI操作
晚于logic启动新任务也安排首tick。画布在遮挡后取消。关闭拦截移入logic，使隐藏窗口
仍能CancelClose→合作停止→等待清理再Close，未确认恢复保留警告。非Native进程退出也
等stdout EOF并消费最后probe状态，不依赖下一次无条件UI重绘。原生watchdog/lease/回调/
协议v5完全不变。新增6条资源/生命周期回归已写，尚未编译；本机preflight因Cargo缺失
停止，保持不安装Rust。此为空闲资源收敛，不是6.83GB根因已证实或修复。

0.2.3-c9307aa在用户解锁后已实际检查新UI的“实际工作进程权限”：报告可读取，GUI与
worker均Denied，Secure Input inactive；没有申请权限或启动Native。新版代码身份变动
需要用户对最后候选明确授权后再验，当前不反复让用户操作中间包。GUI只读检查后结束，
既有设备身份/配对与Mac接收关闭状态保留。实际worker授权传递与物理C06仍待验。


#### 0.2.4运行候选结果（2026-11-06）

fd8c071084afd19047df7354a6eb32b07b278dc6已通过完整五项
[CI42058770247](https://github.com/ForceMind/PeerGlide/actions/runs/42058770247)：三平台fmt/
严格Clippy/locked测试和Debug+Release、进程probe/reconnect/context、GUI smoke/打包、
最低Rust1.95、依赖审计、Mac解压签名与篡改拒绝。新增6条软件资源/生命周期回归通过。
[驻留42058770207](https://github.com/ForceMind/PeerGlide/actions/runs/42058770207)的Mac任务
排队中取消后仅重跑未执行项，最终两平台任务均success；这不是取消失败来制造绿色。

Mac两场景各约181秒：RSS峰116368/116784KiB，预热后增长0/112KiB；所取physical
footprint峰40141/38912KiB，工具reported peak均48128KiB。第一场景partial，第二available，
整体physical检查仍null。不可把success解释为全部GPU/压缩内存验证或长驻事故修复。

Mac artifact13431955670外层sha256=a22a963780f4c56a6c5ad990f6ef6827aa81b81c071dbfe63041102ecd180336，
内层摘要、安全解压/无用户身份配置、本机codesign strict和metadata均已验证；Windows
匹配artifact13430926303已由CI产出，本机不运行Windows程序。运行候选保留在ignored
idle-activity-fd8c071，最终记录更新不改变受测代码，也不要求为纯文档更换试跑二进制。

另只读核对0.2.3实际诊断worker的TCC记录：responsible和subject已统一为应用ID，
不再是旧GUI二进制路径式查询；authValue为未授予状态，与新ad-hoc代码身份需用户确认
相符。未改OS授权/重置TCC，仍需最终候选真正获许可后再测试Native。没有物理C06通过，
没有新部署/稳定发布，没有把Oink源码测试当PG验证或把CI当独立安全审查。

#### 最终候选用户授权后的只读复查（2026-11-06）

用户回复最新版两项已开启后，通过正常应用入口启动同一fd8c071完整包，显式沿用已有
私有试跑profile/设备身份；没有启动Native、通信或发现，也没有修改系统权限。实际
worker只读快照和GUI自身检查均报告辅助功能/输入监控未授予，Secure Input未检测到。
本次worker的TCC responsible/subject仍为应用ID，不能把GUI签名完整性当作OS许可。

随后只读检查系统设置：两项列表中的PeerGlide均为on，但各自“在访达中显示”都选中
旧0.1.0包（8f3847a候选），并非本次0.2.4包。这是已观察到的应用副本关联差异；尚未
读取系统授权记录中的签名条件，不能宣称完整拒绝根因已证实或重加后必然可用。
已在访达选中实际0.2.4候选，集中请用户仅移除这两项旧屏跃授权条目、重新添加该候选
并开启；不删除应用/身份/配对，不重置TCC、不扩大宿主权限，等待用户手动确认。

本次只读GUI运行约66.94秒，采样RSS峰116288KiB；通过窗口关闭正常结束，监测回读
无候选GUI/worker残留。短时RSS不覆盖GPU/压缩内存，也不证明6.83GB事故已修复。
真实Native与所有物理C06项目仍未验收，本机Rust继续不安装。

另回读上一文档head f148116的自动CI：完整五项
[42065590838](https://github.com/ForceMind/PeerGlide/actions/runs/42065590838)与两平台驻留
[42065590740](https://github.com/ForceMind/PeerGlide/actions/runs/42065590740)均success。
应用/工具/依赖/CI源码与fd8c071差异为空；仍保留同一运行包，不因本次记录更换代码身份。

#### 移除旧条目并重新登记后的实际worker结果（2026-11-06）

用户回复“两项已重新添加”后，正常入口启动同一fd8c071完整0.2.4包与原私有试跑
profile。“设置→检查实际工作进程权限”实际返回：辅助功能Granted、输入监控Granted、
输入桌面无需同名权限，未检测到Secure Input。此次重新登记后实际worker许可检查
已通过；没有另行用GUI自身快照替代worker，也不证明未来构建无需再次确认OS许可。

本轮未启Native、通信或发现，用户随后要求完整Oink整合方案，因此先结束只读检查。
通过窗口关闭正常退出；监测约154.71秒、RSS峰115632KiB，退出回读无候选GUI/worker
残留，没有强杀活跃Native。没有观察到真实输入效果，C06所有物理项目仍未验收，
6.83GB内存事故根因仍未知。本批仅补实测记录，应用、签名、身份与配对均未改变。


### 0.3.0-dev设备工作区与中文交互（2026-11-06，待CI）

用户确认完整整合方案，并明确参考Oink后融合现有中文界面。分支device-workspace从
f348235推进B1，root负责状态/页面/交互与集成，复用localize_panels负责单一视觉模块并
窄复核，不新增输入钩子/协议/依赖或执行授权。浅深主题、设备卡与响应式主屏示意接
原有实际worker，主操作仍需新确认；工程细节下沉但恢复/撤销/失败和停止保留。

本机preflight-20261106T012131047029Z-81187明确rustc/cargo不可用，fmt未能执行，
其余Rust检查未运行；保持用户不安装工具链的决定。新增呈现/确认/拖动边界回归及
既有生命周期/真实进程/GUI smokes由精确候选CI执行，当前不声称编译或验收通过。
本批git diff --check已过，尚未浏览器验证或运行新包，没有开启Native或更改系统权限。
前版0.2.4的实际worker权限已过，但不能推导0.3.0包授权自动沿用或物理C06通过。


本批aeea772已通过完整五项CI42147723479（最终状态回读待附），其中首次egui主题
接口不兼容已按0.37官方源码修正，测试场景缺失已登记元数据已补齐，未降低断言。
Mac artifact13471755307下载后外层SHA256=fe51cb23fdab6d8a97438eec654026244a31fd362bf3550a9ef75c048b907fb5，
内层a029e3bdf626baf94e898f4668654a2a8128f1d58b732c15c3faa63b3d1374c3，本机安全解压、
无用户配置/密钥、strict签名与bundle metadata复验均过。下载未结束的中途摘要检查
未通过，完整下载结束后重新核验正确，不能将部分下载解释为包被篡改。

该包在本机正常入口/新隔离随机身份/仅复制公开信任材料的profile实际打开：无Native/
网络/系统授权操作，92.86秒/RSS峰114944KiB，正常关闭且无残留。实际观察发现主屏
工作区把开启按钮挤出首屏；这是CI未发现的真实可用性问题，不能以绿灯直接交付。
已修改为紧凑设备卡、主操作放图形上方、水平布局减少空高，并新增浅深主题/920×700
与1180×820/总览及设备页的主按钮完整绘制且不被裁剪回归。后续运行候选仍待CI及
本机复查，aeea运行包保留供来源核对，不作为修正后GUI的验收证据。浏览器未验证。


#### 0.3.0 B1冻结候选与补充指针回归（2026-11-06）

运行源码0eb687a65554470893c7a16befa6bfcca57ebe00通过完整五项
[CI42152464452](https://github.com/ForceMind/PeerGlide/actions/runs/42152464452)及两平台
[驻留42152464382](https://github.com/ForceMind/PeerGlide/actions/runs/42152464382)。包含三平台
fmt/严格Clippy/locked测试/Debug+Release、真实无原生输入进程probe/reconnect/context、
GUI smoke、最低Rust1.95、审计、Mac完整签名/ZIP解压与篡改拒绝。首屏主按钮完整
绘制回归覆盖浅深主题、920×700及1180×820、总览和设备页。

Mac artifact13473062882：外层SHA256=c5b07672c86b925f968c65490200a0fc3d4182dcb36ff7ac959b7ece76f2ac7e，
内层fdb1d09698e890c54db6fefb278fb4af0dbdf020b966a0bb4cbb97a66c698237。本机安全内容、
摘要、strict签名、bundle metadata和CLI0.3.0版本均核对。Windows匹配包13473756220由
CI构建，外层API摘要4eb5d24b17fffbd8988b9218fb049cf84c942daa6e33769a0ca2a77d267caddf；
本机未运行Windows包。产物及实际CI截图保留在ignored workspace-0eb687a。

Mac驻留两场景各约181秒，RSS峰116944/116992KiB，预热后增长992/0KiB；physical峰
40039/39322KiB，首场景partial、第二available，整体physical检查null。success只说明
已覆盖的短时指标，不是完整GPU/压缩内存或6.83GB根因修复。两个平台任务均success。

本机macOS26.2/25C56 arm64实际运行同一冻结包，在新隔离随机身份下只复制公开信任
材料，不复制原私钥，不联网/开启Native/更改OS权限。总览与设备页主操作首屏可达，
浅深外观、设置、未勾选的开启确认/禁用确认按钮与关闭确认窗均实际观察到。三次有界
检查由既定保护结束空闲GUI，峰RSS100592/117440/115712KiB，均无候选GUI/worker残留。
原试跑profile的settings与公开证书摘要前后相同，现有配对没有改写。

宿主CUA拖动未改变方向，抬前窗口及方向点击也未获得可确认结果；不归因于工具或
产品中的任何一个，也不写成原生验收通过。针对该疑点追加连续指针按下/移动/抬起、
实际方向按钮点击、保存/只读不写磁盘的GUI回归。补充提交516b1e1482ef1ee0f5e14db71b8e16c8e4e0f60f
通过完整五项[CI42157340095](https://github.com/ForceMind/PeerGlide/actions/runs/42157340095)及两平台
[驻留42157340100](https://github.com/ForceMind/PeerGlide/actions/runs/42157340100)。GUI Unix73项
（Windows71项）及Mac20轮并行回归通过。连续GUI事件可以提交left、按钮回到right，
只读时内存及落盘布局不改；此为软件事件证据，宿主真实鼠标与物理C06仍需另验。

516只增加cfg(test)模块：main.rs生产前缀与0eb逐字相同，其他GUI/CLI/核心/工具/依赖/
CI源码差异为空。保留已核验0eb运行包，不为了测试或文档反复更换App代码身份。
未进行浏览器测试或独立完整安全审计，历史#10与6.83GB根因均未确认。现有短码、
托盘、G1、数据、音频仍未接通，真实双机键鼠仍未验收，不因此关闭对应Issue。


## 快速布局拖动边界（2026-11-06，验证中）

workspace-drag-boundaries/PR52从9be1d3b继续。c8fb49f及绘制前置补齐后的c837f4f均在
三平台复现同帧down/move/up未保存，严格断言保持。0.3.1-dev改为仅在本GUI画布中按序
消费指针事件：有效按下归属/偏移、释放最终位置、只读/失焦/画布变化/离开/滚轮/其他
按钮/Escape取消；无新全局hook，不修改Native/v5。当前候选待CI，不把此软件复现当
宿主CUA问题已定位，物理C06仍未验收。本机预检仍因缺Rust停止，不安装。

A3只读盘点确认标准用户资料目录仅有gui-instance.lock，没有身份或配置。四文件迁移
清单在ignored target/local-validation/profile-migration-proposal/plan.json，源为原私有
配对profile（不是workspace新测试身份）。迁移单独确认已发，未执行，不能把预选或
超时视为同意；需重新核对源摘要/目标空、持两端GUI锁、独占创建并保留源，目标将
native/allow/reconnect置false。Windows短测就绪问题也仍待回复，不自动开启捕获。


用户随后明确批准A3迁移。重新检查源四文件摘要/目标仅gui-instance.lock，持源与目标
GUI独占锁，OpenSSL校验证书与私钥公钥一致后，以独占创建复制四文件到标准用户资料
目录；目录0700/文件0600并fsync，源摘要前后全相同。目标native/allow/reconnect均false，
无系统许可变化、无私钥生成或旧文件覆盖。receipt保存在ignored迁移计划旁；不把资料
复制等同于Native/C06通过。尚需正常GUI默认入口回读，不再要求用户重新配对。

78fba26最初仅Clippy因helper放在test module之后失败（最低Rust编译过、资源两项过），
现移动helper到测试之前，不增加allow/跳过门槛。窄复核还发现并补上Down必须在可见
裁剪区/当前命中层、同批WindowFocused(false)即使最终聚焦仍作废；新增对应负例，
最终候选待完整CI。本批并未证明宿主CUA拖动原因，真实硬件验收仍未运行。
