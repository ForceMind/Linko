# C03 capture lifecycle

Status: implemented-unverified for real native capture. The worker owns an
explicit stop flag, completion channel and reaper-owned JoinHandle. CaptureSession
shutdown disables suppression/restores the source pointer before requesting stop;
the caller waits at most 500ms per attempt and reports errors. Drop provides bounded
fallback cleanup. A still-running thread retains the process-wide reservation,
preventing another capture from installing competing hooks during a timeout.

Windows dispatches input immediately via PeekMessage and waits for input with a
50ms maximum idle wait. macOS runs the existing event tap in bounded 50ms run-loop
slices, disables the tap and releases its source/port before completion. Native
initialization failure or readiness timeout drops the thread owner and requests
stop too. A blocked OS call cannot be force-killed safely: the join may time out,
but the gate is disabled and new capture remains refused until the native thread
is reaped. The background reaper can remain blocked; the caller cannot.

Pure lifecycle tests cover 20 start/stop/join rounds, repeated shutdown, early
initialization exit and a deliberately blocked backend's timeout/recovery.
They are not native hook/handle-count measurements. Real permission-granted OS
start/stop cycles, resource counts, permission revocation and session reconnection
remain pending under C03/#3. For the subsequent C04 implementation and remaining
native UX acceptance, see
[WORKER_IPC.md](WORKER_IPC.md).


Task completion is not treated as thread completion: a thread-local destructor
may still block after the completion notification. Neither that notification nor JoinHandle::is_finished is a join
guarantee. A dedicated reaper joins the native thread; the caller only waits on
its acknowledgement with a timeout. The reaper retains capture ownership through
all native thread-local destructors. One reaper thread is added per active capture;
its resource overhead has not been benchmarked. A regression uses a deliberately blocked thread-local destructor and an independent rescue to
prove the previous implementation failed this boundary without hanging CI.

## G03 native short-path increment (default off)

`platform::gesture_capture` supplies a Windows-only library entry on the same
existing WH_MOUSE_LL/WH_KEYBOARD_LL lifecycle; no binary calls it. KVM capture
passes `None` and keeps its current dispatch/control behavior. The G1 branch
runs before the KVM BTreeSets/tokio sender and does no recognition, OS target
query, disk/network I/O or dynamic storage in callbacks. A preallocated 256-event
SPSC ring carries opaque Begin/Move/End records to one worker. Raw coordinates
are Windows physical pixels, not recognizer logical pixels or DPI acceptance.

The worker must independently obtain native `CaptureAdmission`, evaluate app
rules/exclusions and publish one-use admission. The proof binds the native
context epoch, random worker identity and exact independent action consent
revision and the original at-most-five-second expiry; heartbeat cannot renew that
admission. It is not an action grant. A single versioned atomic latch coordinates
publication, callback presence and one-use claim: an event overlapping publish
makes publish fail even when the event has already ended. Existing pressed keys
and all five Windows mouse buttons prevent arming. Known injected/tagged events
never update physical pair ownership or enter G1.

Passed right-down stays passed through its up, including startup-held and
repeated events. Suppressed right-down owns its physical up even after stop,
focus invalidation, other input, queue full, timeout, worker loss or revocation.
Cancellation drops the entire stroke rather than recognizing a queued prefix.
The one-second worker heartbeat watchdog and Ctrl+Alt+Esc are terminal for that
capture. Candidate duration is strictly below 2500ms. Candidate edges stay
blocked and cancellation/completion requests fresh edge rearming; this is not a
live KVM/G1 edge integration claim. New physical input cancels an unretired old
stroke, including delayed action/right-click recovery context.

Stopping cancels first but keeps the original hook alive while physical right-up
is owed. A 500ms caller timeout retains the old thread/reaper reservation and
reports failure. A known message-source failure latches ownership-lost; clearing
the local debt counter then does not prove the physical pair was recovered.
Windows can silently remove a slow low-level hook: that cannot be detected
reliably by this increment and remains a native acceptance/diagnostic limitation.

The existing Reservation now also holds a nonblocking OS file lock on the empty
`temp_dir()/peerglide-native-input-capture.lock`. It is acquired outside callbacks
and explicitly unlocked only after the real thread and its TLS destructors join.
Existing nonempty, directory or symlink objects reject without truncation,
replacement or removal. Newly created Unix locks are mode 0600. This coordinates
cooperating processes sharing that configured user temp root; different temp
roots and malicious local path replacement are outside this coordination claim.
Unlock failure reaches shutdown as an error and keeps the process guard poisoned.
The empty lock file persists; no user file cleanup is performed.

macOS G1 is not exposed by this entry. CGEvent source PID/state/user-data fields
cannot prove physical origin; even HIDSystemState may originate in a user-space
driver. No PID=0/state=1 heuristic or injected fixture substitutes for physical
provenance. Existing macOS KVM tap remains unchanged. A trustworthy physical
provider within the same lifecycle and native acceptance are still required.

The initial short-path increment did not implement gesture worker/recognizer
wiring, application rule dispatch, native overlay or local G1 consent UI. The
subsequent Windows-only owned short-click recovery library is documented below;
it has not been accepted on real input and does not complete product G1. Ordinary unarmed pairs pass; an armed short click is captured; only the separate sealed recovery path may
submit its pair. No product binary calls that path. Therefore product G1 stays disabled. Synthetic pair,
queue, allocation and lifecycle tests are not real right-click, driver, DPI,
permission, sleep or two-machine C06 acceptance. No new protocol/ALPN or grants.

The next sealed Windows short-click library path is documented in
[LOCAL_CLICK_RECOVERY.md](LOCAL_CLICK_RECOVERY.md). Its new atomics share this
same capture lifecycle; source loss and pending injected ownership are not clean
shutdown proof. It does not enable the product worker or macOS/keyboard recovery.


## 基础KVM捕获域增量（0.2.1-dev，待候选CI/实机验收）

Position、Motion、Input与Toggle携带捕获时的Local/Remote域及opaque CaptureEpoch。
它由每个capture唯一instance及原子control中的generation组成，不序列化、不作为
认证或网络lease。激活/本地域fence/禁用原子改变代次，旧队列事件不能被当前
outgoing提升或误撤销新incoming；溢出/源丢失停止，不回绕复用。

既有callback guard冻结一次路由，入队与抑制使用同一决定。固定HID/button表
保留首次Down的Local/Remote/Consumed归属；repeat与Up继承，disable、队列满或
stop不能把旧Remote Up改成本机Up。未支持侧键不冒充中键，不增加远端能力。
没有新增hook、回调识别/IO/脚本或全局G1启用。

begin_routed失败立即禁用捕获；保留原返还位置供清理。shutdown_poll只是不等
join，不保证OS调用时长。原thread在stop后继续pump已抑制pair的Up；pending由
reaper持有reservation。session先请求停和有界通知对端，再以timer轮询；从首个
停请求500ms仍未完成时报告Unconfirmed，但普通停止不能因此自行退出进程杀hook。
真实已reaped的source/restore错误作为Unknown结束，不假报clean或无条件永等。
GUI仍可明确ForceStop并保留未知警告。

本段是phase1软件契约，自动结果需按最终SHA记录。接收端首个物理事件在旧远程
修饰键释放前被OS处理的P1尚需phase2 Receiving保护；当前域隔离不能宣称已解决。
Win/Mac真实输入、DPI/权限/睡眠/故障、硬件Up与OS效果均仍属#3/C06。


## 接收端独立Native owner接线（0.2.1-dev，当前待共同CI）

同一Native loop串行执行pointer/input/normal Release/恢复，callback只做固定配对
和原子请求。Receiving与epoch/physical-held/IN_CALLBACK一次CAS发布后才可Granted。
首个本地键/按钮整对Consumed，移动/滚轮丢本次以请求接管；清理完成且被吞修饰键
松开后才撤保护，旧Up跨clear/stop仍吞。source旧Remote修饰键债务也阻止新的本地
组合；Windows wheel remainder按捕获域重置。G1路径仍关闭，不新增hook。

Driver命令250ms截止与取消序号，Native执行前复查，取消之后已in-flight的当前
SDK调用不能被虚构中断，由单一owner账本回收。Native-only Engine有有限ledger/
队列、epoch-tagged heartbeat及独立watchdog；Tokio停滞时Native仍处理恢复。Core
只管协议意图，不能把其release列表再重复注入OS。Submitted仅SDK请求或接受恢复；
Active/Draining/Retired/RecoveryRequired明确区分，Draining不能显示本地恢复完成。

managed stdin线程先设置stop watch，再直接原子stop并唤醒Native loop，不等Tokio。
常规收尾等engine退出条件与物理pair债务；失败后需松开并使用新的完整逃生组合
请求强制返回，保留Unknown、旧Consumed Up并关闭新入场，不假报正常恢复。OS/loop
真失进展仍只能由外部明确ForceStop，不能自动强杀后写Local。

权限/桌面/SecureInput只在Native loop中只读采样，发生不可确认变化不绕过保护。
观测基准同时用单调和墙钟，不可在慢SDK后刷新；每次后续效果/cleanup前再检查
1.5s连续性，失效不盲发Up。源失效/强制返回不同于观察到了配对Up，保持Unknown。
Mac event对象创建成功前不修改BUTTONS；CGEventPost为SDK请求而非OS/app确认。

此代码与20个Engine fake、Parent配对/滚轮/观测缺口回归尚待本批实际fmt/编译/
测试/进程验证；真实Win/Mac、权限、DPI、睡眠与OS效果没有因此通过。Linux仍仅
诊断，unsupported facade不会生成ready状态或调用OS输入。

## 0.3.17 startup-held button guard

KVM startup now includes Windows XBUTTON1/2 and macOS observable extra buttons
in its held set before admitting Receiving. macOS keeps the original three
CoreGraphics samples and adds a one-time NSEvent pressedMouseButtons snapshot
for native button numbers 0..31. That snapshot may include synthesized state;
it is only a conservative admission veto, never physical-origin authentication
or continuous tracking. Local startup-down ownership and its passed-up pair
remain unchanged. Unsupported buttons remain unsupported by the peer bitmap.
Native tests check refusal while held, local-up clearing and subsequent admission;
real side-button hardware and permission acceptance remain C06.

## Idempotent release after clean Native retirement

A managed Stop can retire the receiving epoch before the async session queues
its final Release. A Closed command/reply is accepted only when a fresh snapshot
confirms the same epoch Retired and the whole receive owner reports Retired.
Draining, failed/source-lost, unknown epoch and a new active owner still reject;
Timeout/Stale/QueueFull are never translated. Thread shutdown, join, source health
and sticky recovery failure checks remain independent and mandatory.


## Ordinary source-edge return

Normal return from an outgoing lease uses the existing capture gate to atomically
check the exact Remote epoch, no held physical keys/buttons and no active callback
before retiring suppression. A hold or a callback/CAS collision keeps the original
remote route and forwards motion normally; later motion may retry after release.
This is not a new hook or a wait inside a callback. Source cursor restoration
retains the existing return-pending fence. Explicit stop, emergency, source loss
and failed-session recovery remain unconditional and may release held inputs.
Deterministic gate/pair tests do not constitute physical drag or DPI acceptance.
