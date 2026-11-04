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
