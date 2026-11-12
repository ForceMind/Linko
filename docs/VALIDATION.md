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
Windows/macOS/Linux matrix in [run 41393650378](https://github.com/ForceMind/Linko/actions/runs/41393650378),
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
Windows/macOS/Linux CI [41578198350](https://github.com/ForceMind/Linko/actions/runs/41578198350),
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
Windows/macOS/Linux CI [41579175459](https://github.com/ForceMind/Linko/actions/runs/41579175459).
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
passed all five jobs in run [41788814220](https://github.com/ForceMind/Linko/actions/runs/41788814220):
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
five jobs in [41790024756](https://github.com/ForceMind/Linko/actions/runs/41790024756),
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
five jobs in [41798545549](https://github.com/ForceMind/Linko/actions/runs/41798545549):
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
all five jobs in [41806308409](https://github.com/ForceMind/Linko/actions/runs/41806308409):
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
[41818463792](https://github.com/ForceMind/Linko/actions/runs/41818463792):
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
[41826266699](https://github.com/ForceMind/Linko/actions/runs/41826266699):
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
in [41836066218](https://github.com/ForceMind/Linko/actions/runs/41836066218):
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
all five jobs in [41849905620](https://github.com/ForceMind/Linko/actions/runs/41849905620):
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
jobs in [run 41883855509](https://github.com/ForceMind/Linko/actions/runs/41883855509).
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
[CI41959403280](https://github.com/ForceMind/Linko/actions/runs/41959403280)：
正常Up在定位/注入成功前保留归属，未知Up不定位/注入；接管事件不立即反向Acquire，
hand-over/恢复后先interior再跨边。key/button定位/Up失败和不重放回归通过。

PR44源码5cedae946bb36e83300948224c4c698cd1232069通过完整五项
[CI41964917548](https://github.com/ForceMind/Linko/actions/runs/41964917548)：
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
[CI41974906260](https://github.com/ForceMind/Linko/actions/runs/41974906260)全部
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
[CI 41987328608](https://github.com/ForceMind/Linko/actions/runs/41987328608)：
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


首轮运行结果：b8d490f普通五项[CI42017317157](https://github.com/ForceMind/Linko/actions/runs/42017317157)
及隔离[驻留CI42017317320](https://github.com/ForceMind/Linko/actions/runs/42017317320)通过。
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
[CI42039285409](https://github.com/ForceMind/Linko/actions/runs/42039285409)及两平台
[驻留诊断42039285155](https://github.com/ForceMind/Linko/actions/runs/42039285155)。
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
[CI42058770247](https://github.com/ForceMind/Linko/actions/runs/42058770247)：三平台fmt/
严格Clippy/locked测试和Debug+Release、进程probe/reconnect/context、GUI smoke/打包、
最低Rust1.95、依赖审计、Mac解压签名与篡改拒绝。新增6条软件资源/生命周期回归通过。
[驻留42058770207](https://github.com/ForceMind/Linko/actions/runs/42058770207)的Mac任务
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
[42065590838](https://github.com/ForceMind/Linko/actions/runs/42065590838)与两平台驻留
[42065590740](https://github.com/ForceMind/Linko/actions/runs/42065590740)均success。
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
[CI42152464452](https://github.com/ForceMind/Linko/actions/runs/42152464452)及两平台
[驻留42152464382](https://github.com/ForceMind/Linko/actions/runs/42152464382)。包含三平台
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
通过完整五项[CI42157340095](https://github.com/ForceMind/Linko/actions/runs/42157340095)及两平台
[驻留42157340100](https://github.com/ForceMind/Linko/actions/runs/42157340100)。GUI Unix73项
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


A3回读已执行：使用已核验0eb687a包正常入口、不带--data-dir参数，实际GUI显示原
Windows设备且为未连接/不接收控制，没有再次交换材料或生成身份。120秒保护结束
空闲进程，峰RSS120640KiB，无残留；未联网/Native/改系统权限。既有身份现在标准
目录可用，后续常规入口优先标准目录；旧私有profile保留作备份，隔离测试身份禁止
用于真实Windows连接。回读记录在ignored profile-migration-proposal/gui-readback.json。

dcd49d2最低Rust和审计过，平台job仅fmt因测试向量换行未过，已按CI精确修正；不能
把未运行的完整平台测试标通过。最终代码与宿主拖动复验仍待当前候选。


668493a在CI42166093751已通过三平台fmt/严格Clippy/全部单测（含稳定画布同帧快速
拖动、裁剪起点和同批失焦负例）、最低Rust和审计，发布构建仍进行，尚不能记完整CI
通过。生产拖动遗漏已有明确失败→修复证据；不能据此倒推宿主CUA问题根因已定位。
随后87159ad把同批失焦取消一致应用到未提交Native勾选并加负例，最终CI42166487947
及驻留42166487907待完成。不要发布或合并未核验候选，不更换已获权包反复授权。

本轮此前Goal turn有B1实际实现/测试/推送证据，属于progress；本轮又复现快速拖动、
提交修复和负例，并实际完成获准的A3资料迁移/默认GUI回读，仍是progress。Windows
就绪问题尚待回复，不把资料迁移授权当作Native短测授权；主Goal仍active，全路线未完。


0.3.1冻结运行源87159ad已回读完整五项CI42166487947及两平台驻留42166487907均
success；49fc993仅记录，不改受测代码。Mac匹配artifact13479072548下载后核对外层
SHA256=b11da41ba42a5fbcfec5f8f882378db9b720db66e4ef46a1e723299784d9dc22、内层
13969bb40e4b88d73accaf1686342ec090a1c2bf420f2145bc64c70868352969，安全解压/无用户身份/
strict签名/metadata均过。匹配Windows artifact13479128469，本机不运行Windows程序。
本机隔离资料正常入口实际观察0.3.1，方向按钮可切left并回right且落盘，native/allow/
reconnect始终false；宿主自动化drag仍未确认，不能因此宣称其问题根因已定位。132.48秒
GUI正常退出、峰RSS126896KiB、无候选残留，未联网/Native/系统许可变化；真实双机C06
仍未验收，6.83GB根因仍未知。包及报告在ignored drag-87159ad，测试身份不用于真实配对。

A3已按用户授权完成且默认GUI回读通过，无需重做配对。真实配对优先标准用户资料
目录；旧临时/项目资料保留备份。新0.3.1包不保证系统授权沿用，不提前反复索取授权。
主Goal保持active，后续沿A/B认证配对/托盘及用户就绪后的物理短测继续。


## 可选托盘／菜单栏闭环（2026-11-06，开发/验证中）

desktop-tray从a31acf1继续已批准B/U01，版本0.3.2-dev。root负责GUI生命周期/独立
桌面偏好/文档/CI，复用sol/high的localize_panels独占platform tray及其Cargo feature/
lib导出，不派其他或递归代理。原生回调只合并Show/Stop/Quit意图并唤醒GUI，无输入
hook/全局快捷键/脚本/自启。具体决策见ADR-0007。

默认不启用后台保留；启用实际可用入口+单独close-to-tray选择才能隐藏，入口失效
退回合作停止；Stop/Quit在poll前消费取消queued Native，退出等待旧清理/输出完成，
未知恢复显示窗口保留警告。DesktopPreferences独立版本化文件，旧settings.json不加
新字段，旧已获权0.2.4输入GUI仍可读；未来/坏桌面文件保留只读，不隐式开启后台。
新增真实非Native helper/GUI关闭、退出优先/旧输入升级取消、偏好隔离回归待CI。

本机preflight-20261106T055548243581Z-31189仍因rustc/cargo缺失停止，不安装。新Mac/
Windows托盘尚未原生验收，Linux明确不支持，不将代码存在/纯测试当平台完成。窗口
默认资料已完成用户批准迁移，仍不注册启动/服务或改OS许可，也不启动真实Native。


0.3.2托盘候选f624e55在CI42174645783的macOS完整平台检查成功；Windows明确失败于
windows-sys未导出NIN_KEYSELECT宏，不把平台失败掩盖为依赖环境问题。6b36703按微软
SDK宏补兼容定义及鼠标/键盘/打包通知回归；CI42176236476最低Rust/审计和驻留两项
42176236442通过，平台仅fmt两处多行assert未过，按日志精确修正后重跑最终候选。
本机f624e55匹配artifact13482872247外层3337acb6007b009bf414aab56d32610544123a9fb5cb09e68f1dcecd60d1c927、
内层8fd7c69fc759b4822766f7cde4a9b77007b689a2ca2ff20264e87d020972f60b，安全解压/strict
签名/metadata通过。隔离GUI实际启用菜单栏、单独后台保留选项并落盘，native/allow/
reconnect仍false；系统菜单栏UI自动化读取超时，未确认隐藏唤回/菜单Stop/Quit，不能
记原生生命周期通过。600秒保护到时结束无Native GUI，峰RSS130064KiB、无残留；
这是保护终止而非正常退出证明，也不能定位历史6.83GB原因。真实配对/OS许可未变。


托盘冻结运行源bb5f4442b4f911c3444f8b2b886cef38b6a43344已通过完整五项CI42181140226，
含Windows格式/独立CLI/严格Clippy/locked构建测试、Release及进程smoke；macOS/Linux
同样成功，最低1.95和审计成功。两平台驻留42181140346均成功。c87到bb仅修正Windows
Shell_NotifyIconW只读指针，无门槛放宽；e499299仅补GUI操作说明。最终文档提交的
精确CI在PR53回读，不能将旧失败提交标过。主线回读仍5d947637，未合并PR堆栈。
第二次f624本机检查读取已保存桌面选项、点击窗口关闭后，GUI进程继续存在且无worker；
隐藏窗口/Dock读取仍被CUA超时阻断，不证明菜单恢复。600.76秒保护终止，峰RSS
119728KiB、无残留。上述两次保护结束均不是原生菜单Quit正常退出证明。Mac完整
Show/Stop/Quit、Windows托盘/Explorer重启、真实双机C06仍需原生补验；历史6.83GB和
#10根因不变。不得把本批CI或范围化复核当完整独立安全审计。

## 可恢复设备登记与撤销（2026-11-06，开发/验证中）

pairing-enrollment-transaction/PR54从f9caf3a继续已批准B2，0.3.3-dev修复现有人工登记
先写证书再保存配置的失败窗口。设置/GUI锁覆盖有界写前记录与两文件提交；启动仅
owner可恢复，旧/新快照或身份不符保留只读，普通保存/重置不能绕过。记录只含本地设置与公开
证书，hash仅查损坏；恢复不授予接收或Native、不连接网络。旧GUI不理解中断记录，
需先用新版完成恢复。Windows断电语义/冲突修复界面未交付，详ADR-0008。

新增真实测试子进程在写前记录/证书/设置三边界退出后首次与替换登记、撤销的重启
恢复；配置/对端/本机身份替换、坏/未来/过大记录、checksum/授权篡改、第二GUI、
备份冲突和设置锁争用负例。窄只读复核未发现阻断缺陷，补齐首次/合法未来记录/锁
争用缺口；不是完整独立安全审计。当前候选待CI，本机preflight-
20261106T075953685912Z-50271仍因rustc/cargo缺失停止，按用户决定不安装。真实配对
资料未被本次开发/测试修改，不启动Native/改系统许可；C06、托盘原生补验与历史
6.83GB/#10仍未完成。短码生产认证仍未接通，不以手工confirmed字段伪造PAKE结果。

0.3.3运行实现冻结1f42503，完整五项CI42189101407通过；7fcd42f仅改测试私钥比较的
失败输出及文档，完整五项42189688590、两平台RSS驻留42189688611均成功。覆盖首次
登记/替换/撤销在三落盘边界实际子进程退出后的9种恢复、迟到文件改变、未来/损坏/
授权篡改、锁争用与现有真实通信/重连；不把进程退出当真实断电或物理C06。

Mac匹配artifact13488684105外层e4e6c013da53ea16bd4c83c83d8860c27ef622c11454f692d4edc346708f6265、
内层f95394059c3abb90d91de65af185c4f27f50ca6e9e09253747f43882833c54ec，安全解压/无用户
配置身份或未完成记录/strict签名/metadata通过。使用无身份/无对端的独立未来记录
资料实际观察中文只读说明与隐藏重置入口；原设置和记录摘要前后相同，没有生成
身份/对端或联网。109.29秒隔离GUI退出，峰RSS129088KiB。随后CUA读取关闭窗口时
自动重开默认窗口，已通过CUA关闭并独立ps确认无残留；真实迁移源、身份/对端字节
及获准迁移后的配置逻辑值全相同，native/allow/reconnect仍false，无真实待收尾记录。
这些不是长驻内存故障已定位、原生输入或旧Mac故障根因证明。闭窗后不要再查询该App
的AX状态（工具会自动重开），应读取应用清单/进程。报告在ignored enrollment-1f42503。

最终文档候选的精确CI/远端SHA在PR54回读；相对7fcd42f只更新记录，不改运行代码。
Windows原生GUI/真实断电、手工冲突修复、生产短码仍未验/未交付；现有已获权0.2.4
包与实际配对保留供用户就绪后的单向物理短测，不反复要求新中间包授权。本轮为有
源码、回归、CI、本机GUI与推送证据的progress，完整Goal继续active，不合并或发布。

## Mac中文菜单与正常退出保护（2026-11-06，开发/验证中）

macos-safe-quit/PR55从1960986继续已批准B3，版本0.3.4-dev。锁定winit默认菜单英文且
Cmd+Q直达terminate/WillTerminate，绕过GUI的CloseRequested等待；新平台owner不依赖
tray偏好，中文菜单/缺失ShouldTerminate桥接转入同一Quit。保留原delegate/既有IMP，
编译期锁winit0.30.13，运行时核对实例/ABI/菜单绑定；弱注册先解绑，进程永久selector
无owner仍取消终止。GUI邮箱在所有owner创建前一次建立，托盘不换Arc；缺失保护拒新
Native，丢失保护请求Stop显窗，Unknown不能被Quit覆盖。无新输入hook或全局快捷键，
v5/身份/配置/独立授权不改。SIGKILL/Force Quit及注销完整保证除外；详ADR-0009。

复用localize_panels sol/high独占平台四文件，根负责GUI/测试/版本/文档；无递归派工。
首轮b1d2fbe最低1.95/审计过，平台仅fmt35hunks未过；Mac资源编译报3处ProtocolObject
向AnyObject转换不匹配。已按锁定objc2 0.6.5的AsRef API与CI格式精确修正，待新候选
全量检查，未把失败标过。本机preflight-20261106T093137433792Z-63984因Rust缺失停止，
按用户决定不安装；原生菜单/正常系统terminate与真实C06尚未验，无Native或OS权限变化。

fb187af完整五项42200296381及两平台驻留42200296460通过。c22713a五项42204109595
通过，Mac真实AppKit检查回执含running/returned/quit/ownerHealthy/noInput全true，
两端仍未做物理C06。Mac artifact13495433513外层c524e13df993262a667fd2b702c2f3408df2b168bc9213053964fd4c283868b5、
内层fa68d7a3ef7a936e269313d5ace401a7e3eb70d3cf29cc90fb2dacc9a8acc0cf，解压安全路径/
strict签名/metadata已过。本机同包隔离系统检查两次SIGABRT，失败stderr为winit
事件重入（event_handler.rs:135）；崩溃栈在AppKit persistent-state嵌套RunLoop，
直接从GUI logic调用terminate不适用于宿主26.2。此为当前辅助检查的可复现失败，
不能套到#10历史根因，也未改变系统防护或winit重入保护。

46d2f85改为NSOperationQueue主队列异步票据，执行时重新核对真实app/delegate/菜单/
bridge/registry，Pending禁止Close和owner拆卸。返回后才能出回执，超时/缺回执/abort
仍失败；正常生产菜单仍只postQuit，未随诊断改动。该源码最低1.95/审计及两平台
驻留42208139236通过，平台只fmt10hunks失败，按日志修正后再跑全量及宿主复验。

本机c227正常菜单已独立观察中文Show/Stop/Quit等项、Show恢复窗口、实际无Native
回环双CLI认证、菜单Stop、再连接、托盘on/off/on及close-to-tray=true时CmdQ退出。
352.4秒GUI在资源截止360秒前退出（LS观察，未收集GUI退出码），双peer退出0/0，
看到两个独立GUI工作进程且无--native/--allow-control；峰RSS129760KiB，测试身份未变，
native/allow/reconnect仍false，最终无残留。并未点击实际Dock退出，也未做隐藏后
菜单栏唤回或真实键鼠。私有脚本/回执在ignored safe-quit-c22713a，测试身份不得用于
真实Windows配对。主Goal有新实现/CI/失败诊断/本机UI证据，继续active，不合并/发布。

a791445a6e3c69cbd958ccc7e12c91127da4d680运行源码已通过完整五项42209777020与两平台
RSS驻留42209777128，含真正AppKit调用→同一邮箱Quit→健康保护→GUI逻辑关闭的
严格回执；缺回执/abort不算通过。Mac artifact13498951503外层4a1ff2ac739913e5b04b9dd237d88c70503e73ac7259204d31ba4f5e090060a4、
内层c173e2e6298dfc4214d6453a5d2b0e8a5a84060884c1e9413e2007d6d5f4e68e，安全解压/strict
签名/metadata过。同一宿主26.2用新包重做真实API检查，退出0、running/returned/
quit/ownerHealthy/noInput全true；确认当前辅助检查的GUI回调内同步调用重入已通过
主队列修正，不关闭状态保存或winit保护；#10历史根因和6.83GB仍未定位。

用户解锁后，普通GUI在close-to-tray=true时⌘Q退出0（80.09秒、无会话）；之前锁屏
90秒保护到时-15保留为未完成。随后用户实际Dock右键退出，同一受监测普通GUI退出0
（61.16秒，无会话/Native、无残留）。这补齐无Native系统退出入口，不是实际KVM
按键释放或睡眠/撤权/8h耐久验收。收据在ignored safe-quit-a791445子目录；Cua闭窗
后只读app清单/ps，未自动重开默认资料。窗口/工具观察不替代进程退出码。

运行包固定a791445，不因后续纯文档更新替换或重复授权。正常默认入口已显示原Windows
配对。最初实际worker诊断AX/IM均Denied；经用户明确要求代操作后，仅在系统设置
移除两项旧PeerGlide登记，用完整冻结App路径确认0.3.4/30.8MB并重新添加、开启。
系统认证由用户本人完成，未读取/填写密码，未改其他App权限或删除应用/身份/配对。
用户解锁后，同一冻结包的“检查键鼠权限”实际worker只读结果为AX Granted、
Input Monitoring Granted、Secure Input未检测到。未启动Native/通信，Mac接收仍关。
随后⌘Q结束诊断，监督记录gui_exited、55.17秒、峰RSS120656KiB、无GUI/worker残留；
这是短时前置检查，不是资源事故修复或C06验收。私有收据仅保留在ignored target。
当前等待Windows就绪和本次Mac→Windows最多45秒短测确认，不重新交换证书或生成
身份。daff5a9文档候选五项42214599991与驻留两项42214600196全过；本次相对a791
仍仅改记录。主Goal仍active，全路线/物理C06未完成，不合并或稳定发布。

## 认证配对网络基础（2026-11-06，实验资格验证）

PR56 / authenticated-pairing基于137471b，原a791445/0.3.4双机候选保持不变。
新增真实TCP→标准OPAQUE→原socket双向TLS1.3→双向context确认，返回短时私有构造
Session；未写盘/授信/启输入，GUI未接。输入v5及既有人工登记不变，见ADR-0010。

初始4.0.1实现的e669d39已过三平台测试/构建及最低Rust，但未修改的审计拒绝
crypto-bigint0.5.5（RUSTSEC-2026-0387）；未ignore、未将该候选接GUI。官方修复实验
固定027abbc91115baa38b70ad3af7f5a22f072f978b，不能用同名已发布pre2或浮动main替代。
新锁的bigint0.7.10、Argon2.6、dalek5等已核对，137471b所有原依赖版本/源/摘要保留。
voprf仍为.6-pre2，当前组合没有适用的独立密码学审计，网络回归不自动解除GUI冻结。

运行源码3f7d57bf3512400f6c2802436091394a7d1b26e4：完整CI42228351335五项均已回读
成功（Windows/macOS/Linux、最低1.95、严格审计）。三平台fmt/严格Clippy/locked
测试构建/Release和进程/打包专项均通过；后续纯文档候选精确状态继续在PR56回读。
Mac日志实际列出13项配对回归成功，覆盖：真实握手/错码、跨socket尝试预算、取消/
绝对期限、首包边界、OPAQUE身份/角色/offer/attempt替换和KE3重放、KE1/2/3非规范
消息、错pin/无客户端证书/错或无ALPN、KE3/TLS/ready前取消、成功handoff以及取消
计算等待者不释放活CPU许可。固定Argon2参数65536/3/1对照旧.5.3输出fixture也成功。

独立上游源码job42228351494成功：84 unit（含RFC9807向量/旧默认KSF输出）、3项
4.0.1兼容fixture、1项registration及19 doc测试；上游本就标记的2项doc ignored，
未当通过。源码原样在PG workspace之外测试；首轮嵌套workspace失败已保留而非忽略。
测试锁artifact13506399366摘要已核对，PG所选密码依赖的版本/校验和全出现在该锁中；
上游另有dev-only旧digest/hmac/sha2，并非PG生产依赖。固定参数旧输出生成artifact
13505289458摘要已核对。私有下载/锁比较/回执保留ignored target，不上传设备资料。

| 分类 | 本批状态 |
|---|---|
| 已实现并自动验证 | 上述网络基础/有界消息与13项回归；同SHA五项平台CI及独立上游套件均通过 |
| 已实现但实际设备未验收 | 跨主机配对、不同网卡/防火墙、真实用户取消/等待及资源测量 |
| 尚未实现 | 中文短码向导、独立后台worker接线、两端ADR0008提交/回执及提交未知恢复 |
| 客观受阻 | 本机无Rust且用户决定不装；双机C06等待Windows就绪与本次短测确认 |

下一批先保持秘密只走私有stdin/内存而非CLI参数/日志，异步结果绑定GUI代次和原身份/
配置快照；成功handoff后的关闭/重来须显式cancel，不再只drop Offer。提交前再验证
Session，不能伪造人工fingerprint确认。持久intent后取消/断线可能为提交未知，不能
回滚旧授权；配对不自动开通信/native/接收/重连。未完成这些条件不称短码产品可用。

## 独立配对工作进程（2026-11-06）

PR57 / pairing-worker基于3b838ae。隐藏pair-offer/pair-connect仅接受managed私有管道，
身份只读、短码不走argv/日志，无Native/设置/登记写入。IPC1命令256字节前置限长、
公开证书事件72KiB；单槽writer两秒确认防停读stdout卡住异步停止，整个child120秒
绝对期限。Stop/EOF/内部异常终态、预算不随socket重建；异常应用数据Failed/Protocol，
普通EOF/reset为断开，均使临时Session失效。

运行源码c6aaf2450ec88bda3b3d2927c4881e681fc88715完整五项42236705671及独立上游
42236705654已全部成功。三平台fmt/strict Clippy/locked工作区测试构建/Release、
进程/GUI/打包专项及最低1.95/严格审计通过。真实双进程smoke在三平台执行：随机
身份、实际OPAQUE/TLS及context/双证书匹配、Stop/peer-close/错码/等待code的EOF、
Code紧接Stop或EOF、同worker五次耗尽无新Listening、超长输入/缺身份与文件不变。
单元覆盖协议边界、阻塞stdout时Stop、writer/任务/计算/reader panic与期限，并用真实
认证TLS额外字节验证proposal失效。新panic hook不打印raw参数但仍Internal/exit1；
坏stdout不能保证事件送达，父监督器须以EOF/退出失效缓存，不能保留旧Auth状态。

保留失败：da0e722的42235077878 Linux/Windows成功，Mac真实等待code的EOF case
失败。code分支可能在control下次poll前把Stop返回InvalidCommand；c6独立返回取消，
发布前核对控制快照、保留Protocol/Internal失败，原取消断言未放宽。此新worker竞态
不定位历史#10或6.83GB根因。

宿主只解出同源CLI，未提取/注册替换App。artifact13511638062摘要校验通过：
外85304a4e3431ab0eeaa1aa565a0dcc08fd3ca6e7313c814f2ebfd86dfb4f149b，
内0df21a4d4b9074cd36349bbf5a14680371015a3646e5f39bac3e2b3eec0a91b5，
worker4d6c3c2933a115e06c36f681be2ba54ac6177047102f61270d853d043ab7f559。
strict ad-hoc worker标识/签名/版本通过，不代表Developer ID或TCC。脚本与c6源码
相同，宿主回环exit0（约0.55秒）、无强制清理/owned CLI残留，临时身份文件不变。
ignored target仅保留无短码/真实身份内容的provenance/回执；未采RSS、不称内存修复。

| 分类 | 本批状态 |
|---|---|
| 已实现并自动验证 | 只读pairing worker/独立IPC/有界生命周期及三平台真实进程smoke |
| 已实现但实际环境未验收 | 跨主机LAN/网卡/防火墙与真实GUI中的配对/取消体验 |
| 尚未实现 | 父监督器代次/原身份配置绑定、中文向导、两端登记/回执/提交未知；无Commit命令 |
| 客观受阻 | 本机无Rust且用户不装；完整C06待Windows当前就绪与本次短测确认 |

原a791445/0.3.4已获权GUI不替换、不再改权限或真实配对，Mac接收仍关。上述结果
不勾选物理C06，不授自动输入/数据/动作；GUI/持久登记仍冻结，阶段进度不是主目标完成。

## 中文短码与双端持久登记（0.3.5-dev，2026-11-07）

PR58已接中文入口、owned parent/worker、v2协商和ADR0008真实事务。Pending收据在
原settings字节内随journal恢复，实际communication/probe/native/request路径拒绝；
全部初始授权关闭。SaveClaim/ReceiptClaim只由实际child阶段发出，IO终态/取消原子
失效，最终snapshot/完整身份/key-match在settings锁内backup后、journal前复查。
Confirmed只表示本机已记录双方Saved证据，不表示对方也保存了Confirmed。通知丢失
或取消不能补造回执、回滚旧trust或自动从磁盘context恢复Session。v2拒旧pairing-v1/
IPC-v1，输入v5不变；两个节点需匹配新候选，旧人工指纹路径保留。

运行源码d3bf45e915cb7a7fbc18b208f1baadbee22932a6五项42265327473、驻留两项
42265327454、独立上游42265327237全部成功。三平台工作区/进程/打包及mandatory
GUI模型通过。该模型使用匹配Release CLI、两个真实临时profile、实际PAKE/TLS与
本地journal，检查claim前坏key/取消/EOF不写主文件，claim先胜后取消仅Pending、
Pending深入口拒绝、确认/重启一致、私钥不变和全部授权关闭、完成后再开始配对。
未来receipt/外部改写不能reset覆盖亦有回归。默认workspace中该专项ignored不计
通过；独立CI脚本严格要求精确测试exactly1 passed/0 ignored（Mac约2.33秒）。
v2消息交换smoke使用synthetic parent save replies，仅计通信/顺序验收；它不替代
上述真实文件模型。保留失败：c7c2dee失LocalSaved场景误归Protocol，d3改为明确
CommitUnknown/exit1；严格禁止伪造PeerSaved/Completed，未放宽为任意终态。

Mac artifact13526254824外c2fc57e7bdf265cefef3d61a5369d76c015ca828af2d8bc558814b1ecbdd7290，
内d375fb9269e1d306234d1cd222ac89dfb4695fee5b3a09d46afa34712fec7183，安全解压和
strict ad-hoc整包identity/resource seal通过，不是Developer ID/TCC或完整安全审计。
宿主原生单端UI在全新临时profile实际打开中文Home/短码面板、创建临时身份、检查
空地址错误、关闭并退出，无监听/对端登记/Native/系统权限变更。无settings/peer-trust
文件和残留进程；约105秒峰RSS124960KiB，不定位历史6.83GB或#10根因。
发现陈旧“短码未提供”、busy误报只读、英文地址错误；已修分离pairing/Pending/
readonly展示和中文不回显解析，新增语义回归。修复后精确候选32f816c78afcbc0e4e750fa2df4b5b93ca7ed917
五项42269528542、驻留42269528320及上游42269528471全部成功，三平台mandatory
实际GUI模型通过。Mac产物13527989729外层aa267fca432de60e41310a0891769c4b8d587cc8e0f89bd394136662c93136d6、
内层4fec311095837cdfbd7d3bd0e66ad5b16f5fb22cf9b2006685983045e3a54ab5，
安全解压/strict ad-hoc整包验证通过。复用上述仅含临时身份的隔离profile，实际见到
推荐短码的中文首页、短码面板、空地址中文提示、关闭返回首页和CmdQ退出；47.75秒
峰RSS130352KiB，无settings/peer-trust/监听/Native或进程残留。busy状态有自动语义
回归，但标题被固定面板遮挡，本次不计视觉验收；未修改真实配对或TCC。首次复查
180秒到界限由guard停止，另次完成上述正常退出；两次记录均保留在ignored target。
同源Windows产物13528987789外层05edd0b3ca5dc1114df14f56e3abf4707ae8a263bcc0aabd74e331e06f629f4a、
内层40fe1f6106857083df863697055e121de71ac1df02766f145923b3d0e5554988已下载核对，未在宿主运行。

| 分类 | 本阶段证据 |
|---|---|
| 已实现并自动验证 | 中文配对/真实持久事务/回执、claim竞争和重启门槛；32f816c三平台真实模型/五项CI/驻留通过 |
| 已实现但原生未全验收 | 修复包单端基本GUI已复查；原生双端保存点击、Windows GUI/真实跨主机仍待验 |
| 尚未实现 | 短码服务自动发现、新增多设备/多屏、G1全局/数据/拖拽/音频及后续发布项 |
| 客观受阻 | 本机不安装Rust；真实C06待Windows当前就绪及本次短测确认 |

原a791445真实配对/权限保持；尚未合并/发布稳定版，浏览器未验证。CI/模型/单端
UI不勾物理C06，未宣称整个Goal完成。私有host/provenance记录仅在ignored target。

## 短码向导的可选局域网地址提示（0.3.6-dev，开发中）

基于PR58/1090c4d，在独立pairing-discovery分支增加默认关闭的配对mDNS服务类型。
TXT配对2/输入5、公开指纹与LAN地址边界独立检查；旧输入服务不能冒充配对服务。
显示端须另点按钮广播，仅实际owned worker Listening期间可用，公开身份来自该
worker快照；不发布码/OS用户名/私钥。选择仅填地址、清空旧码，不启动连接或授信。
取消/失焦/关闭/退出撤下并等待daemon停止回执，停止失败保留owner重试，不创建
竞争daemon；不刷新PAKE期限或遍历服务器尝试同一码。输入v5/独立授权及原生恢复不变。

新增元数据拒绝/洪泛/到期、关闭全部网卡后的真实daemon注册/撤下/停止、默认不建
网络owner及GUI地址选择不改配置/授权/身份回归。macOS包声明新增服务类型，Python
打包7项本机已过；Rust本机预检因缺cargo/rustc失败，按用户决定不安装。当前Rust
源码尚待CI编译/格式/严格Clippy/测试/build及平台流程，不能计已自动验证。
真实跨主机组播、平台权限提示、网络/睡眠变化和物理C06未运行；浏览器未验证。
原已获权a791445 App、真实profile和冻结32f816c试跑包均保留，不替换或修改TCC。

后续修复记录：cf478c8/14723e6首次因fmt停止，已按runner格式修正；c2f1c0e的
Clippy报告停止按钮可合并条件，907afd6按原意修复，不抑制告警或放宽断言。广播
目标额外绑定owned worker代次/剩余尝试/实际端口，这些只用于撤下旧广播，不作认证。
即使Authenticating/AttemptRejected合并在同一帧，尝试计数变化仍撤下旧广播并停止
专用daemon线程；失效后不会自动重新公布。向导可滚动，保留关闭/取消入口。

运行候选907afd6588daf10006eeb9934d76edc60ab8ec9d的42277310726：最低1.95与
审计成功，三平台fmt和严格Clippy已过，Linux完整工作区/真实进程/重连/GUI/打包
成功。Linux日志明确新元数据/daemon停止/默认关闭/选择不授信回归通过，实际子进程
错误码测试验证剩余次数5→4与停止失效；mandatory真实GUI模型1 passed/0 ignored，
约1.29秒。上游42277310669成功，Mac/Windows后续及驻留42277310683仍须最终回读。
锁文件语义核对394项外部依赖完全不变，工作区版本统一0.3.6-dev。最新精确HEAD
的最终CI、产物及待验收状态集中回读到PR59，不把这一中间记录当最终全平台通过。

## 首次开启的就地键鼠权限引导（0.3.7-dev，开发中）

native-readiness基于已通过完整五项/驻留/上游的PR59最终2a7d698。复用既有实际
sibling worker的只读诊断，在工作区呈现明确本机原因，并在确认框提供重新检查。
诊断完成时写单调时间、排队前固定，30秒过期，GUI暂停不刷新；单次到期重绘而非
新增周期计时。未知/失败/拒绝/不支持/Secure Input不开放同意，过期/再查清旧勾选，
最终GUI请求在偏好写入或进程启动前复查。原生worker仍实际验权，不把诊断当授权。

新增权限状态、精确到期/未来时间/GUI延迟队列、恢复/网络失败/活跃Native优先和
无新检查不写偏好/不建身份/不启动回归。原诊断子进程取消/超时/reap测试沿用。
当前尚待Rust编译/格式/严格Clippy/测试/build及平台CI；本机预检仍因缺cargo/rustc
失败，按用户决定不安装。浏览器/宿主新GUI/真实双机与物理撤权未验收，无TCC或
现用App/身份/配对更改，输入v5和独立授权保持。最新精确候选状态须回读相关PR。

后续验证：0e0db90的fmt及最低Rust测试构造曾失败；测试以显式字段构造保留Drop
回收，没有删回收或跳测试。ec0d987/42282898368的Mac与Linux完整通过，最低1.95、
审计与三平台严格Clippy成功。新增post-release专项tools/worker_permission_check.py
真正用匹配Release CLI运行GUI Checker：有界只读报告、完成时新鲜度、子进程reap、
报告不会替代独立同意，不写设置/创建身份/启动Native；Mac/Linux均明确1 passed/
0 ignored。默认workspace里的ignored不算通过，CI三平台须另行严格执行该专项。

同次Windows唯一失败是“完成前1ns”用例返回Ready；其精确30秒到期断言已过。
测试原先假设1ns可在所有系统形成严格先后；39435a1改为1ms输入并先断言实际
Instant顺序，仍要求未来时间/精确到期均Unchecked，未改生产30秒期限或放宽断言。
Rust说明Instant封装系统类型、行为随平台不同（见[官方Instant说明](https://doc.rust-lang.org/std/time/struct.Instant.html#os-specific-behaviors)）；此处只
修正该跨平台测试输入，不归因历史#10/6.83GB。新候选仍需完整Windows与最终精确
HEAD CI回读。394项外部依赖未变，最终结果集中在PR60，不将这个中间记录当全通过。

## 跨屏切换结果反馈（0.3.8-dev，开发中）

switch-feedback基于PR60最终17e219f（五项42284800216、驻留42284800467和上游
42284800233全部通过）。增加独立switch IPC1有限枚举观察：请求中、匹配拒绝、原
过期、实际协商鼠标能力缺失；Idle不是放行。只增会话循环的观察，未改v5/lease/
1000ms原过期阈值、输入判定、授权、回调或恢复。拒绝不推断具体原因或触发重试。
GUI用每child独立原子快照，断线/重连/停止/EOF/终态/退出清旧提示，恢复/控制状态
优先，不与有界诊断队列竞争，也不导出输入或lease内容。

新增闭合schema/字段与边界、reporter去重/终态/不变更输入、快照洪泛/重连/新child、
显示优先级回归；真实无Native会话收到未请求Denied仍不报告本机拒绝。进程smoke
与重连脚本严格验证新增Idle/无鼠标能力状态及重连后重建/终态清空，不放宽旧输入
Disabled/terminal要求。Python语法已检查，Rust预检仍缺cargo/rustc不安装；当前待
CI编译/fmt/Clippy/测试/build及三平台专项。原生正向拒绝/超时、双机与物理C06未验，
浏览器/宿主新GUI未验，不修改现用App/资料/配对/TCC；不称日常稳定或独立安全审查。

后续：58f3a99最低1.95已编译，平台fmt差异已应用为794dd97；该候选三平台
Clippy/工作区测试均过并进入Release，审计通过。自查发现连接Auth与首条能力反馈
可能跨GUI读取交错：原先先消费后按旧离线状态丢弃。157fb1d改为先观察连接，再取
可显示反馈；离线时保留原子值等下一帧，不清掉新会话首条记录。生产仍按连接状态
清旧提示/每child隔离，新增确定性交错回归；其2处断言fmt继续按runner修正。
394项外部依赖未变。最新精确HEAD全平台/进程/驻留结果集中回读PR61，本记录不
代替最终通过；原生正向拒绝/超时反馈和物理C06仍未验收。

## 首次连接等待（0.3.9-dev，开发中）

first-connection-wait基于PR61最终7c69b75（五项42291710144、驻留42291710040、
上游42291709909全过）。GUI连接端在未选自动重连时显式请求60秒首次预算，CLI
默认不变；listen/probe/未managed/叠加reconnect均拒该模式。只允许原瞬时网络错误
在同地址/固定身份上退避；认证/协议/身份错误或预算到期结束，不刷新总预算。

计时器仅包认证/Hello前的无捕获阶段与退避；实际协商完成、最终身份复查后撤掉，
绝不取消活跃Native会话或清理tail。每次/协商后检查真实证书与私钥可用及peer文件，
身份文件错误即使类似网络timeout也非重试。GUI等待/到期与EOF最后快照处理独立于
认证/授权/输入归属，恢复优先，改设置/新尝试清旧提示；不持久化自动重连偏好。

新增单预算/协商后不失效/身份错误分类、参数拒绝/GUI命令和显示回归；CI新增真实
NoNative双进程延迟服务端、协商后跨预算仍存活、不自动重连、超时、停止、pin变化、
错证书与私钥不可用专项。当前源码/脚本尚待CI编译/fmt/Clippy/测试/构建与专项；
本机预检缺Rust不安装，Python语法已检查。未改v5/ALPN/回调/捕获/授权或真实资料，
宿主新GUI、双机与物理C06/睡眠/撤权未验收，浏览器未验证。最终状态须回读相关PR。

后续：c62bf83最低1.95编译过，首轮fmt按runner应用为24ea232；三平台工作区/
Clippy及审计过。首次startup脚本把强制终止当正常断开，Linux保留了真实connection
lost/timed out失败，原“退出0”断言失败。0b0b44d改正常场景为协作停止（仍严格0），
并新增异常丢失必须失败/不重连，不改生产错误分类或掩盖超时。

0b0b44d/42298681886 Linux完整成功：8项真实NoNative检查逐项PASS（延迟服务端、
协商后跨预算存活、异常丢失仍失败、不刷新预算到期、停止、peer pin变化、TLS错证、
私钥不可用），原NoNative/terminal要求保留；参数拒绝、政策、GUI命令/显示回归均过。
最低1.95/审计/三平台Clippy与工作区测试亦过，其他平台后续/驻留仍待回读；394项
外部依赖未变。后续只补记录，最终精确HEAD结果集中回读PR62；物理C06仍未验收。

4498b51仅补记录，但Windows重现协作关闭失败unexpected end of file：完整Hello后
无更多应用帧，FIN先于连接关闭通知时，read_u32不能区分帧边界与截断。现明确首字节
读取区分正常边界EOF；剩余header/body的截断FIN、非法长度/解码打InvalidFrame标记，
不能被normal-close掩盖或当网络重试。普通连接丢失保留原错误类别，c.closed不再
无条件成功。已观察到无效帧用原子标记防关闭竞态吞错；清理tail始终执行，v5字节/
ALPN与权限不变。固定quinn0.11.12源码包按Cargo.lock SHA核对，确认Tokio AsyncRead
下ConnectionLost映射NotConnected而非UnexpectedEof，未升级依赖。
新增真实TLS完整帧后FIN/截断header/body/零长与NoNative session清理回归，原超长
测试仍验证不读body即拒绝，改用精确错误类型。另给隔离GUI辅助命令10秒（已有cargo
60秒保留）、step5分钟边界及finally始终回收自有GUI；原Linux长作业已自行成功，未
因观察超时重启。当前修复仍待新CI，先前4498b51 Windows失败保留，不计最终通过。

7efc07d已应用EOF回归fmt，42303853610 Linux完整成功：真实TLS完整帧间FIN与截断
header/body/零长的区分、NoNative session对应清理路径通过；8项首次等待进程检查
逐项PASS。隔离Xvfb home/gesture-lab在新的工具与step边界内均通过。最低1.95、
审计及三平台Clippy/工作区测试通过，其他平台后续与驻留仍须最终回读。修复不把
所有EOF当正常，也不把连接中断一概当非法帧；关闭timeout/reset继续失败，已观察
到的非法帧不被普通关闭通知吞掉。最终精确候选状态集中在PR62，物理仍未验收。

## A2驻留CPU曲线（诊断脚本增量，2026-11-07）

resource-cpu-sampling基于PR62最终b101fa1（五项42304880228、驻留42304880246、
上游42304880224全过），仅改诊断工具/回归/记录，不改App运行代码或可见版本。
已有RSS/可选vmmap物理内存之外，新增自有Popen GUI的ps累计CPU时间与单调区间
差值占用率；100%为单逻辑CPU，多核可超过100%，非GPU或全进程树负载。TIME格式
按平台精度保留并换算ns，不宣称OS原生纳秒精度；首点仅基准，零增量可为有效0。

先执行RSS/增长及存活保护，再作最多2秒CPU查询；缺测/坏值/计数回退不插值、不
填0、不越过空档算区间。报告schema2显式CPU覆盖及完整性，CPU采样不完整不能以
RSS成功判整体通过；无CPU预算达标结论。物理内存缺测仍独立null，原报告/样本上限、
独占新文件、CI限定、停止/回收保持。仅相对时间/数值，不保存ps原文/路径/异常。

24项本机Python回归已过，含解析/多核/零值/缺测/回退/RSS先保护/中断优先、实际
自有小Python子进程ps查询与回收。未启动GUI/网络/Native或改权限/真实profile。
当前新代码尚待对应CI真实GUI曲线与完整平台检查。GUI隐藏/worker分账、8h/24h与
物理C06未运行，本切片不定位旧6.83GB或#10根因，浏览器未验证。
CPU口径参考[procps手册](https://man7.org/linux/man-pages/man1/ps.1.html)及
[Apple ps手册源码](https://github.com/apple-oss-distributions/adv_cmds/blob/main/ps/ps.1)；
区间占用由累计差值计算，不使用ps的进程寿命平均pcpu作为瞬时值。

f8e4f21完整五项42308474238与新驻留42308474354均成功。两平台24项采样/回收
回归通过（实际ps测试只查询自有Python child）；新报告schema2四场景CPU完整性
均true，未把物理内存缺测变0。已下载并核对artifact摘要/原始JSON/样本与区间数：
Mac artifact13547252763两场景约181.08/181.50秒，平均CPU0.688943%/0.327514%，
峰区间28.021948%/22.944585%，峰RSS113.609/113.688MiB；物理覆盖partial/available。
Linux artifact13548645117各约180.01秒，峰RSS132.176/133.719MiB，CPU区间178个。
Linux ps整秒计数未跨刻度，曲线算0，仅是其量化分辨率内的结果，不证明完全不耗CPU。
这些是隔离CI/no-session GUI短窗数据，不解释用户6.83GB，也不证明隐藏/worker/
Native或8h/24h耐久。App源码/版本和冻结包不变，最终纯记录HEAD结果集中回读PR63。

## A2真实GUI/自有worker分账（0.3.10-dev，开发中）

resource-session-accounting基于PR63最终ff9cf52（五项42310065470、驻留42310065392
成功）。新增显式CI隔离spec入口：仅接受只有可用身份的独立新目录、新报告、不同
有效peer公钥、IPv4回环及17..600秒。复用原本地登记事务生成关闭全部授权的CI
fixture，再由真实GUI正常start_mode启动同包通信worker，实际固定证书TLS/Hello，
绝无Native/allow/reconnect。既有/未来设置、额外文件/符号链接与非回环拒绝，不
从CI/PID/时间制造认证；普通启动不变。深层Native/保存/登记/撤销/重置/配对受限。

JSONL只报告有限阶段、worker PID/回收、相对时间和native=false。父采样器只查
自有GUI、其实际直接child及自有NoNative peer helper，分开RSS/累计CPU/区间CPU；
每role512MiB为诊断中止线，不是产品预算。成功要求达到窗口、原GUI stop/EOF/
terminal后worker被回收、GUI/helper正常退出；失败/中断仍回收自有Popen，不按PID
杀其他进程。只请求普通窗口，未以内部标志冒充OS可见/隐藏或托盘验收，物理内存
分账本批未测。目标CI为30秒短窗，非8h/24h/Native或双机验收。

4项Python收据/部分写入/父PID/入口拒绝本机已过，不启动GUI；Rust增加新目录/
未来文件/独占报告/loopback/Native防线回归，尚待CI编译/格式/Clippy/工作区与真实
GUI会话检查。本机预检仍缺Rust不安装，无真实资料/TCC/现用App修改，浏览器未验。

05b6748五项42314931276、资源42314931401、上游42314931493已全部成功。原始分账
artifact经摘要/结构核对保留在ignored target/local-validation/owned-resources-05b6748。
约30秒Mac GUI峰RSS115.312MiB、平均单核CPU39.659049%，worker11.875MiB/
0.133361%，helper11.844MiB/0.166799%；Linux GUI132.941MiB/26.668246%，
worker7.613MiB、helper7.508MiB（整秒CPU计数未跨刻度，不能解释为完全不耗CPU）。
三类均正常停止并回收，普通窗口仅requested；不是宿主6.83GB或Native验收。

新分账暴露稳定Communication仍被child/events存在条件每100ms唤起全窗重绘。
本批改实际stdout/stderr读线程在状态/诊断/EOF后唤醒GUI；EOF后的child/events回收
继续100ms，Native健康轮询、停止期限及其他异步工作计时保持。CI资源控制输入也
直接唤醒，连接/采样上限使用一次性deadline，避免诊断模式自行恢复周期重绘。
新增真实共用reader的队列满/terminal/EOF/读取错误唤醒及idle/Native/EOF/Stop时钟
回归。当前改动待新候选CI与同场景分账比较，不沿用05绿灯声明修复通过。本机
预检再次因缺Rust停止，不安装；现用App/配对/TCC和输入协议/原生捕获均不变。

130cd8e的Mac资源run42317164367成功，artifact13551786895摘要
3e15febbefa7aa5ba38b2de4e40aaa162c7a054c2a7efb2549c51d351a967bb9核对一致。
同约30.105秒GUI平均单核CPU0.764002%、峰RSS119.078MiB；worker0.166091%/
11.906MiB，helper0.199311%/11.813MiB，均正常停止回收。与05b6748的39.659049%
相比，短窗结果支持通信周期重绘修复有效；不同CI任务数据，不作耐久/资源预算声明。

504a57e已应用fmt，但旧late_diagnostics回归只装诊断channel、没有真实EOF快照，
仍期待任意活动通信定时重绘而失败。0acdc9f改为经过实际共用reader的空stdout EOF，
stderr sender仍存活；完整保留“晚到诊断读完才回收”与“结束后时钟静止”断言。
0acdc9f最低1.95/依赖审计及三平台工作区测试已过，平台Release/进程/GUI专项继续。
本记录批次只改文档，最终精确HEAD五项/资源/上游结果集中回读PR64及Issue5；不能
把130cd8e的资源成功说成其当时全CI成功。现用冻结App/TCC/身份不变，C06未验收。

0acdc9f的42317591163 Windows/macOS完整成功，最低1.95/审计成功；Linux在GUI专项
之前的apt依赖下载耗尽整步5分钟（日志00:25:05至00:30:18仍在下载imagemagick），
没有启动gui_smoke.py，不能算GUI失败或通过。已将依赖准备独立为10分钟步骤，GUI
自身仍单独5分钟，不跳过/放宽测试或自动重试掩盖错误。两平台资源42317591239成功。
该修正只改CI/记录，App代码仍0acdc9f；最终精确HEAD结果继续集中在PR64。旧#10
根因仍未知，本次apt下载超时不用于解释该历史故障或6.83GB。

## 54. A2真实菜单栏隐藏驻留（2026-11-07，开发中）

tray-hidden-resources/0.3.11基于PR64最终8c9e22f（五项42318814759、资源42318814760、
上游42318814990全部成功）。本批在同一显式CI-only、新目录/回环/NoNative资源模式
增加macOS tray_hidden场景；其他平台明确拒绝，不以内部window_hidden充当OS证明。
只在该隔离新profile内存启用tray/close_to_tray，不保存偏好；普通默认不变。
实际NSWindow performClose→原GUI close分支→AppKit isVisible=false后才开始采样；
结束走真实NSMenu显示/停止selector，须同Mailbox命令到GUI、AppKit恢复可见及原
stop/terminal/EOF/reap。连接20秒、隐藏/显示各5秒；过渡50ms仅用于等待SDK状态，
采样保持事件唤醒；stdin仍只stop，不提供命令执行/授权。报告与父采样schema2记录
有限窗口状态/命令观察；请求/未测不等于成功。队列字段非认证，输入v5/原生捕获不改。

6项Python收据/序列/不支持平台拒绝回归本机通过，Rust新代码待CI编译/fmt/Clippy/
工作区/真实AppKit与NoNative会话资源专项。已有窗口场景保留，新增30秒Mac隐藏曲线；
非物理点击/像素无遮挡/Windows托盘/Native/8h/24h/C06或6.83GB根因证据。本机仍
缺Rust不安装，实际App/TCC/身份/配对不变，宿主GUI受锁屏阻碍，浏览器未验证。

15af2a1的两平台资源42321103689成功，含真实Mac隐藏正例；其三平台CI因fmt失败，
不能把资源成功说成完整通过。a99ef06按runner应用格式，89cf251只补有界阶段收据。
原AppKit代码实际编译/链接成功，最低1.95/审计及三平台工作区测试已过，Release/
进程/GUI及最终精确候选继续回读PR65；本机仍缺Rust不安装。旧App/TCC不替换。

首轮隐藏artifact13552499695的SHA256
579e98dfce5a07d1ec378355d7b77f18f909013b67c60f7b43d443e88b76d690已核对一致，
schema2明确appkit_visible→appkit_hidden→appkit_visible，GUI收到真实菜单Show/Stop，
GUI/helper退出0、worker_reaped=true。约30.158秒GUI平均单核CPU0.364750%、峰RSS
122.375MiB；worker0.165868%/11.906MiB，helper0.199078%/11.813MiB。三类各28样本，
原始JSON保留在ignored target/local-validation/tray-hidden-15af2a1。不是无UI标志模拟。

[AppKit isVisible](https://developer.apple.com/documentation/appkit/nswindow/isvisible)
回读表示SDK窗口可见状态，不能证明像素未被遮挡；显示/停止通过
[NSMenu action](https://developer.apple.com/documentation/appkit/nsmenu/performactionforitem(at:))
执行本应用保留的selector，不是物理菜单点击。此记录后仅补文档，最终精确HEAD五项/
资源/上游状态集中在PR65/Issue5，不把早期短窗当最终数值或8h/24h/Native/C06通过。

## 56. B1中文设备工作区重设计（2026-11-07，开发中）

chinese-device-workspace/0.3.13基于PR65最终f4351e9，响应用户“UI和交互仍未整合”的
纠正：优先实际GUI，Windows PR66独立WIP已推送保留，不覆盖/撤销其代码。固定参考
Oink3ca82127的App/DeviceList/Arrangement组织方式，在原eframe实现设备名单→所选
设备→主屏画布→对应操作；不搬Tauri、不复制第三方图标或Oink品牌，MIT/字体许可不改。

默认进入设备页，真实本机与已登记对端在左侧名单；未添加时只有本机与明确的添加
按钮，不能冒充示例电脑。画布单批mesh网格/原有monitor图形，点击选择、拖动保存
方向复用既有有界/失焦/clip/只读/会话锁定处理；选择不写配置，未变化方向不重复保存。
主动作复用workspace_action/实际worker权限与独立Native确认，停止/恢复入口保留。
设置分常规/系统权限/诊断资料，权限跳转直接到对应分区；地址/证书/原始日志收进
详情，详情按钮不再误打开重新登记。错误仍可展开全文。短码向导调整中文任务层级，
沿用真实后台阶段/取消/两端确认与存储事务；未接通数据和全局手势继续明确标注。

源码当前待CI编译/fmt/Clippy/测试/build与实际截图检查；本机预检缺Rust不安装。
增加默认工作区无身份/网络/授权副作用及紧凑画布边界回归，旧主按钮可见性、布局
失焦/剪切/只读测试保留。Linux隔离Xvfb真实通信场景可显式请求独占PNG，默认不
截图、不读用户桌面，CI测试身份/NoNative与物理双机分开。浏览器未验证，宿主锁屏
不操作现用App/TCC/配对。Oink原始设计参考与实际GUI验收不以一份新方案替代。

首轮ddca37d的资源42330409863成功（Mac/Linux真实GUI/通信/隐藏保留），最低1.95与
审计过；完整CI因fmt失败，未宣称交互验收。实际PNG artifact13557516505摘要
 ddecb5848af0bc306935f3053b9a44cd4582c637c9789c9f7a55564f23016223已核对并读取，
确见设备列表/屏幕画布；也发现详情卡落在首屏下方被截断，不能当设计完成。

df6d1eb已将选中态改浅色强调、导航取消未确认同意/布局；其资源42331253020成功，
最低1.95/审计过，完整CI尚因两处格式未过。后续按实图改画布与所选设备详情并排，
紧凑窗口保留主要操作，权限说明用面向用户的用语，状态来源/30秒期限不变；工作区
PNG提前于两个180秒驻留生成，所有原时窗保留。本批仍需新截图与实际按钮回归验证。

a739219真实标准/紧凑工作区PNG artifact13558058531摘要
a0b5e90bc39c4c3069078be57559625743debf24468b8624494700d475dff4ce已核读，两栏
画布/上下文信息完整，紧凑版底部链接仍偏下。8e7f4d1随后收紧窄画布、恢复内嵌停止/
恢复提示，弃用Ui::id改scope_id；新PNG13559245501摘要
1a1b0b6033da63a3605a2b1f5b96ebd13777adc9a19403d76a4ffe943f1d70c2已读取，920×700
的接收设置/连接详情已在首屏可见，不以旧截断图验收。完整CI此前分别因fmt/弃用API
和恢复面板失去调用失败，均保留证据，不删恢复功能或降警告；a14c2fe仅修新测试格式。

添加流程继续按用户要求重设计：空闲时明确二选一“显示/输入配对码”，只呈现对应
字段和动作；切换停止旧发现、清空旧短码，不启动配对/网络或写入信任。身份准备仍
需显式点击，实际加密握手/双端确认/存储与Native授权边界不变。新增凭据清理回归；
Linux实际GUI继续生成设置/添加弹窗截图，仅读取后才可声明其视觉结果，非真实桌面。

91159ba完整五项42335938393、资源42335938363、上游42335938358全部成功，包括实际
点击/不穿透/选择不改授权/首屏可见/切换入口清空短码回归，以及真实GUI模型配对、
实际worker检查和进程重连专项。新实际PNG artifact13559732485摘要
9a3563261d9b5fcfcb0560d560706c460c53db7b068af3406111ead5da19734f已核，已读取
空工作区/设置/添加弹窗，确实完成导航与打开流程且未创建身份。原图也暴露设置卡宽度
不一、弹窗偏左/过高留白；2b881da据图改统一宽度、浅深外观选择和按步骤居中尺寸。
该批最低1.95/审计过，完整CI因格式未过，现按runner修正，所有断言保留。

本次记录之后不为更新数值反复更换App或TCC。最终候选精确HEAD/五项/资源/上游与
最新图片集中回读PR67/Issue5；已看过的图与原生GUI程序均非浏览器渲染，浏览器仍未
验证。此为B1中文界面/交互切片，不把NoNative图或CI当Windows↔Mac物理C06通过。


## Linko拓扑首页 0.3.14-dev候选（2026-11-07）

设计阶段00db519已完成4个SVG/5个PNG的XML、尺寸、摘要和静态渲染核对；属于效果图/线稿，不是实际GUI。实现阶段改六导航、默认拓扑、右侧上下文、主题与缩放；恢复/待双端回执布局锁、外部配置冲突回滚与真实控件报告已有新增回归代码，待CI执行。

本机预检report.json记录rustc/cargo unavailable；fmt未启动，Clippy/tests/build/probe/reconnect均not_run。未安装工具，未执行浏览器、Native或真实双机输入；现用App/TCC/身份保持。最终候选和实际原生PNG证据在本节后续及对应PR补录，不能以本条候选记录宣称验收完成。

### 0.3.14最终代码候选：74e442b

代码74e442b389b1165fa4df8f14377560d996e5e95e通过[完整五项CI 42362134479](https://github.com/ForceMind/Linko/actions/runs/42362134479)：Windows/macOS/Linux fmt、严格Clippy、locked工作区test/build、Release、进程/重连/配对/worker专项、原生GUI模型、隔离Xvfb、最低Rust1.95和依赖审计均成功。[资源42362134407](https://github.com/ForceMind/Linko/actions/runs/42362134407)两平台通过，含180秒驻留、真实NoNative会话和AppKit隐藏/显示/停止；[固定上游42362134421](https://github.com/ForceMind/Linko/actions/runs/42362134421)成功。880a260首轮fmt失败保留，74e442b仅应用runner的9处格式差异，没有删除断言或放宽检查。

同源6张实际Linux原生PNG已读：默认拓扑、设置、实际点击添加弹窗、手势实验室、已连接及920×700紧凑拓扑。新控件矩形驱动实际点击，打开添加未创建身份。摘要记录在ignored target/local-validation/linko-74e442b-native-images/review-manifest.json；默认页SHA256为4b9711d785889c25dc4ed51b13f633be4831c73c0e418e5a608567010619a45f，紧凑图为93634d45fa89fcbc6c5f60585358c7c336543ab59fb72005ca5345144795c6f1。这是隔离Xvfb/NoNative证据，不是AI图或物理双机验收。

两端包绑定同一run/head，外层digest、内层checksum、ZIP路径/CRC、MIT/notice、版本及PE或Mac完整性seal通过。Mac内层ZIP SHA256为52e0403cc69bf82b0c5e1d971cf9fb94cd820bf556be5e1d2dac35a9a88af194，Windows为fb9e49cd74376176bd13e73a213f2ca3b89eac3eec860dbc4f63e06999438fdd。Windows LICENSE的CRLF归一后与仓库精确一致，原包未改；首次换行敏感检查失败保留。记录在ignored target/local-validation/linko-74e442b-packages/verification-final.json；Mac App已解压，未启动。

两项GUI布局P2已修复并复核；新几何报告路径未发现阻塞项。该静态复核仅覆盖本批，不是全项目独立安全审计。主线未合并，宿主App/身份/TCC保持。Windows/macOS人工界面、输入C06仍未验收；文件/系统剪贴板/全局G1、多屏、远程工作空间与窗口桥未接通。本机Rust缺失、浏览器未验证、6.83GB/#10根因未知的结论不变。


## Linko参考图与对外改名 0.3.15-dev候选（2026-11-07）

现有GitHub仓库rename为ForceMind/Linko，ID、PUBLIC/main、已有PR与0079fe7候选保持，本地origin已回读新URL。b071f78提交canonical引用及artifact名称；当前实现原创主屏示意、真实设备卡与快捷操作层级，新包使用Linko.app/Linko.exe/linko。内部worker、身份/数据目录、bundle identifier和v5兼容保留。

Python打包7项单元/mock验证通过，覆盖新入口、旧包共存不覆盖及严格seal/篡改拒绝代码路径。它不是真实平台签名或启动验收。本机预检仍缺Rust/Cargo，未安装；新Rust检查、进程/重连、实际原生PNG和新包验证待CI，不能复用0.3.14结果称0.3.15已过。参考原图未提交，示例延迟/传输/第三台设备未接入。宿主App/TCC/身份保持，浏览器和物理C06未运行。


## 0.3.16-dev：视觉还原验收重新打开（2026-11-07）

0.3.15功能候选d45f7a1通过完整五项42369775070、资源42369775057及上游42369775059；Linko.app/Linko.exe与两端包均已核验，但用户明确指出实图离参考图很远，因此这些通过不能当作视觉还原完成。旧基线与包保留。

新分支linko-visual-fidelity基于d45f7a1，候选0.3.16使用正式透明设备素材、真实400/550/650字重、1280×800布局、设备信息卡和右三层模块；主区输入/接收状态与真实数据绑定，未实现功能仍灰色说明。图像一次进程解码、每上下文缓存，不在每次绘制重新解码。模型测试使用实际字体/主题。截图改直接捕获CI拥有的唯一XID，不拍整张root桌面、不后期修饰；15项截图所有权与边界Python回归通过。新Rust/缓存回归、180秒资源及实际窗口截图仍待CI，不能先宣称视觉达标。

### 0.3.16代码与实图记录：cb27bce

代码cb27bce72ece205f138180fd1b6ef0c8cbf0764f通过[完整五项42382394527](https://github.com/ForceMind/Linko/actions/runs/42382394527)、[两平台资源42382394470](https://github.com/ForceMind/Linko/actions/runs/42382394470)及[固定上游42382394530](https://github.com/ForceMind/Linko/actions/runs/42382394530)。包括严格fmt/Clippy、locked工作区test/build、最低Rust1.95、审计、进程/重连/配对/实际worker、Mac并行GUI和退出/隐藏/停止，字体/纹理缓存及新弹窗可见性回归。180秒资源观察通过不证明8h/24h或6.83GB根因。

六张同源原生窗口PNG已核对，标准1280×800、紧凑920×700完整显示布局页设备卡与协作能力；新添加弹窗的准备/关闭均可见，打开仍未创建身份。图像为自有XID原始像素，无桌面黑边或后期修饰；数据来自隔离NoNative配置。摘要见ignored target/local-validation/linko-cb27bce-native-images/review-manifest.json，标准图SHA256 ab7630f0eb3621672c8803607e56b3625a0c30ddab77582fcdd8ea7c33f89ca5，紧凑图eed1bd972731fd44266e5a4919d51b3c4cb0ebad0400ef439b3103f6c45cf256。已对照设备素材、字重、Shell、信息卡和完整性；不声称全部像素一致或用户已接受。

两端Linko包已核验同run/head、外层digest/内层checksum、CRC/路径、0.3.16版本、MIT及394项notice、Win PE和worker入口。Mac原包及未改复制件strict seal退出0，复制件plist/helper篡改均被拒绝且原包不变。Mac ZIP SHA256 d49a41aa00a79a7cf9303f2fa665928525a8698448e2a0b7bf1dceab604edc5b，Windows 759e298fd66a5f39f1b0df43a6033d92c1b609a9d89d468c9cb7b6817145a7b1；记录在ignored target/local-validation/linko-cb27bce-packages/verification-final.json。未启动程序/迁移TCC或身份。

## 中文官网0.1.0 / Cloudflare Pages（2026-11-08）

用户授权新建官网并发布到其Cloudflare Pages。分支linko-official-site / [PR73](https://github.com/ForceMind/Linko/pull/73)仅新增website静态资源、专用CI及说明，App仍0.3.16-dev；没有改Rust、原生输入、协议、身份或系统权限。上线地址：https://linko-63e.pages.dev/ 。

- 代码6440ec2178404e79cb8e0bea00c6688fdc40beb3已提交推送、工作区干净后重建并发布，首次部署https://2353e877.linko-63e.pages.dev 。构建白名单10文件合计272360字节；不上传仓库根目录、target、profile、依赖或凭据。公开清单asset-manifest.json记录源提交、App/站点版本和资源SHA-256。
- npm run build / npm run check及三个JS语法检查退出0。独立只读静态审查覆盖HTML/ARIA/锚点/图片、无JS回退、键盘焦点、窄屏和reduced-motion源码、公开敏感内容扫描；无阻塞发现。这不是浏览器视觉验收。
- 实际HTTP回读正式域名及首次部署域名：两端各10个公开文件SHA-256与本地构建逐一一致，MIME、CSP、nosniff和中文404均通过，退出0。首个回读脚本曾因Pages把404.html以308转向/404而失败；已按实际扩展名规则修正脚本并通过，保留失败事实。后续最终发布沿用同一回读命令，公开manifest与PR73记录最终源提交。
- 下载元数据经GitHub API实时回读：run42382394527、head cb27bce、macOS artifact13578905150与Windows13577989202均未过期，外层ZIP摘要匹配。下载需GitHub登录；包超过Pages单文件限制，未搬入官网或新建稳定Release。
- 本机预检preflight-20261107T160305697531Z-85069因无rustc/cargo在fmt入口失败，其余Rust/进程专项未运行；按用户要求未安装。原生运行代码沿用cb27bce完整五项CI记录。本批网站候选CI及最后状态见PR73，不将网站检查视为原生双机、安全或长期内存验收。

浏览器未验证；C06、6.83GB历史内存根因、Issue10根因、数据/G1等未完成项保持原状态。网站无账号、分析追踪或远程字体；访问者仅需普通HTTPS。源码与部署说明见website/README.md，本地HTTP证据在ignored website/deployment.local.json。

过程失败保留：测试模块上下文引用/弃用API已修；实图发现顶栏过高、导航重叠、列宽和首屏截断，均按原生图修复；严格Clippy唯一RGBA迭代告警用稳定API修正，未allow。Windows新增截图测试误把POSIX权限位当跨平台保证，现全平台仍断言O_EXCL/mode参数，POSIX才断言实际权限位；生产Linux门与所有权/原字节断言不变。字体引入的初始配对弹窗挤压经实际图和新模型修复，原流程/同意/取消不改。

保留差异：真实两台/未知系统/未接通能力，未照搬参考的第三台、假延迟、传输或已开开关；Linux/X11与macOS参考的窗口Chrome不同。宿主人工Win/Mac界面与物理C06、生产数据/G1、多屏和窗口桥仍未完成。本机Rust缺失、浏览器未验、#10/6.83GB根因未知，MIT与内部身份/v5兼容保持。主线未合并，未发布稳定版。

## 开源文档与公开开发预览（2026-11-08）

分支open-source-release-docs从bcb3813继续。中文首页、快速开始、CLI、贡献／安全／支持、路线图、更新记录、文档索引、问题／PR模板和发布规范统一为Linko当前范围；历史证据与必要许可记录保留，未据此扩张产品能力。

发布0.3.16-dev仍为Pre-release。原始cb27bce两平台内层ZIP摘要已复核；新包只替换根README／SECURITY，全部其他成员逐字节一致，文件列表与执行权限一致，CRC通过。新Mac包解压后codesign --verify --deep --strict退出0。来源与新摘要在docs/releases/0.3.16-dev-provenance.json；程序Rust源码与cb27bce无差异。

官网0.1.1将两端下载改为公开Release，摘要改为实际便携包；Linux诊断构建仍明确需登录。本地构建、10文件链接／摘要／隐私检查、JS语法和新文档链接／产品文案检查通过。预检因本机无Rust在fmt入口失败（preflight-20261108T022643362173Z-41386），未安装；C06／长期内存／历史故障根因未因发布完成。最终同提交CI、发布匿名下载与Pages回读结果记录于对应PR，浏览器未验证。

## 0.3.17-dev 基础键鼠稳定性候选

源码检查发现GUI对stdout/stderr使用lines()后才截断文本，单行可在入队前无界分配，违背有界管道契约。新增4KiB（含行尾）读入上限，超长行排空至换行且不解析前缀；stdout超限锁存状态不可信，后续terminal不能制造clean；stderr超限仅保留固定说明，继续读取下一行。新增16MiB惰性输入、边界/CRLF/EOF/编码、队列满后终态以及stderr排空回归。只读源码定位，不等于此前6.83GB根因或实机复现。

本机无Rust，预检fmt入口失败，后续not_run；新增Rust回归、fmt/Clippy/build/test与平台检查等待当前候选CI。未启动宿主原生输入或更改系统权限，C06仍未验收。

0.3.17第二步修复startup-held采样：Windows只采左/右/中且initial_keys排除VK5/6，导致已按住侧键可漏过Receiving guard；Mac同样只采三个CG按钮。现分别补两侧键与一次性NSEvent 0..31保守快照，原CG采样保留；该快照可能含合成状态，仅拒绝入场，非物理来源凭证。原生模块回归断言held拒绝、Local Up放行/清hold、随后可入场且侧键不进入接收能力、不转发Input。源码/diff检查过，实际执行待CI。

第三步修复停止竞态：managed stop先原子通知Native，可在session读取Active之后、Release排队前完成清理并关闭inbox；过去Closed直接锁存Unconfirmed。现在只对Closed重新确认原epoch Retired且全局owner Retired后幂等成功，其他错误和shutdown/join/source健康门不变。真实ReceiveEngine+假OS后端确定性回归覆盖owned key释放、leave、退役后队列关闭无重复效果，以及Draining/FAILED/source-lost/未知epoch/新owner和Timeout/Stale/QueueFull负例，不模拟物理验收。

独立静态复核发现stderr坏UTF-8整行已消费后仍提前断管的旧问题；现用显式InvalidEncoding与底层I/O错误区分，坏行固定提示并继续排空，stdout异常仍sticky。补充坏编码后terminal/诊断可读与永久InvalidData I/O不重试的回归。本机公开0.3.16包另完成隔离重连、换pin拒绝、Stop退避取消及全部startup-wait探针（均无输入）；只是本机旧版基线，不作为0.3.17新代码执行证据。实际worker只读诊断两项权限Denied，暂不要求用户给未冻结的新包授权。

首轮5a598f7完整CI 42441898480在三平台fmt失败（6处runner格式差异），Linux截图上传因上游未运行亦无产物；不将其写成通过。按runner输出修正换行，未修改行为、断言或检查门。同步候选README/快速开始到0.3.17-dev，既有公开0.3.16及其官网入口不覆盖。两名代理交叉静态复核GUI管道与Closed退役逻辑，无阻塞发现；这是静态复核，不是独立安全审计。

638e58e三平台通过fmt/严格Clippy/build，最低1.95及审计通过；工作区回归在新增Closed负例准备阶段失败（Received Submitted，错误预期Retired）。既有契约明确Release回执Submitted只表示请求接受，pump后的snapshot/outcome才确认退役；据此修正准备断言为Submitted，并新增独立epoch Retired及owner Retired断言，再构造未知/新owner负例。没有改生产Release语义或放宽最终Closed拒绝断言。Win/Mac新增startup-held回归已在该轮实际通过，整体候选仍需新CI。

## 0.3.18系统图标与视觉候选（未验收）

用户再次否定实际UI与确认参考的落差，视觉验收保持开放。图标问题经源码核实为未接Mac bundle资源及原生viewport，非单纯图标缓存。新增原SVG生成的PNG/ICNS/ICO和原生接线，Mac资源在签名前入包并核验seal；Windows使用SDK资源编译器，另以shell API读取图标对照ICO像素而不执行EXE。打包Python8项、Python编译和sips/iconutil读回通过；实际MSVC链接/新包seal/系统显示未验证。

视觉去顶部冗余空间、内层灰板与过重选择框，调整硬件尺度/基线、卡片和图标；不改业务权限/恢复门，不添加虚构设备或状态。候选需要新原生窗口图与参考逐项对照，旧图/旧CI不能作为本批通过。浏览器未验证，宿主运行中的17 App未替换。

69beb0b首轮完整CI42449294641在11处fmt差异失败，低Rust/审计及资源42449294628、固定依赖42449294874通过。根据runner格式输出逐hunk核对旧源唯一匹配，仅应用空白/尾逗号等价变更，未改行为或断言。新原生图由已通过资源作业产生，仍需读取对照后判断视觉差距；Windows资源链接/图标像素与真实新包seal尚需完整CI。


### 0.3.18中间候选6bc76a5与宿主权限核对

[完整CI42452003149](https://github.com/ForceMind/Linko/actions/runs/42452003149)的macOS/Linux、最低Rust1.95、依赖审计通过；Windows fmt/Clippy/workspace test/debug及Release build通过，但新增shell图标逐ARGB比较失败，其后的Windows专项尚未执行。失败原因待定位，不能把本候选写成五项全过。[资源42452003132](https://github.com/ForceMind/Linko/actions/runs/42452003132)两平台、[固定上游42452003138](https://github.com/ForceMind/Linko/actions/runs/42452003138)通过。

同源原生标准1280×800及紧凑920×700图已读取。artifact13605714068的外层SHA256为890c4597f17c5fe4201c3e3b33becfb6f5db6948241aa39a142560d8889549a3，路径/CRC通过；图为隔离Xvfb/NoNative原始窗口像素。头部/画布较17改善；副标题和设备卡标题仍有对齐差距，不称为用户视觉验收。macOS新图标seal及plist/helper/icon篡改拒绝在CI通过，不代表Finder/Dock或TCC验证。

用户授权代操作后，已在系统UI分别核实旧辅助功能/输入监控登记实际指向0.1.0，用户本人完成系统认证，再选入冻结17。重启旧包误选已纠正，单个冻结17实际GUI worker确认两项Granted、未检测到Secure Input。本机接收控制仍关闭，未开数据/G1。按原Mac→已登记Windows短测授权启动一次，实际UI等待约一分钟后超时、未连接；未作物理输入验收，配对保留。启动弹窗中结果过期/滚动引发同意区重排，作为本批交互缺陷修复；不放宽30秒新鲜度或实际worker复查。

用户允许清理旧版后，仅删除仓库target内42个旧App/篡改测试副本；逐个先核对现有或新建恢复ZIP的文件字节和执行权限。当前冻结17、全部源码/Git/工作树/ZIP、身份与配置均保留。只注销旧App路径并注册当前包，没有全局重置系统登记或删除GitHub仓库。本地清单/回执保存在ignored target/local-validation/old-app-recovery，不上传宿主路径/权限报告。


后续0.3.18源码修正：Native启动确认改为固定检查摘要/固定同意确认和停止区，仅中间说明滚动；不改30秒观察期限、实际worker复查及独立同意。新增两尺寸、报告状态/过期、实际滚动/点击的模型回归，待新CI执行。设备素材由Local/Peer改Laptop/Desktop/Unknown；本机用一次性只读有界系统信息探测，失败/含糊型号返回Unknown。当前v5不交换设备形态，对端明确类型未识别，不能按对端角色/名称猜成台式或笔记本。无新配置、依赖或协议。新preflight-20261108T071226776730Z-4939仍因Rust/Cargo缺失在fmt入口失败，其余未运行，不安装。

宿主冻结17后续：用户本人启动后实际GUI确认认证连接、Native模式/Local；用户物理向右跨屏未成功，Control+Option+Right后仍未进入，GUI读回Switch TimedOut。不能声明C06完成，也不能将超时直接归因Windows无回执；需区分Granted后的本机提交失败与真正无回执。此问题继续独立基础键鼠修复，UI18不会覆盖当前测试包或身份。


0cef8b3中间CI42458858560因20处fmt和新增MouseWheel测试缺phase/source失败；审计、资源42458858546及固定依赖42458858567成功。按runner纯格式输出逐hunk唯一匹配/等价核验应用，再按egui0.37真实定义补Move/Wheel，不改滚动/同意断言。c838c42的Windows作业128655173616已通过PE原资源逐字节与Shell可见RGBA检查，随后test_package在SVG源SHA失败；Windows CRLF字节重算精确复现e644cfb0，仓库LF为33ce4032。仅为批准SVG添加eol=lf，保持源和二进制摘要门，而非放宽断言。新完整候选仍待CI。

本次宿主17正常停止后，GUI回读已断开/Local且无恢复警告；只读进程核对GUI仍在、对应worker已退出。此为软件停止观察，不能冒充物理按键释放或C06通过。此前物理跨屏/快捷键测试未成功，修复后需重跑。

00aa781完整五项42460853547、资源42460853583两平台及固定上游42460853542全部通过，含实际Windows PE/Shell图标与进程/重连/GUI/签名篡改专项。设备形态和固定确认区自动回归已执行通过。最新两尺寸原生图artifact13609419891外层SHA256 f078c660b6890039265def2aed3d70eab07f120f52c6e22af9efd517b0902ae5已核读：本机实际Linux机箱被识别为台式，对端无形态显示未知；未按角色伪装笔记本。该图仍揭示halign不能消除add_sized默认居中，以及新元数据挤到底部说明。c7a0ba8改显式LTR固定文字槽、20px元数据行和中性双屏未知符号，新增实际文字起点/底部clip断言；不能用00aa通过替代新代码CI/新图。

宿主只读规则核对本机型号分类结果为Unknown；不保留/上传原始型号，不能借系统为macOS推断笔记本。对端形态交换尚未实现，当前明确未知，v5/授权不改。

c7a0ba8完整检查42464042816因新增对齐断言的一处fmt失败，最低1.95/审计及资源42464042654、固定上游42464042884通过。已按runner精确应用该格式hunk，未改新断言或实际呈现逻辑；最终提交仍以新完整CI为准。

## 0.3.19-dev 切换提交竞态候选（尚未验收）

来自冻结979的真实短测：Native认证通道已建立，用户物理右边缘与切换快捷键均未成功，实际GUI显示TimedOut；正常停止/Local及worker退出已观察。没有上传真实键值/坐标/桌面/私有身份报告。不能据此断言Win没有回执：源码在匹配Granted后，本机一次begin_routed失败就发Release，但仍留pending到超时，存在与现象相容的竞态。

当前独立修复以原1000ms预算/原归属和活动范围为约束，仅瞬时回调/CAS竞争重试，取消即撤销并Release，拒绝迟到Grant。静态交叉复核发现提交ACTIVE后检查revision及Stop顶检到提交之间的窗口，正在修正；未把该中间实现写为已通过。新增确定性模型回归待CI。本机Rust预检缺失，未安装。最终候选须完整五项/进程/平台/资源检查以及实机重跑。

交叉复核后的实现：原子control内分开21位route epoch、20位请求代次和ARMED，保留COUNT/原安全flag；Local持键同CAS取消请求，Down/Up完成也不能复活旧票据，Remote合法按键对不误取消。Stop沿现有ReceiveControl同步关闭同一原子入场门后再发布stop=true，构造时仅共享原子word，无引用环/新锁。初始watchdog不超过原request deadline；延迟CAS后首callback按实时截止退回Local，成功freshness读回后才由既有路径续期。无新hook、协议、依赖或授权。

新增确定性回归覆盖瞬时忙后提交、原预算/次数、旧/迟Grant、持键ABA、Stop在Native尚未观察时到达、原子代次溢出、跨deadline首Down/Up透传和合法Remote按键对。之前三个静态窗口及Linux测试导出已逐项复查关闭；这是窄范围静态复核，不是独立安全审计或物理验收。Rust执行仍待当前源码精确CI。

首轮d13b6f6完整CI42466346726在三平台fmt失败（七处布局和一处互斥cfg import顺序），最低Rust1.95已通过。按runner旧源唯一匹配核验后应用，未改变行为、断言、预算或取消门；新完整候选继续等待检查。

最终候选准备整合已冻结UI源码a245b21。该UI精确完整五项42465916752、两平台资源42465916763和固定上游42465916745全部成功；最新标准/紧凑原生PNG artifact13612035765（外层SHA256 2d603b234caa5547a4a0ba225041def96c1ba4c77c960f504a5e766c39aa7e26）已读，文字左对齐及底部说明完整可见。本机实际形态/未知对端说明与系统图标检查仍属自动/隔离证据，非C06或用户视觉接受。0.3.19合入UI后必须重跑新HEAD检查，不能套用父分支CI；宿主17不替换，避免中间包反复授权。

为下一次实机复跑补充仅阶段的固定诊断：首次匹配且未过期Granted已收到、等待Granted超时、Granted后本机提交超时或本机安全门取消。只在这些有限状态转换输出固定字符串，不输出按键/按钮内容、位置、lease/身份或时间值，不在Native回调记录。它不改变IPC/输入协议、权限、重试、截止或恢复判断；不据此声称此次根因已定。

本机形态实查还暴露一个直接缺口：新Apple通用Mac编号不带MacBook前缀，初版安全回退Unknown。已按[Apple官方MacBook Air目录](https://support.apple.com/zh-cn/102869)补明确的M4 Air两型号映射，并补正/负回归；不把其他Mac编号一概猜成笔记本。仍只读hw.model且仅缓存粗类型，不查询序列号/UUID、持久化或上传机型。当前对端无形态交换，明确未知。

### 0.3.19冻结运行候选：3c11a88

`3c11a88ac946ef664a87a74056c5b1b18ea62872`通过[完整五项42469667355](https://github.com/ForceMind/Linko/actions/runs/42469667355)、[两平台资源42469667277](https://github.com/ForceMind/Linko/actions/runs/42469667277)和[固定上游42469667274](https://github.com/ForceMind/Linko/actions/runs/42469667274)。含三平台fmt、严格Clippy、locked workspace test/build、Release、最低Rust1.95、依赖审计、实际进程/重连/配对/worker、平台模型与Mac GUI退出/并行、Windows原图标资源/Shell读回、Mac资源seal及篡改拒绝。资源检查不证明8h/24h或历史6.83GB根因。

同提交标准1280×800和紧凑920×700原生图已读，版本0.3.19、文字左对齐、完整设备卡与底部说明已确认；图为隔离Xvfb/NoNative，不是物理桌面。artifact13613126885外层SHA256 `c16a99ad155564a5f6f3f238f9cd39f52fc5fe71e5c85a8ecb27cd481024cddb`，两张PNG分别为`630fd187ff0b588376c9917754b485e131143e54f4f3191c799dba6fd53a38f4`、`c7831c44dbe01c269d687f9c6544afe56cec1f9e5ae75d8e28e899fd69f9892e`。不据此声明用户已接受视觉。

两端包已核验同run/head、外层digest、内层checksum、ZIP路径/CRC、MIT、394项依赖声明和中文字体OFL。Windows artifact13613068244外层SHA256 `87819821e6a56dfc874572eea22214338b7a9bae77d978d54fd6122d42135eec`，内层ZIP `76e7d76c596fb3256b5a16b2f79e820a157dd44b9e52b8624e955aafbcb2532f`，两份AMD64 PE通过。Mac artifact13613661012外层 `9923787b20ca9f447352909c1ba0777262766c816e101d051dbd30fd93426920`，内层 `e21b86c0a1798d467b8a31a4e23b897a67cfbabcb49ee8b42a7bbdfb4149125c`；解压后strict seal、GUI/worker身份、图标原字节/hash2与0.3.19版本均通过，原ZIP不变。仅ad-hoc完整性，不是Developer ID/公证/TCC证明。

Mac实际包helper的`--version`与源Clap契约一致为`peerglide-desktop 0.3.19-dev`；初次验证器误期望二进制文件名peerglide，已按源契约修正，未改程序。该helper在宿主以新临时身份/回环完成reconnect_smoke和startup_wait_smoke全部专项，包括同pin重连、换pin拒绝、Stop取消退避、首次等待/预算、认证及本地key错误不重试；未捕获/注入键鼠，不是双机物理验收。

解压曾因实际磁盘满失败，随后仅清除本次不完整解压，并逐文件/执行权限对照保留ZIP，清理26个旧发布解压副本（约1.4GiB）。源码/Git/工作树/全部恢复ZIP、现用17及身份/配对均保留；没有全局清理。空间恢复后重新解压/核验通过，失败未掩盖。本地来源、验证、图和清理回执在ignored target/local-validation，未上传宿主数据。

**状态边界：**软件及上述自动/隔离检查已完成；19的宿主GUI图标、实际worker权限、原生键鼠与双机C06尚未验收。17实机试跑曾失败，19修复尚未证实解决该次根因。切换到19前Mac实际锁屏，已请求本人解锁；未改宿主App/TCC或开始新键鼠会话。先可沿用Windows现有v5对端验证Mac来源端，不重装/重配对；反向控制仍需独立明确接收同意。对端形态交换、数据/G1等既有未完成项不变。文档收尾不重建或替换冻结3c运行包。

## 本机Mac图标更新与V2线稿

上述锁屏后续已解除。只读进程/bundle确认用户运行17且图标字段缺失后，正常退出17并启动已受测3c/0.3.19。实际GUI版本/中文、现有登记及未开会话状态已观察；Finder系统蓝色双屏图标/0.3.19/35.7MB预览已看，通过原生Finder菜单添加新Dock入口。只读复查本项目仅一个persistent入口、目标为19，且仅19 GUI运行；原包strict seal继续通过。没有替换配对、开启键鼠或变更TCC。Dock控件本身CUA选取曾超时，改走Finder原生菜单并回读目标，未以全局重置或偏好文件写入绕过。此项只验收App/图标/入口，不是19原生worker权限或双机输入验收。

新分支ui-compact-wireframes仅增加V2三SVG/PNG/说明和清单，运行源码相对22df80b不变。顶部64px工具带、64px品牌区及176px侧栏的标准/紧凑稿、固定准备弹窗已渲染查看。实际审版修正了首轮CoreSVG不支持样式表/滤镜导致的字色/字号和白底丢失，改显式属性/普通背板；主CTA改开启流程，移除主界面重复检查教学，保留独立同意/严格期限及未实现标记。最终三图中文与控件无重叠、紧凑底部说明及停止/恢复完整可见。XML/本地资源/画布和控件边界、PNG IHDR尺寸、源与预览SHA、git diff检查通过，记录在docs/design/linko-v2/assets.json；没有复制用户参考原图或宿主截图到仓库。

preflight-20261108T144808048857Z-5294仍因缺Rust/Cargo在fmt入口失败，后续not_run，未安装。该设计批次未更改/重建冻结运行包，适当验证为静态SVG/PNG和原生App更新；浏览器未验证。V2尚未实现和用户确认，现用19仍为旧布局候选；恢复/只读等异常细分和深色稿未全覆盖，后续实现继续现有保护，不据线稿声称功能完成。

品牌V3提案：用户要求重新设计Logo和名字组合，新增三种原创几何标记及1280×900对比板，含48/32/16px、单色与左上品牌区场景。四SVG XML/尺寸/无脚本外链检查、PNG IHDR和SHA检查通过；AppKit离屏导出后已看，字标与图标未重叠，推荐A中文主字标。候选和清单在docs/design/linko-brand-v3；现用App未替换，无Rust/运行行为/系统权限变化，不以设计板证明最终小尺寸或系统图标验收。浏览器未验证。

用户确认A后，brand-a-integration制作0.3.20-dev候选：正式SVG、1024/256透明PNG、ICNS/ICO和来源摘要更新；GUI用同几何三角网格绘制36px标记，中文主字标22px、英文辅助10px，深色使用浅紫以保对比。生成器源路径同步，不改变全局主题、输入/权限或配置。Python打包8项通过；初始离屏脚本使用已移除AppKit函数失败，改用CGContext.clear后透明渲染成功，不改系统环境。Rust/新包仍待精确CI，现用19未替换。

266706f首轮完整CI42498259992三平台fmt仅因网格indices调用换行失败，最低Rust1.95通过；按runner精确格式修正，不改几何/行为/断言。V2三SVG/PNG已同步确认A及中文主字标并核读标准图，资源清单重算；新候选CI仍需完成。新预检preflight-20261108T160719629868Z-15897因Rust缺失未能启动fmt，未装工具；系统图标实际替换待新包核验。

8c6ecbb精确完整五项42498621001、两平台资源42498621000及固定上游42498621004全部成功，包括透明PNG回归、原生图标/包检查与既有进程/平台/GUI专项。同源工作区图artifact13624846326外层SHA69ede860694cd0ae7046ba70100ef57d29586da232402e3fa5dca1d79c838931已核读，实际新标记/中文主字标出现，图为隔离NoNative。Mac包artifact13624861431外层8a9dc7b2f156c5af351230d584aed0f6d7151561242169a22d85371f4484caaf、内层dabc60923bbd9e487b7b5eea2e7e64259bd16d9e3a887bd6d5b612e1c14918a8已核；strict seal、图标/source绑定、版本和完整许可/CRC通过。

宿主已正常退出19并运行20，实际中文GUI确认A标记与屏跃/LINKO层级，Finder显示20/34.8MB和新A系统图标；输入会话保持未开启，没有变更TCC。Dock原生菜单添加了20，但回读出现19/20两个入口，旧项移除未完成。Dock CUAs超时且键盘焦点路径未提供可用控件，已请求用户仅手动移除旧蓝色入口；没有用偏好文件写入或全局重置掩盖。当前应用更新完成、旧快捷入口清理待回执，未将其写成已完成。运行包8c冻结，后续记录不重签/换包，真实双机仍未验收。

用户随后说明实际Dock没有旧项，停止要求删除；偏好记录不能代替用户可见画面。另指出图标比近邻大：源码A底板确实填满画布。修正版仅为Mac增加独立源/ICNS和运行位图，100px透明外边距/824px底板，原A形状与Windows/侧栏不变；该数字是待Dock复查的光学目标，不是引用Apple当前layered规范。新增测试解码ICNS的1024 RGBA及PNG各滤波，验证实际alpha边界100..923（1px抗锯齿容差），同时保留SHA/格式断言，9项Python通过。系统近邻图标采样尝试未取得有效图像，不声称已经实测对齐。新修复仍需精确CI及新包/实际Dock检查；现用App尚未替换。

77b17b4首轮CI42501061930在app_icon.rs的一处链式调用fmt失败，最低Rust1.95已过；按runner合并该行，未变更资源、逻辑或断言。新候选继续检查。

3c645c5完整五项42501372361及资源42501372355两平台均过；实际生成器临时完整运行产物与提交ICNS/Mac256一致，9项Python过。Mac包来源artifact13625478130，外层e57596995d4275374ebc6d3cc1c04c20e4ed607f6c7c9bda03f0e27a912007f8、内层8f9771ee03c04b65113a06139c2d00912271a8054132c186be177797c9f32825核对通过；strict seal、资源/许可/CRC与helper20通过。

本机正常退出后用新完整签名包原位更新安装副本，staging/更新后均验签，保留原副本/ZIP和可回退旧Contents，App外层inode/Dock路径未改；不是直接热改资源或重签旧包。重开GUI正常、字标未变、会话未启用，实际安装的5项关键内容与CI新包逐字节一致；原目录名带8c但当前安装来源是3c645c5，以本地active-installation回执为准。仅刷新该App登记，无Dock/全局缓存重置或TCC变更。近邻图标比较链接只显示占位，未视为有效对齐证据，链接已清除；真实Dock齐平尚需用户观察，不误报视觉验收通过。


## 2026-11-09 功能状态、仓库文档与官网同步

本批 project-status-site 基于 5a2291c，新增 STATUS.zh-CN.md 统一公开 0.3.17、候选 0.3.20／3c645c5、main 和实际 C06 失败的区别。README/路线图/开发计划/索引同步，历史记录保留。官网使用已确认 A 源与8c6ecbb的隔离NoNative实际原图（SHA256 071f2760a97838a44457d87dbd0fcc03c62c4d71a76afc5e0ffa0f11b38481e9），明确非公开17界面，不把V2线稿称为已实施。阶段提交2712fbd已推送。

用户追加国内代理下载：官网0.1.4使用现有Pages项目的限定下载Function，仅转发公开17的Win/Mac ZIP。固定源／官方资产重定向白名单／无访问者凭据／元数据边界／5分钟期限／无整包内存缓冲／无断点续传，保留原链及校验。未购买、改付费计划或引入第三方镜像。不承诺国内可达或提速。12文件构建/摘要/中文/链接/隐私检查及8项下载回归通过；部署与完整在线ZIP校验另记。浏览器未验证，未改Rust或现用App／TCC／身份，不发布新原生版本或合并主线。


官网0.1.4运行内容冻结fe93ffc658f0052c90810d83edd6a74a030d7c07，生产https://linko-63e.pages.dev 与部署https://2143ed5b.linko-63e.pages.dev 已各回读10份公开资源／清单，SHA、MIME、CSP、nosniff与中文404通过。两个生产备用下载均完成HEAD及完整ZIP流式读取：Mac40428229字节／SHA4c6535559e92eb897bf7da82f8c83aed51362294a0e66197d178336bae79b87b；Windows40808848字节／SHA dda2cf284b85bafc661c64ad0658c8ac2d094259450ece986617252f12ab0aef，与公开Release摘要一致。非法平台和带url查询地址在线拒绝。仅此环境HTTP验证，不等于国内网络实测、浏览器或双机验收。

网站精确提交CI42505099725通过；同源全平台回归run42505099719结果以PR85检查为准，收尾记录不改受测Rust或已部署网页。PR85堆叠在mac-icon-insets之上，未合并main，未改公开原生发布。所有既有C06失败／待补验、G1／数据未接通、V2未完整落地以及历史根因未知的说明保留。本机沿用缺Rust预检结果，不安装工具链。


## 2026-11-09 官网系统识别与下载定位

基于41f99bc独立分支download-platform-focus，官网0.1.5增加本地系统识别与当前系统标记。只在进入#preview时滚到对应卡片，首页不强制跳转；初始load／重复点击／hashchange／键盘／减少动画处理，保持其他平台入口。移动与未知环境不误推荐，Linux明确诊断，系统不能证明CPU架构，MacApple芯片／Winx64范围保留。14项回归（8中转+6平台定位）及13文件构建／摘要／中文／链接／隐私检查通过。浏览器未验证；未变更中转代码、套餐、App或Rust，未以UA识别作真实平台验收。用户询问费用后核对官方计费规则并记录Free／Paid区别，未购买或升级，也未凭部署成功断言账户免费。


本轮用户进一步指定沿用另一项目的 `https://gh-proxy.org/` 前缀。官网改为0.1.6：完整GitHub发布URL前加此前缀，明确第三方代理并保留原链和摘要；自建_worker.js／_routes.json及专属Function回归移除，恢复纯静态Pages。系统识别与定位保留，6项相关回归和11文件静态／代理精确前缀／无Function检查通过。此变更取代0.1.4的官网中转方案，不再消耗本账户下载Function请求，不购买或升级服务，也不承诺第三方永久免费或国内速度。8ac63ad系统识别步骤已提交推送；最终代理包与生产静态部署回读另记，浏览器未验证。


0.1.6首轮部署24fa5391运行277c120，生产与部署域名各11份公开资源／清单摘要、安全头、中文404通过；两条旧/download/17路由均404且无下载摘要头。指定gh-proxy.org两个完整包均读取成功，Mac40428229字节、Win40808848字节及SHA与原Release完全相同；不是仅检查URL或响应头。网站CI42511140812成功。先前PR85两个精确完整平台回归42505099719／42505501811已成功，未把之前pending误写成当时已过。复核再补ChromeOS带Linux平台字段的排除，避免误定位Linux诊断；同6项回归及静态检查后冻结最终源码，部署回读见PR86收尾。


## 2026-11-09 冻结0.3.20实际worker准备与回环回归

kvm-020-acceptance延续2ab0cd3，运行程序保持3c645c5／0.3.20-dev，不为本次检查重建、重新签名或更换App。实际GUI只读检查先确认两项Denied；用户明确同意重新登记后，经系统设置移除本项目旧记录并为同一个已验签20包添加辅助功能／输入监控，正常退出重开。实际GUI启动的worker复查通过，未检测到Secure Input；未启动Native会话，Mac接收仍关闭，原有配对保留，未触碰其他App权限或TCC数据库。本轮未要求输入系统密码，不能推断以后无需系统认证。

对现用签名包内实际helper运行隔离NoNative真实进程专项：独立身份认证probe／无对端managed stop、固定身份重连／替换pin拒绝／退避取消、首次等待／建立后不被原等待期限取消／断线不擅自重连／超时与停止／坏pin／TLS失败／坏本机key均过。默认沙箱因禁止绑定回环端口EPERM而先失败，允许范围执行后通过，非产品故障。临时测试身份不用于生产；未使用生产profile或捕获／注入键鼠。strict seal通过。本机预检缺Rust仍失败，fmt／Clippy／测试／构建未本地运行，按用户决定不安装；既有精确3c645c5平台CI与原生物理验收仍分开。

本机准备与隔离通信已完成，尚待Windows就绪回执和用户物理试跑；先沿用既有v5配对做Mac→Windows短测，记录两端实际版本，不把混合开发版本试跑当同源两端最终验收。旧17真实跨屏失败、20未物理重测、四边／键盘／DPI／恢复矩阵尚未完成，Issue3不勾选。ignored kvm-020-acceptance/readiness.local.json只保留阶段状态，无真实输入内容或私钥。没有新增钩子／协议修改／新功能，后续按实测首个差异修复，避免在缺少新证据时继续猜测根因。


本轮同源候选补齐：Mac现用3c645c5／20保持原样，Windows取同次CI42501372361的artifact13625564989，完整39995228字节外层SHA e784c8ae4db3c8421af818bab37bc77c622de42bf30c694a23765fe6452d5288。经GitHub连接器取得完整副本并对照官方摘要后，停止本轮重复慢速下载、保留部分文件；不是把部分下载当成完整包。内层Linko-0.3.20-dev-windows-amd64.zip为41655889字节，SHA32810f4f488c3dca73083464cde36af5fc6c1d82a1155f424574aed3d951ea84；路径／CRC／MIT／394依赖／字体声明和两个amd64 PE通过。本机不能执行Windows程序，原生专项以精确CI为证，物理验收仍未运行。

两包均绑定同一3c645c5和run42501372361；Mac内层41319954字节／SHA8f9771ee03c04b65113a06139c2d00912271a8054132c186be177797c9f32825重新核对。ignored same-source-packages.local.json记录候选准备，尚未发布0.3.20至Releases或更改官网下载，公开17仍保持。用户指出Windows仍17后，已解释公开版与候选未同步；随后用户明确Windows17已等待连接，遂先按既有Mac→Windows授权做混合版本故障复查，不把这一结果当两端同源验收。Mac20实际权限再次Ready，启动弹窗自动点击未稳定勾选，已集中请用户只完成App内检查／勾选／确认，无须重配对或系统密码；此时尚未观察到Native连接，禁止写成已启动／已认证／已跨屏。


## 2026-11-09 混合版本源端切换观察与持键回程候选

用户本人完成App内启动后，现用Mac20实际GUI为已连接／Local，已核对其工作进程带--native／--managed且无--allow-control。用户物理向右短测期间，仅读取本应用窗口，观察状态变为“正在控制对端”；这证明源端已提交远端活动范围，越过旧TimedOut阶段，但Windows实际移动／点击／滚轮／返回结果仍等本人回执，未勾C06。Win17＋Mac20为混合版本，不冒充两端同源最终验收；未发送模拟鼠标或按键。两项权限在启动前由实际worker确认Granted，GUI过期显示不替代会话中权限状态。

继续检查发现普通边缘返回直接无条件restore，即使物理按钮／修饰键仍按住也会发送Release；这与“按住按钮不跨屏”不符。独立kvm-held-return基于e60313d，源码候选21改为原gate同一原子word核对原Remote epoch／COUNT／IN_CALLBACK并退役；竞争保持原会话及运动转发，下一次运动可重试，不在回调等待。返回位置沿用return_pending fence，无第二钩子／协议更改。停止／紧急／失去来源／故障仍无条件恢复，按下抬起归属保留。新增确定性持键／回调／并发Down／旧epoch／关闭源以及原HookState按键与三按钮pair路径回归。

本机无Rust，不安装；新源码尚未执行编译／回归，必须精确CI。现用3c645c5／20与Windows17试跑保持不变，不重签／替换或改配对／权限；新21尚未打包／发布或声称物理拖拽通过。当前修复是代码中独立可核验的持键回程缺口，不将其当成用户尚未回报的Windows响应故障根因。

### 持键回程候选自动验证收尾

运行源码冻结 `8b6f256e80c8855f0430500381bbf97d39917dde`／0.3.21-dev。首轮2199c21的42514360743因4处rustfmt失败，按runner修为84965d3，保留失败和全部断言。自查再补既有tick重试：最后一次Motion可能先于回调退出被消费，不能要求用户再移动才返回；tick先确认输入队列排空，再由原epoch／零持键／无回调的原子检查退役，使用原可靠流和递增sequence发送终点Release。未增加新队列或回调等待。

8b6f256的完整五项 [42515136133](https://github.com/ForceMind/Linko/actions/runs/42515136133) 和固定上游 [42515136009](https://github.com/ForceMind/Linko/actions/runs/42515136009) 全过，覆盖fmt、严格workspace Clippy、locked test/build、最低1.95、依赖审计及原有进程／平台／GUI专项。新增Gate、原CaptureSession恢复fence、并发Down和原HookState key／Shift／三按钮pair回归在对应平台通过；这是软件回归，不是物理拖拽或独立安全审查。被取代的84965d3重复运行42514837616／42514837657已回读为cancelled，最新检查未删减。

同源两端包均来自run42515136133，ZIP路径／CRC／MIT／394依赖和字体声明通过：

| 平台 | Artifact | 外层SHA-256 | 原始便携ZIP SHA-256 |
| --- | --- | --- | --- |
| Windows | 13631118195 | f8a1234f04de2861d0ba17b4e679d529885636827976b22411ad95798b6b89bf | 0bd303fba15e9a3edab87741cf7f0b848496b09a09f89fe78f4b16c44f27a6fc |
| macOS | 13631381120 | e1ff000ab85d5b6f5b72aaf58c4bfc406e219a12656e1d8c9c3d555d642d67ca | 1c7ee821a09cd02062e52f29915bfd202b9f7190996358942a7ed14194480278 |

Windows两个amd64 PE通过；Mac0.3.21 plist、图标原字节、bundle身份、strict seal和实际helper版本通过，原ZIP未改。包在ignored `held-return-8b6f256`；未启动21 GUI、安装、登记权限或公开发布21，现用20签名和TCC保留。

短测后未收到Windows移动／点击／滚轮／返回回执，root通过本机菜单请求停止。随后确认原worker退出且无替代worker；这不证明Windows释放或物理恢复通过。后台窗口捕获仍有旧Connected／Remote画面，显示／缩放／正常退出尝试未给出清晰终态，唯一GUI仍在；1秒线程采样显示主线程正常等待AppKit事件，无Native worker。可能涉及后台绘制缓存或未生效的自动动作，根因未确认，不能声称GUI死锁、正常退出或恢复成功。未强杀、重置权限或替换App；原始采样仅在本地ignored保留。需要用户实际前台状态及物理响应回执再继续，C06、历史内存及#10状态不变。


## 2026-11-09 两端输入链路诊断

用户回执纠正此前等待状态。Mac 0.3.20 与 Windows 0.3.17 已认证，Mac 进入远端路由，Windows 实际无响应，跨屏短测失败。点击、滚轮和返回未单独验证。实际断点待查，Issue #3 / C06 保持未通过。

0.3.22-dev 在原生启动确认框提供“开启并诊断”。有通信会话时先停止再开启。两端沿用既有配对、布局和接收许可，诊断连接禁用自动重连。统计从已认证会话开始，持续至多 120 秒，或在会话结束时提前结束。摘要使用本地 stderr，固定字段、每秒一次、队列容量 2，拥塞可丢快照。GUI 连接详情仅复制通过严格校验的最新摘要。没有网络协议改动。

一次试跑用 Mac 从屏幕中间向右移入边缘，然后继续移动。两端点击“复制本机输入诊断摘要”。先比较 grant/commit，再检查 cap/zero/full/deq/stale/no_lease、tx/tx_*、rx/decode/lease/replay、apply/apply_fail/retired。rel_* 记录有序离散输入传递的阶段。sdk 是原生光标提交计数，包含首次定位及其他光标提交。Windows 最多每 250 毫秒在接收执行器内对提交前后光标位置做一次比较，只保留 moved/unchanged/unknown。即时不变可能是系统提交尚未显现，变化也可能受到其他输入干扰，均不能作为单次提交的最终回执。后续根据首个未推进的计数修复，真实响应由用户观察确认。

本机无 Rust。新候选需通过对应平台 CI 后再提供同源两端包。实际 0.3.22 输入、原生恢复和历史问题 #10 的根因仍待验证。


0.3.22 运行代码冻结 c2582cff5300d00c45dbff46916f711165db09b6，PR #89。首轮 9f05b04 格式检查未过，按运行器格式修正后，五项 Rust CI 42557953020、两项资源检查 42557953179 和上游 42557953024 全部通过。无本机 Rust 安装。

同源包经 GitHub artifact 摘要、内层 ZIP 摘要、CRC／安全路径、MIT 与 394 项依赖及字体声明核验。Windows artifact 13648378369 的内层 SHA-256 为 `9f331c544ab249e1f54cc61f07b624d8530b2b30ed7e433057c7cfcc9a1d2b96`，两个 amd64 PE 通过。macOS artifact 13648890478 的内层 SHA-256 为 `5c18857fe9236883e869c57381a116e73b7d1d1abae430e72db76004a7315666`，完整签名、bundle 和 helper 版本通过。已对打包后的 Mac helper 实测拒绝缺少 Native／managed、带 reconnect 和带 probe 的诊断启动组合，无身份或配置文件生成。目录为 ignored `input-diagnostics-c2582cf`。

摘要入口实际位于 **设置 → 诊断与资料 → 高级诊断**，复制最新有效的 `KVM-DIAG/1` 一行。两端同版试跑及摘要待用户操作，尚未安装或公开发布候选。根因与 C06 结论未变。收尾记录为纯文档改动。


## 2026-11-09 公开诊断预览与官网入口

用户要求直接从官网下载。v0.3.22-dev 为 GitHub Pre-release，标签指向已通过八项 CI 的 c2582cf。两端 ZIP 是同一 CI 的原始包。五个草稿资源的大小与摘要均和本机原件一致，发布后匿名读取的两份 .sha256 及 PROVENANCE.json 字节相同。两个 GitHub ZIP 原链匿名 Range 请求均返回 206，本机整包原链下载超时。经用户指定第三方加速链接完整下载两端 ZIP，大小和 SHA-256 均一致。

官网 0.1.7 部署冻结 45b4ce7，Pages 快照 https://8f64737e.linko-63e.pages.dev/ 。生产和部署域名各 11 份内容的摘要、类型、安全响应头和中文 404 回读通过。静态构建及六项平台定位回归通过。官网与 Release 提供两端同版诊断短测步骤，公开说明 Windows 无响应的根因与 C06 仍待实机定位。未执行浏览器验收。仓库 PR #90，未合并 main。


## 2026-11-09 0.3.22 双向实测及 Mac 回程修复候选

用户反馈同版 Windows 可以控制 Mac，但 Mac 不能移动 Windows 光标。提供的摘要与 Mac 端一致（Windows 即时光标采样三字段为零且原生提交有计数）。摘要记录 rx=962、apply=954、sdk_fail=0。反向有 grant=2、commit=2、cap=2、deq=2，stale/no_lease 均为零且 tx 与全部发送错误为零。按 0.3.22 的分支推断，两条已消费移动触发边缘返回而未发包。累计摘要没有事件时间线或位移内容，不能单独确认位移来源。Windows 摘要尚未提供。

0.3.23-dev 候选取消 Mac 成功切换时投递的居中绝对移动，避免该位移混入下一次物理相对移动。Mac 使用事件的相对位移。Windows 仍按原方式停车，以保留钩子坐标计算所需空间。原始恢复位置、首个真实反向移动返回、持键／归属和停止规则保留。新摘要 KVM-DIAG/2 增加 return 计数。修复待平台 CI 和实机复测。当前官网继续提供 0.3.22，不要求试跑期间更换包。

用户另外报告连接中无法调整布局。0.3.22 通过启动参数传入固定方向，worker 活动期间禁用布局编辑。后续拟经受限本地 IPC 提交方向，仅在两端输入归属安全时应用，作废旧边缘请求并要求回到屏幕内部重新触发。收到 worker 确认后再显示已生效，协议 v5 保留。此行为尚未实现。

0.3.23 运行代码冻结 0061bba17550a1213b20216af2d5249b6f51117d。五项平台 CI 42572655073 和上游 42572654957 全过。同源 Windows artifact 13653922620 的内层 SHA-256 为 0dbf7124619a96f00832fa0c111b0490b94bf44545cb4e1c603827096daf86c5，Mac artifact 13654192593 的内层 SHA-256 为 8eba0a3065115a23512ea34286e13541b45bdaa7ddfebb494655b184da1f094c。ZIP／架构／许可及 Mac 完整签名与 helper 版本均通过。隔离包位于 ignored mac-source-edge-return-0061bba，尚未安装、发布或实机验收。PR91 的独立 Windows 超时测试重跑通过。布局仍按当前锁定规则运行，后续单独接入连接中安全应用。


## 2026-11-09 连接中布局增量（验证中）

分支 connected-layout-updates 基于 62ebff9／PR92。沿用未发布的 0.3.23-dev，增加受限私有管道方向请求、实际 worker 的租约／持键／回调检查、旧 epoch 退役、边缘重新进入和 GUI 确认后保存。输入网络 v5、单一捕获生命周期、逃生与停止路径不变。新增消息边界、队列隔离、捕获门、旧回执、超时和配置冲突回归；tools/layout_smoke.py 使用临时身份与 loopback 的两个真实 NoNative 进程。

本机预检 preflight-20261109T071816745221Z-84718 在 fmt 因缺 Rust/Cargo 失败，后续未运行，遵用户要求未安装。Python 语法和 diff 检查通过；完整五项、进程 smoke 与 GUI 资源检查待精确提交 CI。没有安装候选、改系统权限或启动原生键鼠，双机实际布局与 Mac→Windows 修复尚未验收，官网仍为 0.3.22。


连接中布局运行代码冻结 f8cce5d895c04da76735fb582dedadc5b0da54d5。完整五项 CI 42579669153 与两平台 GUI 资源检查 42579669163 全过；三平台 fmt、严格 Clippy、locked workspace build/test/release、布局/通信/重连/进程专项通过，Mac 20 轮并行 GUI 回归也通过。本轮为主智能体自查和自动验证，不是独立安全审计。首轮 9da082c 的格式失败与 f988fca 的测试模块 Clippy 失败均修复后重新检查，没有放宽规则。

同源 Windows artifact 13656489829，外层 SHA a7aefc42e7fd7fb2418cfd41eaf3ec42058ee07c31fa4c4492b9b1885a4b3961，内层 SHA 1b88c22ea5bd6f7038f6071e8cf94ecd54e24da4139453682fc85d3b9ec95fa6；Mac artifact 13656598909，外层 SHA e72dd745ddd250a9daecf9d67a58f0001d1bfcce17e4dbb3101f0a640b64d945，内层 SHA ffb656dc10ee552f2f89705f4346cf3735a39fd56a940b92b1228f7730f87d58。完整包官方摘要、ZIP/CRC/路径、MIT、394 项依赖和字体声明通过；Windows 两个 amd64 PE、Mac strict seal/元数据/worker23 通过。慢速下载部分保留，已停止本任务重复下载，不以部分文件替代包。

本机从已验签 Mac 包执行 tools/layout_smoke.py、tools/smoke.py、tools/reconnect_smoke.py 全部 exit0：两实际 worker 的四向更新不重连，超长/无效命令正确排空，通信探针、正常停止、重连、替换证书拒绝和退避取消通过。仅使用临时身份与 loopback，无原生输入、系统剪贴板、现有配对或权限变更。隔离证据位于 ignored connected-layout-f8cce5d。运行包尚未安装，双机 Mac→Windows、真实布局和完整 C06 仍需复测。


## 2026-11-09 同源 0.3.23 公开预览与当前实机阻碍

运行包继续冻结 f8cce5d；PR93 文档头 5ffe9d1 的完整五项 42582000425 和资源两项 42582000294 全过，Rust 代码与冻结包一致。PR94 官网提交 772c35281df0f0c710baebc38eb1ea695ca5e68e 的完整五项 42583142554 与官网检查 42583142727 全过。0.3.23-dev 已按既有预览发布授权公开，保持 Pre-release 且不标稳定最新版；标签精确指向 f8cce5d，五个上传资源大小及 GitHub 官方摘要均与本地原始 CI 包／校验文件／公开来源 JSON 一致，未重打包或更改签名。

匿名完整读取 gh-proxy.org 的 Mac 41374121 字节与 Windows 41709611 字节，便携 ZIP SHA-256 分别为 ffb656dc10ee552f2f89705f4346cf3735a39fd56a940b92b1228f7730f87d58、1b88c22ea5bd6f7038f6071e8cf94ecd54e24da4139453682fc85d3b9ec95fa6，全部匹配。GitHub 原链两 ZIP 的 Range206、两个校验文件及来源 JSON 的匿名精确内容读取均有成功证据；部分重试仍返回504，不推断为登录限制，也不保证持续可达。完整代理复核最终 exit0。没有添加自建下载 Function、存储或付费服务。

官网 0.1.8 已部署 772c352，部署地址 https://2f4ca213.linko-63e.pages.dev，正式域名 https://linko-63e.pages.dev 的源码／版本／下载／摘要、安全头、MIME、中文404和两端各11份在线哈希回读通过。只上传网站静态白名单，浏览器未验证。Issue3/5已更新；main仍未合并。

宿主通过应用菜单正常退出22，实际打开冻结23，原配对与右侧布局保留。真实GUI显示0.3.23，实际worker只读诊断确认辅助功能未授予、输入监控未授予，Secure Input未检测到；未改系统授权、未开启本轮原生键鼠。当前需要Windows更新并确认23已打开，以及用户对这个新代码身份的两项系统授权作当前确认；已分别提出最小操作／确认请求，尚未收到回复。系统认证由用户本人完成，不能把等待写成已授予或物理复测成功。当前仍无稳定开发者签名凭据，Windows未知发布者和Mac升级权限沿用没有修复，不能承诺再登记一次便永久解决。本迭代原生复测在这些外部条件前停止，冻结包不重新构建、改名或重签。


## 2026-11-09 冻结 0.3.23 实际 worker 权限复查通过

用户确认 Windows 已打开0.3.23，并明确允许为当前冻结Mac23重新登记辅助功能／输入监控。通过系统设置仅移除屏跃旧条目，两页均用完整路径选择同一冻结应用，文件框预览版本0.3.23／34.9MB。输入监控按系统提示退出并重新打开；本轮没有弹出密码认证，未输入或处理任何凭据。设置列表显示名仍为PeerGlide.app，不能用该名称推断版本，当前已由所选路径／版本和实际worker复查确认。

重启后的真实GUI显示0.3.23，原配对与右侧布局保留。GUI发起的实际worker只读检查确认辅助功能已授予、输入监控已授予、未检测到Secure Input，刷新后的结果没有过期提示。坐标点击未稳定触发后使用Tab／空格在实际GUI中完成检查；不据自动点击问题推断原生输入故障。没有重签、重建、移动应用、覆盖身份或配置，也没有扩大其他应用权限。运行包仍为f8cce5d，长期稳定签名与升级权限沿用仍未解决。

本轮原生会话尚未开启、物理跨屏尚未运行。已集中请求Windows开启其独立接收控制并选择“开启并诊断”；其准备完成回复同时确认Mac本次真实键鼠诊断短测，先测Mac→Windows。等待该回复期间不启动Native worker。既有受测代码和预览包保持冻结，文档变更用diff检查，复用已通过的精确提交平台证据。


本轮后续：用户已确认Windows开启接收控制与“开启并诊断”，并明确同意Mac本次真实键鼠诊断短测。Mac确认框可以打开，权限检查通过，但自动鼠标/键盘操作未能稳定提交最终启动；只读进程核对仍仅有GUI，没有原生worker。观察到Tab可以进入弹窗背后的导航，且30秒观察过期会清除确认；未据此认定全部自动提交失败的根因，更未把未启动算成跨屏故障复测。

当前Mac已留在“开启键鼠协同”弹窗，已请用户只操作“检查键鼠权限”→本次同意→“开启并诊断”，无需系统密码或重新配对。此处是实际GUI操作交接，等待用户报告已连接或等待/失败后再读当前状态；不在交接中继续发送界面输入。尚未观察到本轮认证、Native输入或物理跨屏。冻结f8cce5d程序、已有身份和配对均保留。


本轮连接复查：用户完成Mac当前确认框并报告已连接。随后只读读取实际GUI，确认0.3.23显示“已连接／本机控制（进程报告）”，对端方向右侧。只读进程核对确认冻结f8cce5d的包内worker由该GUI管理，实际带有--native、--managed、--input-diagnostics；不是仅通信或仅界面状态。没有再次改权限、重配对或启动第二条输入捕获路径。

读取时worker已运行2分53秒，120秒诊断采样窗口已结束；后续新动作不能用这段旧快照作为计数证据。已请求用户松开所有物理键/按钮，以Mac物理鼠标从屏幕内部向右越过边缘，继续移动两秒，先不点击；异常用Control＋Option＋Esc恢复。这次物理结果仍在等待，不把已认证／本机控制或原生模式启动记为Windows光标已响应。当前会话保持用户已明确同意的试跑，不向对端发送模拟动作替代验收。


## 2026-11-09 转交独立诊断：0.3.23 单向故障仍复现

用户在两端0.3.23、Mac实际原生连接与权限检查通过后明确报告：Windows光标仍不随Mac移动；随后再次确认Windows能控制Mac，Mac不能控制Windows。取消Mac切换时居中投递的候选并未解决本次实测问题。不能把先前过早回程推断、平台CI或认证连接写成根因已确认／物理修复成功。

第一轮已正常停止，实际worker退出。Mac GUI末尾可见KVM-DIAG/2 expired：cap=0、grant=0、commit=0、tx=0、filtered=1797、rx=1798、lease=27、apply=1771、sdk=1797、sdk_fail=0、return=0；其后出现“Peer grant received; checking local capture admission.”。该汇总仅覆盖早先120秒，后来的物理试验不在此采样内，不能拿它判定本次完整链路。固定数值与范围已保存至ignored connected-layout-f8cce5d/native-023-first-expired.local.json。

用户随后重开Windows诊断；Mac仍用同一冻结包，正常退出并重新打开后重连。用户在立即跨屏提示后再次报告同一单向故障。第二轮也已通过应用菜单停止，进程核对只剩Mac GUI，没有原生worker。第二轮最新末尾摘要尚未导出，仍保留在当前Mac GUI会话记录中；交接前没有继续重启或清空它。请先读取该轮摘要及phase，核对动作是否在采样期，再决定是否需要新的同步双端采样；不要假定已拿到第二轮有效计数。

用户现要求提供提示词给其他AI独立查看。当前停止新增修复，不换包、不重签、不再授予权限；保留现有身份和配对。下一位应从实际捕获→队列→Acquire/Grant→本机提交→移动发送→Windows接收/注入逐段定位第一个分歧，避免重复未经证实的猜测。GUI自动点击与键盘导航未稳定提交的现象不等同于跨设备输入根因；背景窗口截图可能残留旧帧，必须结合实际进程和当前界面。Mac辅助功能/输入监控已在冻结23的实际worker中验证通过，但长期签名和更新权限沿用仍未解决。


## 2026-11-09 旧目录清理后的新会话交接（以此节为准）

用户要求此后只使用移动硬盘上的新工作区，并清理旧目录。迁移已逐文件验证83292个原始文件/链接，四个Git工作区、完整Git对象、npm入口、网站构建/六项非浏览器回归及App签名检查通过。ExFAT的执行位差异用仓库本地core.filemode=false处理，本地排除._*；复制产生的AppleDouble元数据已另行归档。源代码没有因迁移改变。身份和配对所在的Application Support目录未移动。

清理旧App前，已从实际Mac GUI读取并保存第二轮末尾累计摘要至 ignored target/local-validation/connected-layout-f8cce5d/native-023-second-expired.local.json：

KVM-DIAG/2 expired cap=97 zero=27 filtered=1257 full=0 deq=97 stale=0 no_lease=0 grant=1 commit=1 tx=96 tx_unsupported=0 tx_disabled=0 tx_oversize=0 tx_closed=0 rx=1231 decode=0 lease=12 replay=1 apply=1218 apply_fail=0 retired=0 sdk=1256 sdk_fail=0 moved=0 unchanged=0 unknown=0 rel_tx=0 rel_rx=25 rel_apply=25 rel_fail=0 return=1

该数值来自Mac，不能把其中sdk/apply当成Windows的注入结果；moved/unchanged/unknown在Mac为0也不证明Windows光标状态。采样窗口内至少发生过捕获、出队、许可、本机提交及向QUIC提交移动，不能再把旧的tx=0推断作为本轮根因。但累计计数不能精确对应每次动作，tx=96不证明坐标内容正确、网络送达或Windows接收/注入，Windows对应计数仍缺失。用户两次同版实测仍报告Windows能控制Mac、Mac不能移动Windows光标；根因仍未确认，取消Mac切换居中投递并未解决问题。

旧App随后已正常退出，并确认旧路径没有本项目进程。剩余App文件逐个与新工作区副本核对，迁移说明等残留另行备份后，旧工作区目录已删除。只使用新工作区中的冻结0.3.23/f8cce5d包；不要重新创建旧目录或使用旧绝对路径。新路径实际worker的系统权限是否沿用尚未复查，不能把迁移前授权或验签通过当成新路径输入权限已确认，更不要先反复要求用户重配对/授权。当前没有本轮原生worker运行；Windows当前进程状态需另行核对。

用户接下来会新开主执行对话，并让Claude独立诊断。主执行负责集成/最小修复/验证/提交推送；Claude默认只读审查，避免两方并发改同一文件。优先检查首个真实分歧：Mac事件/坐标与能力协商→队列/epoch→Acquire/Grant→本机提交→发送→Windows接收/租约/注入owner→实际光标。不要把CI、系统API返回值或已连接当作物理验收通过，不引入新全局钩子、任意远程命令或安全防护绕过。


## 2026-11-09 新工作区接管：单向故障证据核验

接管起点为干净的 `kvm-023-native-validation` / `0ad92c04940946de246b1d2521c4f5925fe109e8`；远端分支同 SHA，main 为 `921ea7075b0f14d7f697e2198b2bb709cf2d07c4`。PR95 仍开放，base 为 `preview-023-downloads`，没有退回 main 或合并堆栈。该精确 HEAD 的 [Rust CI 42607340092](https://github.com/ForceMind/Linko/actions/runs/42607340092) 已完成：三平台 fmt、严格 locked workspace Clippy、locked build/test/release、原有进程专项，以及最低 Rust1.95和依赖审计五项全过。运行源码仍冻结 f8cce5d，不以文档 HEAD 替换已冻结运行包。

重新读取第二轮本地摘要，cap97/deq97/grant1/commit1/tx96及其限制与最新交接一致。源码核对：Hello 的 motion 是接收支持能力，不协商相对移动帧；Mac callback 读取事件字段4/5的相对位移，session 以本机主屏尺寸累加并限制为0..1的绝对坐标，v5 Motion 经 QUIC datagram 发送。Windows 接收先检查解码、lease、sequence，再由现有 Native owner 提交带 TAG 的绝对 SendInput；注入事件绕过物理接管路径。这些是代码事实，不证明实际坐标、Windows收包或系统响应正确。尚未定位本次物理故障的首个分歧，不修改输入行为。

新路径冻结 Mac App 的 `codesign --verify --deep --strict` exit0。通过正常应用入口打开后，GUI显示0.3.23、原Windows登记和右侧布局、本机允许接收、尚未连接；只读进程核对仅该包GUI，没有原生worker。直接从工具宿主启动同包 `diagnose-context` exit0，报告AX/IM denied、SecureInput inactive、no_foreground；该启动上下文不同，不能作为GUI托管worker权限结论。实际GUI触发只读检查后最后可见状态仍为“正在检查”，未读到最终报告；没有重配对、变更TCC、开启Native或发送模拟跨屏输入。自动界面输入/截图的延迟或检查显示不推进不作为单向故障根因。

已集中请求Windows系统版本/缩放、对端方向、当前会话状态和原KVM-DIAG/2摘要。收到后先比较对应窗口的Mac tx与Windows rx/decode/lease/replay/apply/sdk/光标观察；旧摘要缺失时再组织一次同步120秒内物理短测。Mac新路径权限仍需实际GUI托管检查最终结果。Windows端状态/采样窗口未知，根因与C06仍未确认。本机仍无Rust/Cargo且未安装；本批仅记录，自查diff，不新增浏览器测试、发布或机器配置变更。


## 2026-11-10 KVM-DIAG/3：补齐单向故障与滚轮的计数盲区（待 CI）

起点 `617158f`，运行包仍冻结 f8cce5d／0.3.23-dev，本批没有改任何输入行为、线上协议、v5／ALPN、捕获生命周期或安全门，只增加会话内聚合计数。第二轮 Mac 摘要只能确认发送链到 QUIC 入队：`deq` 等于 `tx`＋一次边缘返回，说明唯一一次远端阶段由 Mac 的第 97 个移动事件结束；Pong 超时、对端 Release、能力不支持三条结束路径、位移方向、滚轮和 Windows 接收退役原因此前没有计数，所以不能据 Mac 一端判定首个分歧。

新增字段（摘要版本 `KVM-DIAG/3`，旧 `/2` 行不再被 GUI 复制入口接受，诊断需两端同包）：

- 发送端阶段结束：`pong` 收到当前租约的 Pong；`pong_to` 因 1 秒无 Pong 恢复本机；`peer_rel` 对端 Release 结束远端阶段；`cap_stop` 因对端不支持某输入而停止。
- 位移方向：`mv_pos`／`mv_neg` 为 dx 大于／小于 0 的已转发移动事件个数，只计数，不记录数值。可与 `return` 对照实际轨迹是否回拉。
- 滚轮：`wh_phys` Windows 钩子看到的物理滚轮事件；`wh_inj` 带注入标记而被放行不转发的滚轮事件（平滑滚动驱动可能如此）；`scr_tx` 发出非零滚轮；`scr_zero` 发出 0／0 滚轮（亚格累加未满一格）；`scr_apply` 接收端提交滚轮。`wh_*` 只在 Windows 钩子计数，Mac 恒为 0。
- 接收端数据面被退役的首个原因：`g_time` 命令超期或被取消；`g_state` epoch 已不在 Active；`g_source` 输入源观测失效；`g_epoch` 捕获门已属于另一接收 epoch；`g_mods` 仍有被吞修饰键。此前合并在 `retired` 里无法区分。原判定顺序与短路不变，只在失败时多记一笔。

读数建议：先看 Windows 行 `rx`；`rx＝decode＋lease＋replay＋apply（＋retired＋apply_fail）` 应闭合。`rx` 近 0 查传输；`lease` 高查心跳／Pong；`retired` 高看 `g_*`；`apply`≈`sdk` 且 `sdk_fail`＝0 而 `moved`＝0、`unchanged`＞0 属系统层问题。滚轮先比较 Windows 的 `wh_phys`／`wh_inj`／`scr_tx`／`scr_zero` 与 Mac 的 `rel_rx`／`scr_apply`。

代码核对（读代码，未运行）：滚轮路径 Windows 钩子→整格折算→可靠流 Input→Mac `CGEventCreateScrollWheelEvent`（Rust 以可变参数声明，调用约定正确）一路没有发现缺陷；Oink 参考实现使用 `CGEventCreateScrollWheelEvent2`、像素／行单位和阶段字段，本项目只发整格行单位。是否需要对齐取决于上述计数。Mac 位移取自 CGEvent 浮点字段 4／5，Oink 同样读取这两个字段；其 `physical_motion.rs` 头注释只说明拖动帧没有可用的 HID 子事件、CG 浮点字段与 NSEvent 恢复不出拖动数据，改读 IOHID 指针子事件用于普通帧，不能据此认为本项目的位移取法不可靠。更换与否等计数再判断，不预先更换。（此前此处误写“Oink 称其在部分帧不可靠”，已更正。）

摘要最长约 892 字节；`MAX_SUMMARY_BYTES`＝1200 同时约束摘要校验和 GUI 诊断行截断（原 600）。新增回归：最大摘要不超限、旧版本行被拒，以及过期数据命令按 `Time` 记录并仍然退役。本机无 Rust/Cargo，未编译；fmt、严格 Clippy、locked test/build 以精确提交 CI 为准。真实物理验收仍须同包双端在同一 120 秒窗口内复测并取两端摘要，不能以 CI、连接成功或 SendInput 返回当作通过。


## 2026-11-10 KVM-DIAG/3 首次双端实测与 /4 补充（待 CI）

用户用 e0f75c6 的同源两端包（Rust CI 42666341955）做了 120 秒窗口内的短测：Windows 能控制 Mac 的鼠标和键盘，滚轮不工作；Mac 仍不能移动 Windows 光标。现象由用户本人回报；以下读数来自用户复制的 Windows 摘要，Mac 摘要尚未取得，不能据此下全链路结论。

Windows 摘要（`KVM-DIAG/3 expired`）要点：`cap＝deq＝tx＝2408`，`grant＝commit＝3`，`pong＝478`，`peer_rel＝2`，`return＝0`，`mv_pos＝1056`／`mv_neg＝555`；滚轮 `wh_phys＝222`、`wh_inj＝0`、`scr_tx＝216`、`scr_zero＝0`，`rel_tx＝562`；接收侧 `rx＝decode＝lease＝replay＝apply＝retired＝sdk＝sdk_fail＝0`，`filtered＝5`，`g_*` 全 0。读法：

- Windows 作为源端工作正常：物理滚轮被钩子看到（没有被驱动注入标志旁路），折成非零整格后经可靠流发出 216 次，没有零增量。Windows 一侧不是滚轮失效的位置，断点在 Mac 接收、注入或系统效果，需要 Mac 的 `rel_rx`／`rel_apply`／`scr_apply`／`wh_inj` 才能再分。
- 三次远端阶段中两次由 Mac 的 Release 结束（`peer_rel＝2`），没有一次边缘返回，与 Mac 端物理输入接管 Windows 控制的设计一致，不是故障证据。
- Mac→Windows：Windows 在窗口内 `rx＝0` 且 `sdk＝0`。Windows 授予 Acquire 前必须先成功放置一次光标，那会让 `sdk` 加一，因此该窗口内 Windows 没有授予过任何 Acquire（或 Mac 一次也没有发出）。断点在 Acquire／授予阶段，早于数据报与注入；这与第一次实测不同（当时 Mac 有 `grant＝1`、`tx＝96`）。`filtered＝5` 与三次 Windows 源端停车加两次回位的本机合成移动相符（推断）。
- 现有计数没有覆盖 Acquire 阶段的拒绝原因，所以本批补 `KVM-DIAG/4`（本机无 Rust，未编译，待 CI）。

`/4` 新增字段（线上协议不变）：源端边缘判定 `edge_hit`（到达边缘）及首个阻挡原因 `blk_peer`（对端无移动能力）、`blk_gate`（捕获门非 Local 空闲）、`blk_busy`（已有发出中的请求或租约）、`blk_recv`（本机仍持有接收租约）、`blk_prot`（接收保护尚未撤销）；`acq_tx` 发出 Acquire、`denied_rx` 收到 Denied、`acq_to` 等待授予超时、`acq_cancel` 本机准入保护取消；接收端 `acq_rx` 收到 Acquire、`deny_arg` 参数无效、`deny_busy` 本机忙、`deny_auth` 未授权接收控制、`deny_lease` 租约被拒（已有租约或编号复用）、`deny_begin` 进入接收失败、`deny_place` 入口定位失败。macOS 的 `wh_inj` 现在统计回到 tap 的自注入滚轮，`wh_phys` 统计未带标记的滚轮；若 `scr_apply` 很大而 `wh_inj` 为 0、`wh_phys` 很大，说明注入的滚轮没有带上自有标记，被当成本机物理输入而吞掉并请求接管。摘要上限提高到 1600 字节，最长约 1237 字节。

下次复测先看 Mac 行：`edge_hit` 是否大于 0；若是，`blk_*` 哪一项非零（本机没发出 Acquire），或 `acq_tx` 大于 0 而 `denied_rx` 非零（Windows 拒绝，再看 Windows 行的 `deny_*`），或 `acq_to` 非零（Windows 没回应）。


## 2026-11-10 方向不一致提示与零增量滚轮跳过（待 CI，未实机）

背景（用户实测，未由本批复现）：两端都保持默认“另一台在右侧”，而物理上 Windows 在 Mac 左侧，Mac 只能向右推才会跨屏，表现为“Mac 过不去”；把 Mac 改为“Windows 在左”、Windows 保持“Mac 在右”后恢复正常。Linko 此前不检查两端方向是否互补。

本批改动（线上协议、ALPN v5、授予／拒绝逻辑均不变，不新增钩子，回调内无新增工作）：

- 接收端在 `Message::Acquire{edge: remote_edge}` 处比较 `remote_edge == 本机 edge.opposite()`。不一致时，只经 worker 私有 stdout 发出一行 `layout_hint`（`crates/peerglide-protocol/src/layout_ipc.rs` 的 `LayoutHint`，字段 `local`／`peer`，256 字节上限，未知字段／版本被拒），同一 (本机方向, 对端方向) 组合只发一次，方向互补后清除记忆。
- GUI 在 `read_worker_output` 解析该行，存入每个子进程独立的共享槽位；方向选择器下方显示“两台电脑的位置不一致”和“改为 X 侧”按钮，按钮走既有 `try_set_edge`，连接中即既有的 `request_edge` 实时更新。提示只在连接中、子进程存活、显示方向仍等于提示比较时的本机方向时出现；点击成功、方向改变、断开或停止后消失。
- `Captured::Input` 分支遇到 `Scroll{0,0}` 时只计 `scr_zero`，不再经可靠流发送；非零滚轮的 `scr_tx` 计数不变。

读代码得出的依据：发送端 Acquire 携带的是发送端自己的 edge（对端在其哪一侧），所以双方互补时 `remote_edge == edge.opposite()`；接收端入口位置本就用 `remote_edge.opposite()`。提示只在对端实际尝试跨屏时才会产生，且只能说明两端“不互补”，不能判断哪一端与物理摆放相符，界面文字因此只陈述对端的设置并给出镜像方向的一键修改，是否采纳由用户按实际摆放决定。零增量滚轮在本机已被钩子吞掉，跳过发送不改变本机行为；上一轮 Windows 摘要里 `scr_zero＝0`，所以它不是那次滚轮问题的解释，本改动只是省去无效消息。

新增测试（均未运行）：`layout_ipc` 往返与畸形输入拒绝；session 中的去重函数与零滚轮判定；GUI 中提示随连接／显示方向出现与消失、点击产生方向为镜像值的 `request_edge`、提示行进入槽位而非诊断队列。

未验证：本机没有 Rust/Cargo，未编译、未运行任何测试；fmt、严格 Clippy、locked test/build 以精确提交 CI 为准。未做任何真机或 GUI 渲染检查。物理验收仍需用户在两端确认：方向不互补时哪一端出现提示、按钮文字是否清楚、点击后两端可以双向跨屏，以及方向互补时不出现提示。该验收属于 #3/C06，不能由 CI 或合成事件勾选。

评审修正（同一 PR）：提示行改为“观察记录”，一致的组合在刚报告过不一致之后也会发出一次，用来撤回 GUI 里过时的提示；GUI 点击后不再立即清除提示，而是等 worker 确认新方向（显示方向改变后自然隐藏），被拒绝时保留以便重试，等待确认期间按钮禁用。另按 rustfmt 调整一处断言格式。仍未在本机编译。


## 2026-11-10 Windows→Mac 滚轮：Apple 芯片上的可变参数调用约定错误（待 CI 与实机）

用户第三次双端短测（Mac 为 `/4` 包，Windows 仍为 `/3` 包）：Windows `wh_phys＝44`、`scr_tx＝44`；Mac `scr_apply＝44`、`wh_inj＝44`、`sdk_fail＝0`。即 44 次滚轮全部到达 Mac、提交成功，并以本进程标记回到 tap 后被放行，没有被当成本机物理输入吞掉；用户仍看到 Mac 不滚动。

原因（读代码与调用约定得出，未经实机验证）：C 原型为 `CGEventCreateScrollWheelEvent(source, units, wheelCount, int32_t wheel1, ...)`，`wheel1` 是具名参数；本项目把它声明为 `fn CGEventCreateScrollWheelEvent(source, unit, count, ...)`，把 `wheel1` 也作为可变参数传入。Apple arm64 调用约定把可变参数全部放在栈上、具名参数放在寄存器里，所以系统读到的纵向值是寄存器中的残留值，我们传的纵向值被当成横向值；x86_64 上两者恰好一致，因此只在 Apple 芯片上出现。修正改用定长的 `CGEventCreateScrollWheelEvent2(source, unit, 2, y, x, 0)`（Oink 参考实现同样使用该接口）。仓库内没有其他可变参数 FFI 声明。排除过的解释：滚轮被本机 tap 吞掉（`wh_inj＝44` 否定）、Windows 未发出或零增量（`scr_tx＝44`、`scr_zero＝0` 否定）。

同一轮另见 Windows `rel_fail＝1`：一次来自 Mac 的可靠输入在 Windows 注入失败，按现有代码会使会话以错误结束（两端摘要均为 `end`）。具体错误需要 Windows 会话记录中的错误行，尚未取得，不在本批修改。


## 2026-11-10 Mac→Windows 滚轮后会话因原生接收超时而结束（待 CI 与实机）

同一轮 Windows 会话记录末尾为 `Error: native receive service: Timeout`，结束摘要 `rel_rx＝2`、`rel_apply＝1`、`rel_fail＝1`、`scr_apply＝1`、`wh_inj＝1`：Mac 发来的第一次滚轮在 Windows 注入成功，第二次的原生命令在 250 ms 内没有回复。超时会使取消守卫请求恢复，原生端随即把这次接收标为排空并清理已注入的按下（现有测试 `bounded_timeout_cancels_late_begin_and_down_even_without_any_native_pump_progress` 覆盖这一点），但会话层把 `Timeout` 当作致命错误，直接结束了整个连接，之后的滚轮与移动全部丢失。

本批只改会话层：输入或光标命令超时且该接收已不再活动时，写一行诊断并按“未提交”处理；下一轮循环走既有路径释放租约、通知对端 Release，并照旧检查原生端确认清理完成，无法确认时仍按原逻辑报错结束。原生恢复与已注入按下的清理逻辑不变，线上协议不变。会话层这一分支没有新的自动测试；物理效果需要两端同包复测。为什么第二次命令会超过 250 ms 仍未查明，本批不修改超时时长。


## 2026-11-10 集成包实测：Windows→Mac 滚轮恢复；Mac→Windows 仍因 Windows 原生线程停滞而反复中断（/5 计时计数，待 CI）

用户用集成包 e31a38a（PR97–PR100）双端实测：Windows→Mac 滚轮恢复正常（Windows `scr_tx＝188`，Mac `scr_apply＝188`，用户确认），确认 PR99 的调用约定修复在 Apple 芯片上生效。Mac→Windows 仍不正常：Windows `rel_rx＝60`、`rel_apply＝40`、`scr_apply＝34`，记录末尾仍为 `Error: native receive service: Timeout`；Mac `peer_rel＝4`，即 Windows 四次主动结束 Mac 的控制。PR100 已使输入超时不再断开连接，这次的超时出现在释放或进入接收阶段，说明 Windows 原生线程停滞的时间超过单条命令期限。用户同时看到 Mac 光标在控制 Windows 时仍跟着移动，PR98 的解耦在后台 worker 进程中未生效（与该 PR 中记录的风险一致）。

为区分 Windows 停在系统注入调用还是线程别处，本批新增 `KVM-DIAG/5` 三个计时计数（只在原生循环内计时，不在回调中）：`slow_ptr` 单次光标注入超过 50 ms，`slow_inp` 单次输入注入超过 50 ms，`loop_stall` 原生循环两轮之间超过 200 ms。线上协议与输入行为不变。本机未编译。


## 2026-11-10 构建版本改为提交时间（待 CI）

版本改为按提交时间（北京时间）生成：`tools/build_version.py`、`tools/package.py`、CI 在构建前写入 `LINKO_VERSION`，GUI 侧栏优先显示该编译期值，规则见 docs/RELEASING.md。本机 Python 3 运行 `test_build_version.py` 与 `test_package.py` 通过；本机无 Rust/Cargo，GUI 与 build.rs 的改动未编译，fmt、严格 Clippy、locked test/build 及 Windows/macOS 打包以精确提交 CI 为准。未做任何物理验收，也不影响 #3/C06 的状态。


## 2026-11-11 GUI 重做 S5：安全对话框改为“图标＋要点”（待 CI）

分支 `gui-redesign-s5`，起点 S1 `025a058`。只改 `desktop_ui.rs` 的“开启键鼠协同”（W3）与撤销／重置／强制停止／停止与恢复（W4）对话框，以及 `worker_permissions.rs` 的 `native_start_header`、`native_start_details`。没有改按钮文字、勾选框文字、启用条件、点击后的行为或任何权限、同意、恢复判定；W3 固定尺寸、头部 112、底部 104 不变。

文字处理只做拆句加图标，不删语义。每条要点不超过 30 个字符，图标用 S0 的自绘 `Icon`，不只用图标表达任何授权或危险信息。对照表摘要（旧句 → 新要点，均保留全部含义）：

- W3 头部“检查不联网、不捕获输入，也不申请系统权限。”原样保留并加锁形图标；头部状态句（`Readiness::detail` 与过期句）原样保留，按状态配对勾（M9 描线）、离散转圈（只随已有权限轮询节拍）、锁形或警告图标。
- W3 详情“由实际输入程序检查……不能代替实际输入程序的许可。”拆为 3 条；“权限观察仅30秒有效……检查通过也不代表对端许可或实际输入效果。”拆为 2 条（30 与 `FRESHNESS` 由测试绑定）。
- W3 说明 6 段拆为 16 条要点（随条件最多同时显示 15 条）：真实捕获与注入、工作进程重新验权、连接/认证不代表对端允许；等待约一分钟可随时停止、重新验权后才捕获、不开启断线自动重连（原条件不变）；接收偏好允许，或不允许加可请求控制已授权本机的对端；仅通信会话先停止、清理成功后再重连且失败不自动继续（原条件不变）；Ctrl+Alt+Right 切换、Ctrl+Alt+Esc 紧急恢复、实机兼容性等仍未验收；接收端首次物理动作只用于收回控制且不执行或回放、切回后先松开按键再操作。
- W4 撤销 1 句拆 2 条；重置 1 句拆 3 条；强制停止 1 句拆 3 条；停止与恢复 3 段拆 10 条。其中“进程报告只表示软件路由与清理请求，不代表OS或应用效果已确认”拆为相邻两条（第二条以“它”指代进程报告）；强制返回的条件、两个步骤和“仍保留未知警告，不证明系统按键已释放”按原顺序分为 4 条。

新增测试：每条要点有非空文字且不超过 30 字符、W3 各条件分组随原条件出现；W3 与 W4 每条要点渲染时文字旁都有图标；1280×800 与 920×700 固定窗口在未检查、检查中、就绪、拒绝、失败、过期六种状态下，头部两行完整可见且位于同意勾选框之上，可见要点不横向越界；W3 打开、就绪对勾描线、关闭，以及恢复对话框打开关闭后，重绘请求归零。原有 W3 布局与同意测试的定位文字未改。

动效：只新增就绪对勾描线（240ms，一次性）；对话框淡入沿用 egui 窗口默认 `fade_in`。窗口在 `dialog` 置空后不再显示，所以没有淡出动画，这一点未改。

未验证：本机没有 Rust/Cargo，未编译、未运行测试、未截图；fmt、严格 Clippy、locked test/build 与截图以 CI 为准。中文换行与图标对齐的实际观感、Windows／macOS 原生外观和高 DPI 未看过。逐条对照表需主会话或独立审查核对。


## 2026-11-11 GUI 重做 S7：动效收尾与重绘预算审计（待 CI）

分支 `gui-redesign-s7`，起点 S1–S6 合并基线 `3b95f94`（`origin/gui-ui-all2`）。按设计规格决策 5，第一期不加“减少动效”开关，也不改桌面偏好存储。本切片只改 `motion.rs`、`desktop_style.rs`、`icons.rs` 和本文档、`GUI_INTERACTIONS.zh-CN.md`；没有改任何界面行为、输入链路、协议、平台 crate 或 `session.rs`。

### 重绘预算审计

预算：单个动效不超过 360ms、一次性、播完后不再请求重绘；动画状态用固定 `Id`，不把变化的文字拼进 `Id`；等待转圈只在已有 100ms 节拍运行时走格，自己不请求重绘。下表全部是读代码的结论（S7 读了每个调用点所在函数与 `motion.rs` 全文），没有运行；其中“已有回归”一栏指 CI 中覆盖该路径的测试。

| 动效 | 调用点（文件：函数） | 时长 | 触发 | 重绘条件 | 是否符合 | 问题／说明 |
|---|---|---|---|---|---|---|
| M1 悬停 | `shell_ui.rs`：`paint_control_surface`（侧栏行、图标按钮）；`desktop_ui.rs`：`segment`；`workspace_ui.rs`：`paint_canvas_button`；`pairing_panel.rs`：`entry_card` | 80ms | 指针进入或离开 | egui 动画表，值在 0 与 1 之间时才重绘；`Id` 为控件 `response.id.with("hover")` | 符合 | 无 |
| M2 选中 | 同上四处，另有 `workspace_ui.rs`：`canvas_screen`（选择环，`Id` 为 `scope_id().with("local-screen"/"peer-screen").with("select")`） | 120ms | 选择变化 | 同上 | 符合 | 无 |
| 详情抽屉展开 | `workspace_ui.rs`：`details_drawer`，`Id` 为 `context_control_id("details-drawer")` | 160ms | 点 ⓘ 开或关 | 同上；关闭到 0 后不再绘制 | 符合 | 规格表没有单列此项，按 M4 的一次性规则审；已有回归 `details_drawer_plays_one_transition_each_way_then_requests_no_repaint` |
| M3 侧栏 | `desktop_ui.rs`：`desktop`（`sidebar_mix`，`Id` 为 `"linko-sidebar"`）；`shell_ui.rs`：`sidebar`（文字过半才显示） | 200ms | 点侧栏开关，或窗口宽度跨过 1100 点 | egui 动画表 | 符合 | 无 |
| M4 页面切换 | `desktop_ui.rs`：`desktop`（`page_motion`，`Retrigger<u8>`，键为 `view_key(page)`） | 160ms | 页面键变化（首次绘制不播） | `OneShot::tick` 只在进度小于 1 时请求重绘 | 符合 | “设备”和“屏幕布局”共用一个键，二者互切不播；透明度不低于 0.35 |
| M5 状态胶囊 | `shell_ui.rs`：`status_capsule`（`capsule_motion`，键为 `view.short`） | 180ms | 胶囊短文字变化 | 同上 | 符合预算 | 与规格触发条件有偏差：规格写“标题或色调变化”，实现只看短文字；短文字不变而标题或色调变了时不重播。不影响预算，只记录，不在本切片改 `shell_ui.rs` |
| M6 角标弹出 | `workspace_ui.rs`：`workspace_canvas`（`local_badge`、`peer_badge`，`Retrigger<Badge>`） | 200ms | 角标种类变化（首次绘制不播） | 同上 | 符合预算 | 与规格触发条件有偏差：规格写“状态从无到有”；`Badge` 没有“无”这一项，实现对任一种类变化都弹一次。只记录 |
| M7 回弹 | `workspace_ui.rs`：`CanvasMotion::retarget`、`remaining` | 220ms | 松手，或方向变化且偏移大于 0.5 点 | 同上 | 符合 | 规格写 `animate_value`，实现用 `OneShot` 缩小两轴偏移，效果等价；已有回归 `released_drag_rebounds_once_then_requests_no_repaint` |
| M8 抖动 | `pairing_panel.rs`：`Feedback::reject`、`shake_offset` | 360ms | 输入端地址或码格式错、码被拒 | 同上 | 符合 | 已有回归 `rejected_code_shakes_once_and_then_stops_repainting` |
| M9 对勾或叉号（配对） | `pairing_panel.rs`：`Feedback::observe`、`glyph` | 240ms | 阶段进入成功或失败（首次观察不播） | 同上 | 符合 | 无 |
| M9 对勾（权限） | `worker_permissions.rs`：`native_start_header`（`animate_flag`，`Id` 为 `"native-start-ready-check"`） | 240ms | 检查结果变为就绪 | egui 动画表 | 符合 | 对话框首次打开时已就绪，直接显示完整对勾 |
| M10 弹窗高度 | `pairing_panel.rs`：`ui`（`dialog_height`，`Id` 为 `"pairing_window_height"`） | 200ms | 目标高度在 520／540／600／620 之间变化 | egui 动画表 | 符合 | 高度另受窗口高度减 48 的上限约束 |
| M11 等待转圈 | `shell_ui.rs`：`paint_capsule_glyph`（等待状态且 `activity_tick_active`）；`workspace_ui.rs`：`workspace_canvas`（同一条件，否则固定第 0 格）；`pairing_panel.rs`：`paint_glyph`（配对子进程轮询中）；`pairing_discovery.rs`：`search_spinner`（查找句柄存在时）；`worker_permissions.rs`：`native_start_header`（检查中） | 不定 | 等待类状态 | 只读 `input.time`，从不请求重绘；后三处显示的条件分别等于 `pairing_panel.needs_poll`、`discovery.needs_poll`、`worker_permissions.needs_poll` 为真，而这几项都在 `activity_tick_active` 里 | 符合 | 无 |
| M12 列表项进出 | `pairing_discovery.rs`：`sync_rows`、`hint_row` | 180ms | 发现结果增减 | 每行一个 `OneShot`；离开的行播完即删；最多 320 行；停止查找时清空 | 符合 | 不使用 egui 动画表，所以不增加 `Id` |

结论：14 项全部满足时长、一次性、结束后零重绘、固定 `Id` 和转圈只随已有节拍这五条，没有发现违反预算的地方，本切片没有修改任何调用点。另有两处与规格触发条件不一致（M5、M6），不影响预算，只记录。

其他从代码看到的情况：

- 同时播放数：单次状态变化最多同时触发胶囊 M5 和两个角标 M6；配对失败最多同时触发 M9 叉号、M8 抖动和 M10 高度。都不超过 3 个，最长 360ms。这是读代码推断，没有运行计时。
- egui 动画表（0.37 `animation_manager.rs`，WebFetch 读源码）只插入不清理。本应用写入该表的 `Id` 要么是固定名字，要么是控件按布局顺序得到的 `response.id`，数量随界面结构有上限，不随时间或文字增长。
- `motion::animate_flag` 和所有 `OneShot` 都用设计令牌里的固定时长，不读 egui 的 `Style::animation_time`。所以把 `animation_time` 设为 0 只影响 egui 自带的效果（例如窗口淡入），不会让本应用的动效变成瞬时。`scaled_secs` 是留给以后“减少动效”开关的入口，目前所有调用都传 1。
- 不属于动效、但会请求重绘的现有路径：`main.rs` 的 100ms 活动节拍；权限观察 30 秒到期时请求一次重绘；退出检查、资源检查这两种隔离测试模式里的 50ms 轮询；手势画布绘制中的 `request_repaint`。这些都在 S0 之前就有，本切片没有改。

### 新增回归测试（`motion.rs` 的 `shell_budget_tests`）

两个测试都用 `crate::tests::Fixture` 加载真实 `App`，每帧按 eframe 的顺序依次调用 `logic_tick`、`desktop`、`request_activity_tick`，时间按 60Hz 推进，窗口有焦点、不注入指针。由于本切片不能改 `main.rs`，测试放在 `motion.rs` 里，通过 crate 内可见的 `App` 私有方法和 `crate::lifecycle_tests::managed_child` 驱动。

- `idle_workspace_has_no_periodic_repaint`：空闲（未登记）、已登记离线、稳定仅通信（受管测试子进程，工作进程快照为已认证，断言 `connected` 为真）三种状态，各先稳定 60 帧，再跑 100 帧，断言没有延迟小于 1 秒的重绘请求。灵敏度：同一窗口改为 Native 模式跑 5 帧，断言计数大于 0，证明计数器能看到已有的 100ms 节拍。
- `animation_time_zero_is_functionally_identical`：egui 默认动画时长与 `animation_time = 0` 各跑一遍四条主路径，结果逐项相等，并等于预期值：点画布空位“＋ 添加另一台电脑”打开真实配对窗口；“开启键鼠协同”对话框显示同意勾选框文字，旧的勾选在没有新鲜权限检查时被清掉，对话框保持打开；标题栏“停止协同”向受管子进程发出停止；把另一台屏幕拖到左边后内存和 `settings.json` 的方向都为 `left`。全程不创建身份、不打开键鼠或接收许可。注意上一条所说的限制：这个测试证明的是 egui 自带动画为 0 时功能不变，不能证明本应用动效时长为 0 时的表现。

按 CLAUDE.md 的红绿要求，这两个测试应先在破坏被测行为时失败（例如让 `request_activity_tick` 在仅通信时也请求 100ms 重绘）。本机没有 Rust，这一步没有做，也没有运行过这两个测试；灵敏度目前只靠 Native 一段的正向断言。

### 去除的 `allow(dead_code)`

逐项用全文搜索确认在非测试代码里有调用后去除：`SHAKE_KNOTS_PX`、`OneShot::is_active`、`animate_scalar`、`expand_amount`、`dialog_height`、`shake_offset`（`motion.rs`）；`SHAKE_SECS`、`STROKE_DRAW_SECS`、`RADIUS_CHIP`、`RADIUS_CARD`、`surface_color`（`desktop_style.rs`）；`paint_check_progress`、`paint_cross_progress`（`icons.rs`）。

保留并在代码旁写明原因：`linear`、`Retrigger::progress`、`Glide`、`presence_amount`、`sidebar_width`、`stroke_progress`、`MAX_INTERACTION_SECS`、`RADIUS_MENU`、`RADIUS_PILL`、`text_color`、`canvas_color` 只在测试中使用；`Icon` 枚举的 `Cross` 变体在测试之外没有被构造（配对窗口用 `paint_cross_progress` 逐笔绘制叉号）。

### 已知遗留（本切片不修）

1. 屏幕上下排列时，画布上的落点区只有约 20 点宽（S2 汇报），拖到上下方向不易命中。
2. `desktop_ui.rs` 有 17 行、`pairing_panel.rs` 有 20 行超过 100 宽（中文按双宽计），集中在高级登记向导和配对窗口 `ui` 等 rustfmt 跳过的旧写法里；格式检查不会因此失败，但不易审读。
3. 对话框关闭没有淡出：`dialog` 置空后窗口当帧消失（S5 已记录）。
4. M5、M6 的触发条件与规格不一致，见上表。
5. 界面字符串仍有约 40 行使用“对端”（含测试中断言的同一文字）（`desktop_state.rs`、`desktop_ui.rs`、`main.rs`、`worker_permissions.rs`、`workspace_ui.rs`、`pairing_supervisor.rs` 等），与术语表“另一台”不一致。其中 W3、W4 安全要点和权限说明属于 K 类文字，改动需要逐条对照表；本切片只在文档中标注。
6. 资源对照（规格 3.4 第 4 项）：参见下文 PR114 的空闲与仅通信资源对照。

未验证：本机没有 Rust/Cargo，未编译、未运行任何测试、未截图；fmt、严格 Clippy、locked test/build、截图与内存工作流以本分支的 CI 为准。视觉效果、动效的实际观感、Windows／macOS 原生外观和高 DPI 都没有看过。

## 2026-11-11 PR114 CI 截图审阅与 GUI 资源对照

候选为 PR114 的 `2a78ccf`。三平台构建、最低 Rust、依赖审计、固定上游资格与两平台资源检查共 8 项已通过。两端包为 [Windows](https://github.com/ForceMind/Linko/actions/runs/43024893167/artifacts/13856104900) 与 [macOS](https://github.com/ForceMind/Linko/actions/runs/43024893167/artifacts/13856759372) 的 Actions 产物。[Rust CI](https://github.com/ForceMind/Linko/actions/runs/43024893167) · [上游资格](https://github.com/ForceMind/Linko/actions/runs/43024893044) · [资源工作流](https://github.com/ForceMind/Linko/actions/runs/43024893161)

### 截图复查及任务清单

已逐张查看 [GUI 截图产物](https://github.com/ForceMind/Linko/actions/runs/43024893167/artifacts/13857182180) 的 4 张 1280×800 原图，以及同提交资源作业的 [仅通信工作区](https://github.com/ForceMind/Linko/actions/runs/43024893161/artifacts/13857226612) 标准与 920×700 紧凑图。均来自 Linux Xvfb。

| 截图 | 已观察到 | 待处理或补证 |
| --- | --- | --- |
| `peerglide-home.png` | 设备侧栏、画布、上下文栏和恢复按钮均可见。本机名称和状态分布在侧栏、画布、上下文栏与下方设备卡。 | 减少首页重复的设备详情，核对保留详情入口和未登记主操作。 |
| `peerglide-add-device.png` | 显示码、输入码和独立权限说明可见。关闭入口同时出现在标题栏与内容底部，“尚未开始”单独占一行。 | 收敛重复入口和初始态占位，逐条核对关闭、取消及配对说明。 |
| `peerglide-settings.png` | 三个设置分组可见，Linux 托盘选项禁用并解释原因。深色选项月牙处有越过控件边缘的细线。 | 用干净的 Linux 复拍区分指针残影、截图过程和绘制问题，再决定修复位置。 |
| `peerglide-gesture-lab.png` | 显示录制说明、轨迹、识别结果和样本表单。截图停在滚动后的录制区，返回箭头不在可见区域，样本目录长路径只显示一部分。 | 核对滚动后返回入口及长路径查看方式，保留本窗口测试、手动保存与不上载的提示。 |
| `peerglide-workspace-connected.png` | 标题显示已连接，上下文明确键鼠尚未开启。标题栏有停止协同，上下文栏有断开。画布已锁定。 | 核对仅通信状态下停止与断开的作用及入口，继续检查两者在原生运行和恢复状态的表现。 |
| `peerglide-workspace-connected-compact.png` | 折叠侧栏、四行上下文和停止、恢复入口可见。下方设备卡的底部在可视区之外，页面有滚动条。 | 复核最小窗口内的可见信息和详情卡重复，补拍高 DPI。 |

本次截图覆盖未登记、配对入口、常规设置、窗口内手势实验及仅通信。配对码／回执过程、开启键鼠、撤销、强制停止、恢复未确认及上下排列仍需补图。静态截图未验收动效、Windows/macOS 原生外观、高 DPI 或 #3/C06。

### 资源对照

[基线 `130cd8e` 的工作流](https://github.com/ForceMind/Linko/actions/runs/42317164367) 与候选 `2a78ccf` 的报告均已读取。表内数值为 GUI 进程的平均 CPU（单逻辑 CPU 为 100%）和采样峰值 RSS（MiB）。

| 平台和场景 | 基线 CPU / 峰值 RSS | 候选 CPU / 峰值 RSS | 候选预热后最大 RSS 增长 |
| --- | ---: | ---: | ---: |
| macOS 空闲 180 秒，空身份 | 0.594% / 114.594 | 1.056% / 109.234 | 0.250 MiB |
| macOS 空闲 180 秒，已有身份 | 0.300% / 114.484 | 0.461% / 109.266 | 0 MiB |
| Ubuntu 空闲 180 秒，空身份 | 0.000% / 131.488 | 0.000% / 150.316 | 0 MiB |
| Ubuntu 空闲 180 秒，已有身份 | 0.000% / 132.535 | 0.000% / 149.707 | 0 MiB |
| macOS 仅通信约 30 秒，普通窗口 | 0.764% / 119.078 | 1.397% / 110.875 | — |
| Ubuntu 仅通信约 30 秒，普通窗口 | 0.000% / 134.793 | 3.334% / 152.426 | — |
| macOS 仅通信约 30 秒，AppKit 隐藏 | — | 0.366% / 118.359 | — |

- 交接提到的 0.76% / 119 MiB 对应 macOS 30 秒仅通信场景。同口径下，候选 CPU 增加约 0.633 个百分点，峰值 RSS 下降 8.203 MiB。Ubuntu 空闲候选峰值 RSS 比基线高约 17–19 MiB。
- 两平台的 180 秒空闲作业均通过 RSS 512 MiB、预热后增长 64 MiB 两道门槛，工作流阈值未改。Linux 的 0.000% 是进程 CPU 时间在采样精度内未跨计数刻度。两轮独立 runner 的单次差值只记录为观察值。
- 候选三次 NoNative 分账均确认 worker 回收。按 macOS 普通、Ubuntu 普通、macOS 隐藏的顺序，worker / helper 峰值 RSS 分别为 11.828 / 11.938、7.641 / 7.734、11.875 / 11.859 MiB。AppKit 隐藏场景验证了程序化隐藏、恢复显示与停止。短时采样未覆盖真实键鼠会话、8h/24h 耐久或旧 6.83 GB 事故。

### “对端”文案逐条对照

基于 `2a78ccf` 的源文件行号。34 处面向界面或在界面展示的诊断文案，另有 7 条文字断言同步更新。设置页复用开启确认的同一条权限过期说明。隔离资源检查的命令行启动错误仍用原文。开启同意、撤销、重置、强制停止和恢复状态的判断逻辑保持原样。

| 位置 | 原句 | 修改后 | 核对 |
| --- | --- | --- | --- |
| `desktop_state.rs:138` | 登记等待对端回执 | 登记等待另一台回执 | 登记状态 |
| `desktop_state.rs:273` | 上次切换未被对端接受 | 另一台未接受上次切换 | 切换被拒 |
| `desktop_state.rs:285` | 对端未提供鼠标能力 | 另一台未提供鼠标能力 | 缺少能力 |
| `desktop_state.rs:286` | 对端不可用 | 另一台不可用 | 警告胶囊 |
| `desktop_state.rs:300` | 请观察对端是否响应。Ctrl+Alt+Esc 可紧急恢复本机控制。 | 请观察另一台是否响应。Ctrl+Alt+Esc 可紧急恢复本机控制。 | 保留观察指引和紧急恢复快捷键 |
| `desktop_ui.rs:66` | 已获准向对端发送输入 | 已获准向另一台发送输入 | 保留已获准和发送方向 |
| `desktop_ui.rs:67` | 对端正在控制本机 | 另一台正在控制本机 | 接收状态 |
| `desktop_ui.rs:792` | 对端实际 IP 与端口（请勿照抄示例） | 另一台的实际 IP 与端口（请勿照抄示例） | 保留使用实际地址的提醒 |
| `desktop_ui.rs:793` | 填写对端实际 IPv4:24870 | 填写另一台的实际 IPv4:24870 | 地址和端口示例 |
| `desktop_ui.rs:814` | 对端有多个网卡：选择与你同一局域网的地址 | 另一台有多个网卡，请选择与本机同一局域网的地址 | 保留同一局域网条件 |
| `desktop_ui.rs:827` | 对端公钥证书文件路径 | 另一台的公钥证书文件路径 | 证书路径 |
| `desktop_ui.rs:833` | 本机等待连接；对端需选择连接本机。 | 本机等待连接，另一台需选择连接本机。 | 保留连接发起方 |
| `desktop_ui.rs:842` | 对端证书BLAKE3指纹 | 另一台的证书 BLAKE3 指纹 | 保留指纹算法 |
| `desktop_ui.rs:845` | 尚未读取有效的对端公开证书。连接信息不是短码配对，也不自动授信。 | 尚未读取另一台的有效公开证书。连接信息不是短码配对，也不自动授信。 | 保留有效证书、独立配对和不自动授信 |
| `desktop_ui.rs:858` | 我已在对端独立核对该指纹，完全一致 | 我已在另一台独立核对该指纹，完全一致 | 保留独立核对和完全一致 |
| `desktop_ui.rs:886` | 允许此对端把键鼠输入发送到本机 | 允许另一台把键鼠输入发送到本机 | 保留键鼠接收授权 |
| `desktop_ui.rs:887` | 不能代授对端接收本机控制。剪贴板/文件尚未提供，默认不开放。 | 不能替另一台授权本机控制它。剪贴板/文件尚未提供，默认不开放。 | 保留双向独立授权和数据能力关闭 |
| `desktop_ui.rs:1621` | 连接/认证不代表对端允许本机控制。 | 连接/认证不代表另一台允许本机控制。 | 开启确认，连接和认证不等于控制许可 |
| `desktop_ui.rs:1640` | 本机接收偏好：允许已登记对端控制本机。 | 本机接收偏好：允许已登记的另一台控制本机。 | 开启确认，保留已登记前提 |
| `desktop_ui.rs:1645` | 本机接收偏好：不允许对端控制本机。 | 本机接收偏好：不允许另一台控制本机。 | 开启确认，明确本机拒绝接收控制 |
| `desktop_ui.rs:1649` | 可请求控制已授权本机的对端。 | 可向已授权本机的另一台请求控制。 | 开启确认，保留反向请求的授权前提 |
| `desktop_ui.rs:1714` | 停止会话并清除对端信任与本机接收许可。 | 停止会话，撤销本机对另一台的信任及本机接收许可。 | 撤销确认，保留停止、撤销信任和清除接收许可 |
| `layout_updates.rs:59` | 两台电脑的位置不一致（对端认为本机在其{peer}） | 两台电脑的方向设置不一致（另一台将本机设在{peer}） | 保留另一台所设方向 |
| `main.rs:704` | 本机登记尚待对端回执；请重新核验或撤销后再连接 | 本机登记还在等待另一台回执，请重新核验或撤销后再连接 | 保留重验或撤销后才能连接 |
| `main.rs:883` | 请输入对端 IP 地址和端口 | 请输入另一台的 IP 地址和端口 | 地址校验提示 |
| `main.rs:1209` | 等待对端连接 | 等待另一台连接 | 监听状态 |
| `pairing_receipt.rs:104` | 配对收据与当前本机或对端身份不一致 | 配对收据与本机或另一台的当前身份不一致 | 两端身份绑定 |
| `pairing_supervisor.rs:303` | 本次已确认，请等待对端 | 本次已确认，请等待另一台 | 保留本机已确认 |
| `pairing_supervisor.rs:323` | 本机可能已保存，对端回执未知 | 本机可能已保存，尚不清楚另一台是否回执 | 保留本机可能保存和回执未知 |
| `pairing_supervisor.rs:335` | 保存通知未送达，对端回执未知 | 保存通知未送达，尚不清楚另一台是否回执 | 保留通知未送达和回执未知 |
| `resource_check.rs:164` | 隔离通信对端 | 另一台（隔离通信） | 仅 CI 的可见设备名称 |
| `worker_permissions.rs:25` | 上次结果已过期，开启前请重新检查。检查通过也不代表对端许可或实际输入效果。 | 上次结果已过期，开启前请重新检查。检查通过不代表另一台已许可，也不代表实际输入有效。 | 开启确认头部和设置页，保留过期重查及两项边界 |
| `worker_permissions.rs:54` | 检查通过也不代表对端许可或实际输入效果。 | 检查通过不代表另一台已许可，也不代表实际输入有效。 | 开启确认详情，保留两项边界 |
| `workspace_ui.rs:223` | 等待对端回执 | 等待另一台回执 | 工作区登记状态 |

## 后续输入审计

F02 的回归覆盖主屏四边、亚像素位置、另一轴越界和非有限位置，以及
副屏位置不能重新武装。四边真实跨屏与多屏环境保留至集中验收。

F06 在捕获队列前过滤 0／0 滚轮。接收侧物理接管、目标能力检查与 Windows
滚轮余量沿用现有逻辑。新的 `scr_zero` 计入当前回调归属为发送远端的
零增量，表示已跳过。worker 仍在发送前检查零增量。新增回归覆盖本机
放行和远端吞掉、满队列时保留有效滚轮及当前控制归属。原生手感待集中验收。

F18 将 QUIC 运动数据报的发送缓冲设为 16 KiB。锁定的 quinn-proto 0.11.19
默认值为 1 MiB，缓冲不足时由 Quinn 淘汰较旧数据报。运动仍是带序号的
v5 绝对位置，可靠的按键、滚轮、释放和停止流程使用原有流。回环检查
覆盖传输兼容性，真实链路上的延迟与丢包表现留到集中验收。

F01 接收端每轮最多检查 32 个已就绪数据报，只把序号最高的有效位置送去
注入。坏包、无效租约和重放分别计数，`rx_coalesced` 表示同批次合并掉
的有效旧位置。KVM-DIAG/5 的读取同时接受旧摘要和带该字段的新摘要。有效
收包仍满足 `rx = decode + lease + replay + apply + retired + apply_fail + rx_coalesced`
的分类关系。原生指针命令与恢复等待沿用现有路径，真实延迟留到集中验收。

F09 发送端只保留一份待发绝对位置，两次发送间隔至少 4 ms。待发位置
替换计为 `tx_coalesced`，空闲无运动定时唤醒。发送前重新核对控制租约、
捕获代次和停止状态。可靠输入和边缘释放携带自己的最新光标锚点，可
直接清除其覆盖的待发位置。v5 线格式不变。实测延迟留到集中验收。

F10 的按键和按钮账本使用覆盖完整本地编码范围的固定容量位图。启动
时系统采样仍在钩子安装前，已有配对归属继续走原有账本。新增分配计数
回归覆盖重复按下、启动别名、未知键编码及对应释放。捕获队列的分配
另行处理。

F10 的捕获队列复用现有预分配 SPSC 环形队列，容量仍为 128。原生回调只写
队列，由原事件循环在回调返回后唤醒异步消费者。新增回归检查满队列、
零分配、回调期间不唤醒、循环唤醒与关闭唤醒。Windows 和 macOS 仍使用
各自唯一的捕获生命周期，输入手感留到集中验收。

## 桌面布局与动效

紧凑上下布局为两侧保留可达落点，回归用 270×180 点画布验证上下
互换各有至少 32 点可用距离。胶囊观察标题和色调，角标只在连接、
控制或恢复未确认等重点状态从缺席变为出现时弹出。切换已有状态、
首次绘制和回到静态状态不重播，旋转提示仍随原有活动节拍。真实
平台的布局与动画观感留到集中验收。


## G1 受控 worker

复用原 PR40 的识别、规则与候选所有权核心。当前分支先在各平台运行纯
worker 核心检查，覆盖原应用、DPI、原期限、误触拒绝、配置更新与归还
候选。此切片不启动原生捕获或动作。下一切片接入 Windows 库级 worker，
产品入口仍关闭，真机行为留到集中验收。

Windows 库级 worker 接回现有原生捕获和当前预分配队列。规则构造不会
创建捕获或授予权限。加入原始 DPI 绑定、未退休序列阻止准入覆盖、
失焦／权限／停止回收及对应软件回归。产品入口仍保持关闭；本机 OS
动作与短右键实际效果留到集中验收。

## 2026-11-12 数据协作草稿验证

PR156 审阅提交为 `2e5b28c84ea18d97bb22579c086994dca1421b4c`。
Actions 实际检出的合成提交为 `3dcb1d8bcc0198aad428c72d5b6dfaf90857aec2`，
两者的源码树均为 `9bddd35c9a1946e3b008b0375123cbdac19acc47`。
该次 Windows、macOS、Ubuntu 格式检查、严格 Clippy、locked 工作区
测试、构建和平台冒烟已通过。最低 Rust、依赖审计、固定上游资格及 macOS/Ubuntu 资源检查通过。
[三平台 CI](https://github.com/ForceMind/Linko/actions/runs/43102482924) ·
[固定上游资格](https://github.com/ForceMind/Linko/actions/runs/43102483061) ·
[资源报告](https://github.com/ForceMind/Linko/actions/runs/43102482967)

已读 Ubuntu 用例日志。两端回环覆盖文本独立许可、本机确认后导入、
停止使待导入票据失效、文件分块准入、逐条同意、拒绝不写盘、完整文件
提交、重新连接恢复、停止保留暂存、节流和只读历史。PNG 用例覆盖
尺寸／大小边界、坏帧、动画拒绝、像素保留与移除文本元数据。图片目前
仅有校验库，尚无图片网络或系统剪贴板入口。

| 采样场景 | 平均 CPU | 峰值 RSS | 预热后最大增长 |
| --- | ---: | ---: | ---: |
| macOS 空身份空闲 180 秒 | 1.150% | 109.188 MiB | 0.281 MiB |
| macOS 已有身份空闲 180 秒 | 0.539% | 109.188 MiB | 0 MiB |
| Ubuntu 空身份空闲 180 秒 | 0.000% | 146.027 MiB | 0 MiB |
| Ubuntu 已有身份空闲 180 秒 | 0.000% | 146.098 MiB | 0 MiB |
| macOS 普通窗口仅通信约 30 秒 | 2.223% | 111.250 MiB | — |
| Ubuntu 普通窗口仅通信约 30 秒 | 3.334% | 149.242 MiB | — |
| macOS 隐藏窗口仅通信约 30 秒 | 0.433% | 118.156 MiB | — |

空闲报告保留 RSS 512 MiB 与预热后增长 64 MiB。0.000% 表示采样
精度内未跨过进程 CPU 时间计数刻度。与上述同口径 macOS 普通窗口
仅通信基线相比，CPU 高约 1.459 个百分点，峰值 RSS 低 7.828 MiB。
三次仅通信进程检查均确认回收 worker。这里记录短时、单次 runner
观测，未测原生输入或文件传输期间的延迟和资源。

该次 [Windows 包](https://github.com/ForceMind/Linko/actions/runs/43102482924/artifacts/13892823980)
和 [macOS 包](https://github.com/ForceMind/Linko/actions/runs/43102482924/artifacts/13892734026)
已由 CI 产出。真实 Windows/macOS 系统剪贴板、局域网文件、混合 DPI、
大文件并行键鼠延迟及 #3/C06 仍待最终同源候选的统一验收。

### F15 设备插图资源对照

审阅提交 `b0fa5ba` 将解码像素移交给纹理管理器，应用只保留每个
GUI 上下文的纹理句柄、裁剪坐标和比例。GitHub Actions 的三平台
回归通过，确认纹理复用和画布几何。

| 空闲 180 秒 | 调整前平均 CPU / 峰值 RSS | 调整后平均 CPU / 峰值 RSS |
| --- | ---: | ---: |
| macOS 空身份 | 0.961% / 108.969 MiB | 1.116% / 115.734 MiB |
| macOS 已有身份 | 0.572% / 108.734 MiB | 0.505% / 109.781 MiB |
| Ubuntu 空身份 | 0.000% / 148.531 MiB | 0.000% / 141.926 MiB |
| Ubuntu 已有身份 | 0.000% / 149.168 MiB | 0.000% / 142.223 MiB |

两平台预热后增长不超过 0.266 MiB，原 512 MiB／64 MiB 门槛保留。
Ubuntu 本次峰值低约 6.6–6.9 MiB，macOS 空身份本次峰值较高。
这些是独立 runner 的短时观察，纹理驱动和启动峰值仍计入 RSS。
[调整前报告](https://github.com/ForceMind/Linko/actions/runs/43102956952) ·
[调整后报告](https://github.com/ForceMind/Linko/actions/runs/43110989149) ·
[工作区 CI](https://github.com/ForceMind/Linko/actions/runs/43110989136)

## 2026-11-12 集成草稿的同源包证据

PR180 审阅提交 `7916b5e4eada6fe8580eb164fd4943ba0cb83b12` 的
[三平台 CI](https://github.com/ForceMind/Linko/actions/runs/43113699213)
通过。读出两个便携 ZIP 的 `BUILD_SOURCE.json`，同为
`2026.11.12-0640`，实际检出 `f359802b2bd47f80c7690756bf4e4946b4c229b2`，
源码树 `9f0e81bf3fc04197ef4e1136f2e5acbc2c40dc89` 与审阅提交一致。
两份内层 ZIP SHA-256 均与各自同名摘要一致。
[Windows](https://github.com/ForceMind/Linko/actions/runs/43113699213/artifacts/13898495834) ·
[macOS](https://github.com/ForceMind/Linko/actions/runs/43113699213/artifacts/13898306373)

随后加入音频协议、F15 与便携回滚的集成提交 `cfdeb3e` 的
[三平台 CI](https://github.com/ForceMind/Linko/actions/runs/43116253905)
通过。便携回滚在 runner 隔离目录验证。实际系统音频、原生拖放、
多设备路由、安装和 #3/C06 按最终候选记录。

### 集成版本 2026.11.12-0900

[三平台 CI](https://github.com/ForceMind/Linko/actions/runs/43131489508) 的审阅
提交为 `81a5f6253ba70731a3ce6449c92aea342f9907d1`。已读取两份便携包，
`BUILD_SOURCE.json` 的实际检出为 `52d6b7174fdc941c351c0b040abee3c4c3fccbe7`，
源码树为 `5a222668e12c899db706f201e6f0015fc2cb3b3e`，与审阅提交树一致。
内层 ZIP 的实算 SHA-256 分别等于各自摘要。

| 包 | SHA-256 |
| --- | --- |
| [Windows x64](https://github.com/ForceMind/Linko/actions/runs/43131489508/artifacts/13905499578) | `11c609e7e61e247690a69c7901191c392ed90307950152e62ea9556c9492c9f7` |
| [macOS arm64](https://github.com/ForceMind/Linko/actions/runs/43131489508/artifacts/13906522740) | `e3326478b8f18bf461fc8d0b8fc36455522f83ebf11053add3e32162707ead83` |

此记录用于阶段证据，统一验收以最后冻结的匹配包为准。

### F16 中文字重资源对照

基线 `d4c872b` 使用同一静态字体字节及 400、550、650 三个实例，试验
`2bb9e9f` 采用 400 和 600 两个实例。两份 180 秒资源报告均通过原有
512 MiB 峰值和 64 MiB 增长门槛。表内为峰值 RSS（MiB）。

| 平台和配置 | 三个实例 | 两个实例 |
| --- | ---: | ---: |
| macOS 空身份 | 107.797 | 113.297 |
| macOS 已有身份 | 107.844 | 107.578 |
| Ubuntu 空身份 | 140.844 | 140.406 |
| Ubuntu 已有身份 | 140.582 | 140.227 |

隔离截图核对了首页、添加电脑、设置和滚动后的手势实验页。紧凑窗口的标题、
配对操作和返回入口完整可见。各平台这次短测的变化方向不一致，候选保留原有三个
字重及完整中文字库，真实长时字体资源仍需观察。
[基线资源](https://github.com/ForceMind/Linko/actions/runs/43123212220) ·
[试验资源](https://github.com/ForceMind/Linko/actions/runs/43123965980) ·
[试验截图及 CI](https://github.com/ForceMind/Linko/actions/runs/43123965950)

### 原生捕获模块拆分

F14 将配对账本和 `HookState` 先后移入独立模块。两次分别通过
[账本 CI](https://github.com/ForceMind/Linko/actions/runs/43112553592)和
[`HookState` CI](https://github.com/ForceMind/Linko/actions/runs/43138743507)。
原生逻辑通过现有配对、恢复和平台回归。F19 的系统显示与光标查询成本及 F20 的
macOS 循环唤醒成本仍需真实输入测量。

### 集成 `244fad7` 资源复核

[两平台资源检查](https://github.com/ForceMind/Linko/actions/runs/43139144851)通过。
180 秒空身份／已有身份峰值 RSS 分别为 macOS 108.641／108.625 MiB、
Ubuntu 140.281／140.262 MiB，观测到的最大暖机后增长为 240 KiB。
门槛沿用峰值 512 MiB、增长 64 MiB。

macOS 可见窗口的 NoNative 仅通信约 30 秒，GUI 平均 CPU 1.232%、峰值 RSS
110.344 MiB。原始对照为 0.764%、119.078 MiB。本次报告仅代表隔离短测。

### 减少界面动效

[三平台 CI 与隔离截图](https://github.com/ForceMind/Linko/actions/runs/43139145128)
通过。标准和紧凑设置页都可见“减少界面动效”。现有回归对比默认与开启减少动效后
的添加电脑、启动确认、停止协同和布局修改，操作结果一致。设置保存在桌面偏好中。
真实 Windows/macOS 外观与键盘可访问性留到集中验收。

### Windows CLI 启动栈

[Windows 无设备探测](https://github.com/ForceMind/Linko/actions/runs/43146799475/job/130946399000)
在 `--version` 复现调试版主线程栈溢出。此前配对测试卡在子进程进入监听前。
解析命令后按实际子命令分配 Future，副屏确认只在显式启用时分配，原生恢复与原测试期限保留。
[Windows 启动与配对回归](https://github.com/ForceMind/Linko/actions/runs/43148511574/job/130951925339)
通过 `--version` 和 `cargo test --workspace --locked` 的实际配对子进程用例。
副屏独立分支的回移版 `9371022` 也通过[完整 CI](https://github.com/ForceMind/Linko/actions/runs/43151157745)。

### 集成 `6debc15` 的同源包与资源复核

[三平台 CI](https://github.com/ForceMind/Linko/actions/runs/43149399517)及
[两平台资源检查](https://github.com/ForceMind/Linko/actions/runs/43149399520)通过。
下载了 `2026.11.12-1136` 的 Windows amd64 和 macOS arm64 包。两个包的来源清单
均指向审阅提交 `6debc15` 和检出树 `bb506705c7798c9f5b86b768914d9cad05c67a29`。
两个内层 ZIP 的 SHA-256 分别与各自发布的校验文件一致。

180 秒空身份／已有身份峰值 RSS 分别为 macOS 107.891／107.672 MiB、
Ubuntu 142.734／142.047 MiB。观测到的最大暖机后增长为 304 KiB。
峰值门槛 512 MiB，增长门槛 64 MiB。macOS 可见窗口的 NoNative 仅通信约 30.13 秒，
GUI 平均 CPU 1.626%、峰值 RSS 109.531 MiB。原始对照为 0.764%、119.078 MiB。
这些数值来自隔离 CI 短测。

### 显示器变化与键盘取消

本机布局变化或现有布局无法核对时，当前原生会话进入原有释放和恢复流程。
布局的增删、主屏变更、位置、缩放和旋转回归已通过
[安全退出 CI](https://github.com/ForceMind/Linko/actions/runs/43152806734)。
已启用自动重连的主屏会话只在清理确认后重连原有证书所绑定的电脑，开始新的捕获前
重新核对布局。再次自动跨边前要求光标先离开切换边，沿两块本机屏幕的共用边移动不算离开。
显式副屏需要重新确认。[主屏重建 CI](https://github.com/ForceMind/Linko/actions/runs/43154571778)
覆盖重连条件、共用边和边缘重新武装，热插拔仍待实际设备验证。

安全确认窗和旧版添加向导支持 Esc 取消，未提交的键鼠确认随窗口清除。
关闭恢复详情保留恢复阻止状态，`Ctrl+Alt+Esc` 继续执行紧急停止。
配对面板继续使用原有取消流程。[键盘取消 CI](https://github.com/ForceMind/Linko/actions/runs/43153704868)

### 集成 `8a3d32c` 的两端包与资源复核

`2026.11.12-1226` 的 Windows amd64 和 macOS arm64 产物均已下载核验。两包的
来源清单对应审阅提交 `8a3d32c` 和检出树 `cf312014ba831f7a667b9c55c6e16a6f81fb9789`，
内层 ZIP SHA-256 与各自校验文件一致。[三平台 CI](https://github.com/ForceMind/Linko/actions/runs/43154782953)

180 秒空身份／已有身份峰值 RSS 分别为 macOS 107.891／107.750 MiB、
Ubuntu 141.801／141.832 MiB。最大暖机后增长 272 KiB，门槛仍为 512 MiB 和 64 MiB。
macOS 约 30 秒的可见窗口 NoNative 测量为平均 CPU 1.398%、峰值 RSS 109.406 MiB，
原始对照为 0.764%、119.078 MiB。[两平台资源检查](https://github.com/ForceMind/Linko/actions/runs/43154782934)

### 分段发送与接收确认

`probe-route` 按当前屏幕快照和版本化分段布局选择唯一证书，使用 v5 可靠流核对该电脑。
三身份回环覆盖相邻分段、共用边屏蔽、旧快照、错误证书、离线目标和重叠配置。
[分段认证探测 CI](https://github.com/ForceMind/Linko/actions/runs/43158320271)

原生双机与串行监听在正常结束且确认本机输入已恢复时，使用同一 QUIC 连接发出正常
关闭回执。隔离回环检查恢复成功、恢复未确认、异常和仅诊断状态对应的关闭说明。
[原生清理确认 CI](https://github.com/ForceMind/Linko/actions/runs/43159083704)

原生 `--remote-entry-edge` 独立选择另一台的进入边，支持正交出入边。自动回程沿进入边，
本机恢复仍沿本机出边。默认四边保持既有位移。[独立进出边 CI](https://github.com/ForceMind/Linko/actions/runs/43159714722)

`connect-routes --native` 使用同一个原生捕获生命周期，按当前外沿分段逐次连接固定
证书。目标能力在确认租约时发布，回程归还到原出发显示器。普通关闭、超时或不匹配的
回执会阻止继续发送。分段角点同时匹配两条路由时保留本机控制。
[捕获 CI](https://github.com/ForceMind/Linko/actions/runs/43163016016)覆盖能力替换和既有捕获代次。
[发送 CI](https://github.com/ForceMind/Linko/actions/runs/43164449827)的三身份隔离回环检查
释放报文、两种可靠流结束顺序、损坏帧和同连接回执。该回环使用模拟的恢复终态。
物理三机切换、混合 DPI、真实按键归属、屏幕热插拔及缺失回执时的设备行为由 #3/C06 验收。

分段发送可按目标请求接收端已选副屏。预留绑定到当前输入连接、固定证书、租约、方向和
位置。拒绝时保持本机控制。隔离回环检查确认后发送接管请求以及拒绝时无请求。
[分段副屏 CI](https://github.com/ForceMind/Linko/actions/runs/43167517220)

桌面编辑器可维护多个核验过的目标、选本机屏幕、分别设置两端边沿范围，并导出 CLI
布局文件。新增回归覆盖重叠拒绝、导出保留已有文件、证书替换拒绝及固定指纹保留。
[分段编辑 CI](https://github.com/ForceMind/Linko/actions/runs/43170772848)
桌面分段发送沿用权限确认、停止与恢复终态。工作进程在捕获前重新核对确认过的草稿、
本机身份和每台目标的证书指纹。回归覆盖草稿替换拒绝、启动前停止和分段待机呈现。
[工作进程草稿](https://github.com/ForceMind/Linko/pull/258) · [桌面启动草稿](https://github.com/ForceMind/Linko/pull/259)
Windows/macOS 实际编辑、物理切换和副屏定位待验收。

### 本机光标定位与拖尾

Windows/macOS 常规设置提供「定位光标」和默认关闭的「光标拖尾」。定位环保持 900 毫秒，
拖尾只保留最近 300 毫秒、最多 10 个采样。减少动效时使用静态定位。远控、恢复、隐藏
窗口时停止绘制。覆盖层通过现有平台接口读取本机光标，采样在 GUI 工作线程执行。
[光标草稿](https://github.com/ForceMind/Linko/pull/260)的回归覆盖默认值、过期、旧视口失效、
负坐标换算与非激活穿透配置。真机检查混合 DPI、光标跨屏、点击穿透、透明效果和焦点。

### 本机分段布局投影

从分段编辑器打开独立投影窗口，按真实本机几何绘制屏幕与出边范围。拖动结束只调整
内存预览中的本机出边、屏幕和范围。保存前再次读取屏幕，核对原草稿版本并走既有
冲突校验与存储锁。取消或关闭投影会丢弃预览。
[投影草稿](https://github.com/ForceMind/Linko/pull/261)的回归覆盖共享边、重叠、角点、负坐标、
草稿与屏幕变化，以及模拟鼠标事件下的拖动。真实投影位置和混合 DPI 留到集中验收。

### 剪贴板接收后的本机拖放与 Xorg 恢复回执

在剪贴板接收页选择「导入并准备拖放」后，成功导入的文字或 PNG 可从该页拖入本机应用。
离开页面、准备期间失焦、配对变化、进入键鼠会话或 60 秒到期会撤下待拖内容。
三平台 CI 覆盖文字和 PNG 回环、剪贴板变化、停止，以及 Windows/macOS 的原生适配器构建
和 GUI 状态回归。实际应用投递和从源应用按住后跨屏仍是独立验收及开发项。
[剪贴板本机拖放 CI](https://github.com/ForceMind/Linko/actions/runs/43204727378)

Xorg 接收端确认本机清理后，沿用固定证书连接上的 v5 恢复回执。隔离 Xvfb 使用真实
回环连接覆盖申请、授权、左键按下、心跳、正常释放和协议错误结束。协议错误时不报告恢复确认。
真实 Xorg 设备仍待验收。
[Xorg 隔离回执测试](https://github.com/ForceMind/Linko/actions/runs/43205863741/job/131140727017)

### Xorg 主动指针发送

显式启动 `send-x11` 后，只有从屏幕内部用相对指针向指定外沿推动，且固定证书
的接收端确认 v5 租约，才会限制本机指针并开始发送。回到接收端的进入边会提交回程
位置，缺少续期、连接结束或本机活动会释放指针。一次命令执行一次会话。

固定证书回环覆盖授权前无接管、发出相对移动和同连接的恢复回执。隔离 Xvfb 覆盖
发送与接收所有者互斥、合成移动不会触发发送、失去续期后解除限制、右键按下与抬起
均交回本机。实际双机跨边、热插拔和不同鼠标留到集中验收。
[Xorg 主动发送 CI](https://github.com/ForceMind/Linko/actions/runs/43212585535/job/131163680796)

### Xorg 鼠标按钮与双轴滚轮发送

`send-x11` 可分别开启三个标准鼠标按钮和双轴滚轮。接收端许可与发送端选择均匹配，
且 XInput2 原始事件能和当前冻结的本机按钮对应时，才在现有 v5 可靠流发送对应输入。
滚轮的每个有效步只提交一次。停止发送时若仍按住已经截获的按钮，原所有者等到抬起
再归还本机指针。

固定证书回环覆盖按钮按下、滚轮、按钮抬起、回程位置和恢复回执的顺序。Xvfb 覆盖
合成事件仍走本机、已截获右键的抬起由原所有者收尾，以及随后新的右键操作正常到达
本机。物理鼠标、驱动产生的滚轮和双机接管待集中验收。
[Xorg 按钮和滚轮 CI](https://github.com/ForceMind/Linko/actions/runs/43218395557/job/131183401615)

### Xorg 键盘发送

`send-x11 --allow-keyboard-capture` 在现有 v5 租约中发送双方支持的 XKB 物理键位。
键盘沿用当前鼠标发送所有者。XKB 物理键位表变化会停止转发，已经截获的按下仍由
原所有者收尾。长按中的重复按下由源端消化，Ctrl+Alt+Esc 结束远程发送。

固定证书回环覆盖修饰键可靠投递、按住修饰键经过回程边、抬起后提交回程位置。
隔离 Xvfb 覆盖合成来源拒绝、按住键时的重复判定、紧急组合键决策、修饰键释放及
归还后新键对到达本机。真实布局切换、长按、锁定键、紧急恢复和双机接管待统一验收。
[Xorg 键盘发送 CI](https://github.com/ForceMind/Linko/actions/runs/43226579990/job/131211201534)

### Wayland 指针接收

`listen-wayland` 请求 RemoteDesktop 指针访问，并在合成器提供的 EIS 区域内接收
固定证书来源的 v5 指针租约。按钮和双轴滚轮分别开启。系统撤权、设备或区域拓扑
变化及租约结束会停止提交。正常清理释放本会话提交的按钮，并在 EIS 往返确认后报告恢复。

CI 覆盖本地命令许可、区域歧义、按钮释放计划、滚轮方向和幅度、固定证书回环与模拟
portal 撤权。真实 RemoteDesktop 对话框、EIS 设备交付、区域映射、撤权后的实际恢复
留到合成器验收。
[Wayland 指针接收 CI](https://github.com/ForceMind/Linko/actions/runs/43234093393/job/131236722896)

### Wayland 键盘接收

`listen-wayland --allow-keyboard` 随指针租约显式申请键盘许可。只有合成器
给出已就绪的键盘设备才声明受支持的 HID 物理键位。设备变更使当前租约结束。
正常清理仅释放本会话提交的按下，EIS 确认往返后再向发送端回报恢复。

CI 覆盖有限且互不重叠的 evdev 键码、键盘设备选择、未获授权时拒绝、固定证书
回环中的修饰键对及释放账本。实际合成器的设备授权、输入布局和按键释放待验收。
[Wayland 键盘接收 CI](https://github.com/ForceMind/Linko/actions/runs/43239484644/job/131255702553)

### Wayland 主动发送

`send-wayland` 请求系统 InputCapture，在合成器暴露的边沿建立屏障，并只发送
与该次门户激活编号一致、在固定证书 v5 租约下接纳的完整 EIS 帧。按钮、键盘和
离散滚轮分别选择。回程、屏幕变化、撤权或断线结束此次捕获。屏幕变化后通过下一次
显式启动取得新区域。

CI 覆盖有符号区域和共享边屏蔽、无效激活与过时帧、修饰键对、滚轮步进及固定证书
回环回程。真实合成器上的许可、屏障、回程、撤权和多屏变化留到统一验收。
Wayland 接收端的物理输入自动接管待开发。
[Wayland 主动发送 CI](https://github.com/ForceMind/Linko/actions/runs/43246056361/job/131278878302)

### 源应用边沿文件投递

Windows 和 macOS 文件页可显式开启一次边沿发送。源应用把单个文件放到接收条后，
Linko 重新检查配对身份、地址和屏幕布局，再通过现有文件连接发起请求。接收端仍需
对该文件单独确认。切出文件页、最小化、布局或配对变化、停止和超时都会撤回接收条。

Windows 和 macOS CI 覆盖暴露边沿、负坐标、绝对路径、单文件、单次投递及授权失效。
实际 Finder／Explorer 拖放、屏幕缩放和目标应用行为待统一验收。按住同一个拖动跨机
进入目标应用仍待开发。
[边沿文件投递 CI](https://github.com/ForceMind/Linko/actions/runs/43253654318)

### Wayland 发送集成候选资源数据

提交 `e84f8af` 的 macOS 和 Ubuntu 两种 180 秒空闲场景通过 512 MiB 峰值与 64 MiB
增长门槛。macOS 的最大峰值为 108.75 MiB，预热后最大增长 0.22 MiB。Ubuntu 的
最大峰值为 142.82 MiB，采样未观察到预热后增长。

macOS 普通窗口 NoNative 仅通信约 30.09 秒，GUI 平均单核 CPU 为 0.997%，峰值
RSS 为 110.609 MiB。对照同类基线 0.764% 和 119.078 MiB，本次 CPU 高约 0.233 个
百分点，峰值 RSS 低 8.469 MiB。数据来自隔离 CI 单次短测。
[资源作业](https://github.com/ForceMind/Linko/actions/runs/43247295714)

### 边沿投递集成候选资源数据

提交 `fb2d4d8` 的 Windows、macOS 与 Linux 包均来自同一检出树 `486bc79`，版本
`2026.11.13-0015`。CI 校验了包内来源、路径、架构及 SHA-256。macOS 和 Ubuntu
空闲场景的最大峰值 RSS 为 108.188 和 142.188 MiB，最大预热后增长为 0.234 MiB。
峰值和增长门槛分别为 512 MiB 和 64 MiB。macOS 普通窗口 NoNative 仅通信 30.13 秒
测得 GUI 平均单核 CPU 为 1.294%，峰值 RSS 为 109.938 MiB。与 0.764%、119.078 MiB
基线相比，CPU 高约 0.530 个百分点，峰值 RSS 低 9.140 MiB。保留为单次短测数据。
[三平台打包](https://github.com/ForceMind/Linko/actions/runs/43256660925) ·
[资源作业](https://github.com/ForceMind/Linko/actions/runs/43256660418)

### macOS 窗口内触控板检测

在手势实验页显式开启检测并将指针留在画布内，可观察窗口收到的缩放、旋转和横纵向
触控表面滚动。只保留事件类型，失焦、离开实验页、停止检测或开始设备会话即清除。
Windows 和 Ubuntu CI 已通过事件分类回归，macOS 已通过发布构建和资源作业。关闭
条件由集成提交的 macOS 专项回归检查。真实触控板、阶段及跨机注入留待进一步验证。
[窗口内检测 CI](https://github.com/ForceMind/Linko/actions/runs/43254784908) ·
[macOS 资源作业](https://github.com/ForceMind/Linko/actions/runs/43254785421/job/131320184141)

## 集中验收清单

当前开发在 [草稿集成 PR180](https://github.com/ForceMind/Linko/pull/180) 中。此清单随集成更新，包的范围以各自的源码树为准。
审阅提交 `d4c872b` 的 [三平台 CI](https://github.com/ForceMind/Linko/actions/runs/43123212190) 已通过，包含 F13、F22、macOS 小数滚轮、多证书通信诊断和两端系统音频 CLI。随后增加了 IPv6 配对发现、X11 键位以及本机复制拖放适配器；更新后的集成提交使用各自的 Actions 检查。

| 范围 | 已验证的证据 | 集中验收与开发缺口 |
| --- | --- | --- |
| 双向键鼠与恢复 | 运动、滚轮、按键映射和恢复回归覆盖于集成 CI；[macOS 小数滚轮 SDK 核对](https://github.com/ForceMind/Linko/actions/runs/43112306563) | 先跑 Mac→Windows，再跑 Windows→Mac。四边、回程、按键及修饰键、长按与拖拽、物理接管、滚轮、光标隐藏、睡眠、断网、退出和撤权均待真机。F03、F07 和 F12 需要两端诊断才能定位。 |
| 配对、发现、权限和后台 | [IPv6 短码配对发现](https://github.com/ForceMind/Linko/actions/runs/43121334625)；[隔离 X11 窗口与流程](https://github.com/ForceMind/Linko/actions/runs/43123212190/artifacts/13901353937) | Windows/macOS 实际配对提示、IPv6 多网卡与作用域、关闭到托盘、从托盘停止、输入权限变化和键盘可访问性待验。 |
| 本机手势 | [Windows 受控库工作器](https://github.com/ForceMind/Linko/actions/runs/43091867388)；隔离窗口录制、预览与规则回归；[macOS 窗口内检测候选](https://github.com/ForceMind/Linko/pull/271) | 触控板事件类型待真实设备确认。G1 全局捕获和动作保持关闭。G2 保留设计，远程动作和脚本执行不在本次候选能力内。 |
| 剪贴板与文件 | [双工作器文本及文件回归](https://github.com/ForceMind/Linko/actions/runs/43102482924)；[显式 PNG 导入和系统后端](https://github.com/ForceMind/Linko/actions/runs/43107244245) | 双向测试文本、PNG、同意／拒绝、撤权和外部剪贴板变化；文件测试续传、取消、历史、同名文件及文件负载下的键鼠。 |
| 多台电脑、多屏与 Linux | [多证书串行原生监听](https://github.com/ForceMind/Linko/actions/runs/43136235340)、[副屏预留](https://github.com/ForceMind/Linko/actions/runs/43125086927)、[Xorg 指针、键钮和双轴滚轮接收](https://github.com/ForceMind/Linko/actions/runs/43138051348)通过三平台 CI。[Xvfb 指针、键钮释放及滚轮按钮对](https://github.com/ForceMind/Linko/actions/runs/43138051348/job/130918045948)已验隔离合成。 | [主屏方向档案](https://github.com/ForceMind/Linko/actions/runs/43131249465)及[本机副屏选择](https://github.com/ForceMind/Linko/actions/runs/43137143430)通过 CI。[外沿分段发送](https://github.com/ForceMind/Linko/pull/254)可用[桌面导出的布局](https://github.com/ForceMind/Linko/pull/257)和[按目标选用的副屏预留](https://github.com/ForceMind/Linko/pull/256)。物理分段切换、副屏定位和热插拔待验收。本机光标定位与拖尾待实际混合 DPI、透明和焦点验收。本机布局投影待实际焦点与屏幕位置验收。Xorg 指针、键盘、鼠标按钮和双轴滚轮主动发送已有隔离验证。Wayland 键鼠收发待真实合成器验收，接收端物理接管和完整触摸板手势仍待开发。 |
| 原生拖放 | [Windows 适配器](https://github.com/ForceMind/Linko/actions/runs/43121324256)、[macOS 适配器](https://github.com/ForceMind/Linko/actions/runs/43121330845)与[文件页本机复制拖放](https://github.com/ForceMind/Linko/actions/runs/43131249334)通过 CI。Windows [源应用边沿文件投递回归](https://github.com/ForceMind/Linko/actions/runs/43253654318/job/131305962814)已通过。 | Finder、Explorer 的边沿文件投递待真机验收。源应用按住后连续跨机进入目标应用的文字、PNG 和文件拖放仍待开发。实际验证复制、取消、失焦和原文件安全。 |
| 系统音频 | [Windows WASAPI 与 macOS ScreenCaptureKit](https://github.com/ForceMind/Linko/actions/runs/43119118577)完成编译和会话回归；`d4c872b` 的集成已复验 | [桌面独立授权](https://github.com/ForceMind/Linko/actions/runs/43125086706)和[本机输出选择](https://github.com/ForceMind/Linko/actions/runs/43127954327)已通过各自的三平台 CI。[四来源播放和队列隔离](https://github.com/ForceMind/Linko/actions/runs/43135410348)及[两端独立麦克风 CLI](https://github.com/ForceMind/Linko/actions/runs/43132393131)也通过三平台 CI。真实设备、录制提示、断线静音、输出切换及持续音质待验；[桌面麦克风入口](https://github.com/ForceMind/Linko/actions/runs/43137144867)通过三平台 CI，虚拟音频设备仍待开发与授权。 |
| 便携包、安装和回滚 | [同源与校验](https://github.com/ForceMind/Linko/actions/runs/43110809035)；[隔离暂存、切换和回滚](https://github.com/ForceMind/Linko/actions/runs/43110821243) | 最终统一候选须核对两端包的版本、检出树与 SHA-256。真实安装、升级、回滚及长期资源观察待验。正式签名、公证、开机启动、系统服务、官网部署和发布按项目授权处理。 |

### 物理回报

- 在同一个最终候选版本记录 Windows 与 macOS 的系统版本、应用版本、屏幕排列和 DPI、输入设备接入方式。#3/C06 的蓝牙、USB 和 DPI 分别由实际设备确认。
- Mac→Windows 记录跨屏后光标是否移动、点击和滚动是否生效、Mac 光标是否隐藏、普通返回与紧急停止，以及是否出现 `native receive service: Timeout`。Windows→Mac 用同样步骤测量，方向分开记录。
- 每次故障带回两端对应时段的 `KVM-DIAG/5` 数值摘要和原始错误行。优先对齐跨屏提交、移动收发、滚轮来源与注入阶段，保持按键、剪贴板和文件内容在本机。
- 先跑基础双向键鼠、恢复和权限，再跑数据与音频。多机和多屏以 [CLI 说明](CLI.md) 和最终候选实际公开的入口为准。按开发计划记录双向 500 次切换、关键恢复场景 100 轮、8 小时预览与 24 小时正式候选耐久的执行范围。

F04/F11/F17 和 macOS 完整冻结仍按 [ADR 草稿](https://github.com/ForceMind/Linko/pull/125)待决定。
#3/C06 与历史故障 #10 保留待验证状态。Linux Xvfb、模拟输入、编译通过和 SDK 核对仅作为各自范围的证据。
