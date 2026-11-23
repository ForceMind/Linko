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

## RW03 本机预览与 RW04 媒体实验（2026-11-23）

以下证据对应各行的草稿候选提交。不同分支的实验结果不能合并当作一个发布版本的验证。

| 范围 | 精确提交与证据 | 已验证 |
| --- | --- | --- |
| RW03 本机单窗口只读预览 | `b846ed0874d288e34aaf6c17d6fb3549e934f066` · [Workspace Lab](https://github.com/ForceMind/Linko/actions/runs/45103803193) · [Rust CI](https://github.com/ForceMind/Linko/actions/runs/45103803166) | 两端原生构建通过。Windows 100 次启停无进程句柄或 GDI 对象增长，另通过 40 次采集、缩放、源关闭及清屏。macOS 在已有录屏许可的 runner 上通过实际窗口的采集、缩放、停止、重新选源和关闭。 |
| RW04 Windows 系统 H.264 | `d364905ae1a258eb004954b63ce43a758aafdc3c` · [首轮](https://github.com/ForceMind/Linko/actions/runs/45102064419/job/137454936884) · [复跑](https://github.com/ForceMind/Linko/actions/runs/45102064419/job/137457855673) | 独立测试进程从 WGC 采集开始到 Media Foundation 停止保持 COM 生命周期。两台 runner 共通过 60 次实采窗口关闭和编解码，每次 24 帧且验证两种颜色。最大积压一帧。历史异常的执行地址未能识别具体模块。 |
| RW04 macOS 原生回环 | `8701cdaf8e548c8abc1d64e549aa66e8403bb0d5` · [原生作业和测量产物](https://github.com/ForceMind/Linko/actions/runs/45087485463/artifacts/14781172077) | ScreenCaptureKit 捕获测试窗口，经本机 WebRTC UDP/DTLS H.264 回环在 AppKit 显示。覆盖颜色、缩放、撤权后拒绝旧帧、重开和关闭。100 次启停中，部分完整回环配置的 IOAccelerator 驻留页仍有增长。 |
| RW04 进程替换与协调器 | `0cee2ee57d6462ccbf84e982adfbcdc30a409ee5` · [macOS 实际编码进程](https://github.com/ForceMind/Linko/actions/runs/45098986436/job/137445189037)<br>`b846ed0874d288e34aaf6c17d6fb3549e934f066` · [三端工作空间回归](https://github.com/ForceMind/Linko/actions/runs/45103803193) | macOS 子进程被终止后，新进程从零驻留映射开始并解码新素材。Windows、macOS、Linux 另用本机测试子进程验证协调器清空同一连接的两个视图、拒绝迟到的展示回执，并保留其他连接及原有释放和窗口恢复义务。协调器场景使用测试授权和像素。 |
| RW04 macOS 本机媒体进程控制 | `090965ac1b805f0d4ba78f8e30c26727db393114` · [VideoToolbox 作业](https://github.com/ForceMind/Linko/actions/runs/45104951436/job/137463976144) | 实际硬件编解码子进程通过继承管道校验控制版本、消息类型和会话代次。正常停止在释放会话后回执。错误版本、截断、命令乱序、零代次及跨会话停止均被拒绝，强制退出后的新进程能重新解码。 |
| RW04 双端实采和绘制确认 | `79361b0502cd8d49f8c6cdfeca6d3e9932f8500c` · [Windows 原生作业](https://github.com/ForceMind/Linko/actions/runs/45121283750/job/137515393765) · [macOS 原生作业](https://github.com/ForceMind/Linko/actions/runs/45121283750/job/137515394056) · [Workspace Lab](https://github.com/ForceMind/Linko/actions/runs/45121283669) · [Rust CI](https://github.com/ForceMind/Linko/actions/runs/45121283651) | 两端实际采集并编解码测试窗口缩放前、后的两帧。协调器取出第一帧，原生窗口绘制后回采核对九点才确认展示。进程强制退出后清空同一连接的两个视图、拒绝旧回执和迟到帧，保留另一连接和恢复义务。测试只显示一帧，协调器使用逻辑时钟。 |

RW03 构建 `2026.11.23-1104` 的 Actions 产物为 [Windows 预览](https://github.com/ForceMind/Linko/actions/runs/45103803193/artifacts/14786178954) 和 [macOS 预览](https://github.com/ForceMind/Linko/actions/runs/45103803193/artifacts/14785619836)。它们是本机只读预览。应用的连接、键鼠和共享功能随各自候选另行验证。

正式 RW04 仍需要 Windows 固定 WebRTC 工具链的 SDK `10.0.28000.0` 与通过评审的独立媒体协议。托管 Windows SDK 预检仍有缺项，局域网观看、正式媒体与用户授权接线、八小时连续资源验证仍待完成。[Windows 预检](https://github.com/ForceMind/Linko/actions/runs/45087485463/job/137409299532) · [媒体 ADR #333](https://github.com/ForceMind/Linko/pull/333)

本节编译、测试及原生试验均在 GitHub Actions 运行。Windows/macOS 用户机器的系统权限、真实双机行为及 [#3/C06](https://github.com/ForceMind/Linko/issues/3) 仍待验收。

## RW06 工作区资源调度（2026-11-23）

- [PR #372](https://github.com/ForceMind/Linko/pull/372)，`45e6ffe2baac71f465b7253fa7d3a3c2f8173485`。四面板回归覆盖观看焦点、离屏、授权到期、尺寸更新、关闭和解码槽竞争。原输入票据仍只对原目标有效。[三端 Workspace Lab](https://github.com/ForceMind/Linko/actions/runs/45123200722) · [Rust CI](https://github.com/ForceMind/Linko/actions/runs/45123200689) · [双端原生回归](https://github.com/ForceMind/Linko/actions/runs/45123200688)
- [PR #373](https://github.com/ForceMind/Linko/pull/373)，`8330e5431475f051f34627234e11eaa3a47f4a89`。单槽预算拒绝在旧工作进程确认退出前启动替换。Windows 与 macOS 的实际编解码进程回收后，新进程取得槽位并正常停止。离线回归验证同一来源旧票据及另一预算池的票据无法释放现有槽位。[Windows](https://github.com/ForceMind/Linko/actions/runs/45125279984/job/137528082503) · [macOS](https://github.com/ForceMind/Linko/actions/runs/45125279984/job/137528082825) · [三端 Workspace Lab](https://github.com/ForceMind/Linko/actions/runs/45125279941) · [Rust CI](https://github.com/ForceMind/Linko/actions/runs/45125280004)

- [PR #374](https://github.com/ForceMind/Linko/pull/374)，`f176c05645c409d5a0b15ac46480dd65ba53eaeb`。现有 eframe 工程的离线示例通过三端焦点、离屏、暂停和恢复回归。逐张核对 916×584 和 640×300 的 Linux 原生窗口截图。紧凑窗口按 4 后完整显示底部两张卡片，第四面板获得全画质分配。[截图](https://github.com/ForceMind/Linko/actions/runs/45128907777/artifacts/14797650518) · [Rust CI](https://github.com/ForceMind/Linko/actions/runs/45128907777) · [Workspace Lab](https://github.com/ForceMind/Linko/actions/runs/45128907770)

本批验证覆盖协调器、单槽进程交接和离线面板操作。实际多路媒体呈现、GPU 预算和跨设备观看待验证。
