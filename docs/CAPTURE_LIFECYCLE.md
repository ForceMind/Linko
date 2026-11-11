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
runs before the KVM hold accounting and event queue and does no recognition, OS target
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
remote route and forwards motion normally. Later motion or the existing session
heartbeat tick may retry after release; the tick first waits for queued captured
input to drain, retaining reliable discrete-input/terminal-cursor ordering.
This is not a new hook or a wait inside a callback. Source cursor restoration
retains the existing return-pending fence. Explicit stop, emergency, source loss
and failed-session recovery remain unconditional and may release held inputs.
Deterministic gate/pair tests do not constitute physical drag or DPI acceptance.


## 源端远端阶段的光标冻结与隐藏（实现未经实机验证）

目标：本机控制另一台期间，源端光标不再可见地移动；能隐藏时隐藏。任何结束路径
都恢复。本节记录实现到哪一级、为什么停在那里。代码只在 CI 编译与测试，物理效果、
后台进程是否生效、崩溃后的系统行为均待 #3/C06 实机确认。

### macOS：第一级已实现（解耦），第二级不实现（隐藏）

机制：进入远端阶段时调用 `CGAssociateMouseAndMouseCursorPosition(false)`，让鼠标
移动不再改变光标位置；返回本机前调用 `(true)` 恢复关联。不 warp、不投递绝对移动、
不改光标图像或任何用户设置。Apple 文档说明解耦后应用收到的事件位置恒定、但仍带
X/Y 位移更新，因此现有 tap 读取的 `MouseEventDeltaX/Y`（字段 4/5）应继续有效，
远端位移计算不需要改；本机阶段只在恢复关联之后才读取绝对位置。

线程与锁：所有调用都在 tap 回调之外。冻结在会话线程的 `begin_routed_from` /
`begin_routed` 提交成功之后执行；判断"当前仍是同一远端代次、未过看门狗截止、源
健康"在同一把进程级锁内完成，所以已结束的旧代次不可能迟到解耦。恢复有三个入口：
会话线程 `restore_checked`（阻塞取锁）、捕获线程每个运行循环切片（`try_lock`，
取不到就留给下一片，绝不在 tap 仍启用时等待会话线程），以及捕获线程退出时（tap
已停用后阻塞取锁）。请求被拒或结果未知都按"仍需恢复"记账，之后每个本机切片重试。

结束路径与恢复点：
- 普通边缘返回：`restore_at_boundary` → `restore_checked`。
- 显式停止、会话断开、`CaptureSession` drop：`shutdown_wait` → `restore_checked`。
- 紧急逃生、看门狗过期、队列满、不支持的输入等使门控离开 Remote 的故障：捕获
  线程下一个运行循环切片（≤50ms，独立于异步会话）恢复；会话随后的 restore 是第二
  道。
- 失去来源：`restore_checked` 在健康检查之前先恢复关联；捕获循环退出后再恢复一次。
- 进程正常退出（含 Ctrl+C 走优雅停止、`std::process::exit`、main 返回）：首次冻结
  时登记一次 `atexit`，若仍欠恢复则重新关联。
- SIGKILL（例如 GUI 的强制结束）、崩溃、abort：无法运行任何本进程代码。Apple
  文档没有说明进程退出后 WindowServer 是否自动恢复关联，本项为**未知**，需要实机
  确认。

已知限制：Apple 文档把该函数描述为"应用在前台时"生效。worker 是没有 AppKit 事件
循环的 tokio 命令行进程，通常不在前台；后台调用是否生效没有官方说明，只能实机验证。
若不生效，行为退回到此前的"光标跟随但事件被吞"，不影响位移转发。

第二级（隐藏）不实现。`CGDisplayHideCursor` 文档写明多数情况下调用方必须是前台
应用；后台隐藏通常需要非公开的连接属性和主线程 AppKit 透明窗口来接管光标所有权，
而 worker 没有主线程事件循环，非公开接口也无法给出"所有结束路径与崩溃后都能恢复"
的依据。替代方案：未来若由 GUI 进程（有 AppKit 主线程）承担一个透明非激活窗口，需
另立设计并实机验证。

### Windows：第一级已满足（停车），第二级不实现（隐藏）

现有机制保持不变：提交远端后把光标停在主屏中央，钩子用 `MSLLHOOKSTRUCT.pt` 与
当前光标位置之差计算位移，并吞掉移动事件，所以远端期间光标不移动但可见。停车仍然
必需：从现有代码注释看，光标停在边缘时钩子坐标没有越过边缘的余地，向外的位移会
丢失（未实机测量）。

第二级不实现，原因：
- `SetSystemCursor` 替换的是整个会话的系统光标；文档只给出用
  `SystemParametersInfo(SPI_SETCURSORS)` 重新加载系统光标的办法，没有说明进程退出
  后自动恢复。进程被强制结束后用户会留下透明光标，而 `SPI_SETCURSORS` 重新加载还
  会覆盖其他程序或辅助功能做的光标定制，违反"不改用户光标方案"。
- `ShowCursor` 只维护一个内部显示计数，文档没有说明它能隐藏其他进程窗口上方的
  光标；后台 worker 调用预计无效（未验证）。
替代方案：在捕获线程（已有消息泵）上为停车点创建一个透明、置顶、不激活的小窗口，
在 `WM_SETCURSOR` 中把光标设为空。窗口随线程结束由系统销毁，崩溃后可自动恢复，
但能否在被吞掉的移动下持续生效、对全屏/提权窗口的表现都需要实机验证，留待后续。

### 测试

`source_cursor` 状态机与门控 `remote_live` 有纯软件测试：每个远端阶段只解耦一次；
停止/紧急/故障、看门狗过期、失去来源后各只恢复一次；已结束的代次不会再解耦；系统
拒绝的解耦不记为待恢复；失败的恢复在远端期间不重试、回到本机后重试到成功。它们不调用
任何 OS 接口，不代表物理效果。

评审修正：恢复关联失败只写一行诊断，不再让正常返回或停止报错结束会话；捕获线程在本机
阶段继续重试。原因是 worker 作为后台进程调用该接口是否被系统接受尚无依据，若一律报错，
会让已经能用的跨屏在每次返回时中断。

依据：
- https://developer.apple.com/documentation/coregraphics/cgassociatemouseandmousecursorposition(_:)
- https://developer.apple.com/documentation/coregraphics/cgdisplayhidecursor(_:)
- https://developer.apple.com/library/archive/documentation/GraphicsImaging/Conceptual/QuartzDisplayServicesConceptual/Articles/MouseCursor.html
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setsystemcursor
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-systemparametersinfow
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showcursor
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setcursorpos

## Windows 接收注入独立线程（实现未经实机验证）

问题：Mac 控制 Windows 时，接收端 Native loop 既安装 WH_MOUSE_LL/WH_KEYBOARD_LL，又在
同一线程里同步 `SendInput`。SendInput 返回前，本线程不取消息，排在它后面的低级钩子回调
也就无人处理；若 SendInput 本身变慢（其他进程的钩子、受保护桌面、系统负载），本线程停滞
可接近系统 `LowLevelHooksTimeout`（约 300 ms），而命令截止只有 250 ms，于是出现 `rel_rx`
多于 `rel_apply` 和 `native receive service: Timeout`。是否正是实测停滞的唯一原因尚未在实机
确认；本改动消除的是"钩子线程被 SDK 调用占住"这一个已知风险。

参考：Oink 的注入独占一个线程，理由是原生注入可能阻塞；输入线程通过有界队列交接，
确认类操作用原子栅栏，生产者只轮询结果、从不在回调里等待注入方。来源与差异见
[REFERENCE_RESEARCH.md](REFERENCE_RESEARCH.md) 的对应小节。

### 所有权与生命周期

- 注入线程不是钩子，不安装任何钩子，不读物理输入。它只执行 `ReceiveBackend::pointer` /
  `input` 交来的固定小包。只有接收用的 capture（`capture_receiving`）创建它；KVM 源端与
  G1 本机路径不变。
- 由 Native loop 在安装钩子之前创建，创建失败等同钩子安装失败（capture 启动失败）。
  `NativeReceiveBackend` 独占唯一句柄，没有第二个生产者。
- 退出顺序：Engine 最后一次 `pump(stop)` → 请求注入线程停止（丢弃尚未执行的条目，
  不再开始新的 SDK 调用）→ 卸载两个钩子 → 有界等待（500 ms）在途调用返回并 join。
  超时：把接收 owner 标为 RecoveryRequired（`shutdown` 报告"cleanup remains unconfirmed"），
  然后仍阻塞 join，使注入线程不会比 capture 线程活得更久；原有 reservation/reaper 规则
  照旧阻止新的 capture。被丢弃的条目从不在 capture 结束后执行。

### 交接队列

- 一个预分配的有界 FIFO（256 项），`Mutex` 只在入队/出队时短暂持有，SDK 调用期间不持锁。
  生产者（Native loop）从不等待注入线程；注入线程空闲时阻塞在条件变量上。
- 光标移动可合并：若队尾尚未开始执行的条目是同一 epoch 的光标移动，新位置直接覆盖它，
  不占新槽位。只合并相邻的移动，所以"先移动到快照位置再按键"的顺序不变；键、按钮、
  滚轮从不合并、从不丢弃。这消除了 F01 描述的"每个数据报一次往返、先进先出累积延迟"：
  积压时只执行最新位置。
- 队列满：键/按钮/滚轮以及无法合并的移动返回 `BeforeSubmission`，Engine 走既有
  fail/retire 保护路径（按下仍按"先记可能的 Down"规则处理，见下）。

### 失败回报与结果语义

- 入队成功即 `Ok`，`Submitted` 仍只表示"已提交 SDK 请求"，不是 OS/应用收据。
- 注入线程上 SendInput 失败、部分成功、或执行前复查不通过，都记为失败（不区分为
  BeforeSubmission，按未知处理）：
  - 下一次 `pointer`/`input` 调用返回 `UnknownOutcome`；Engine 每轮 `pump` 也会主动查询，
    即使没有新命令也会 fail + retire。
  - 失败的松开（键/按钮 Up）记入固定位图，由 Engine 在 cleanup 中取回：重新记为仍按下并
    计一次失败尝试，沿用原来的 3 次重试与"重试用尽即 FAILED"规则。
  - 失败之后，队列里排在后面的按下/移动/滚轮不再执行（同步实现在失败后也不会再发出它们）；
    松开仍执行，因为 Engine 账本已按已提交计。
- 每个条目在 SDK 调用前于注入线程上复查：capture 健康、同一 Receiving epoch 仍是当前、
  Native loop 最近一次来源观测（同一 1.5 s 单调+墙钟规则）仍新鲜。不通过则不调用 SDK，
  因此 SourceLost/换代之后排队的 Up 不会盲发。
- 已知差异：引擎的时间/状态/修饰键检查仍在入队时做；从入队到执行之间（通常亚毫秒）新出现的
  本机修饰键不会让已排队的效果撤回。

### 栅栏

- `leave_receiving` 之前：Engine 的 cleanup 先发出所有 Up，再要求注入队列为空且最后一次 SDK
  调用已返回；未完成时本轮返回、等注入线程完成后的唤醒（PostThreadMessage 到 Native loop；
  50 ms 空闲等待兜底），绝不在 Native loop 上阻塞等待。随后取回失败的 Up，若有则继续重试，
  不移除保护。等待超过 1000 ms 仍未完成：fail 并进入 FAILED（RecoveryRequired），不假报干净。
- `force_return`：先丢弃尚未执行的条目（强制返回本就禁止任何后续注入），若仍有在途调用则
  本次返回失败，Engine 下一轮重试；在途调用返回前不清除保护。
- Native 后端的 `leave_receiving` 也自行复核队列已空，作为第二道检查。
- macOS 与测试 Fake 仍是同步后端：默认实现"永不挂起、永不异步失败"，行为与之前一致。

### 钩子与诊断计数

- `mouse_hook`/`key_hook` 对 TAG/LLMHF_INJECTED 事件的放行逻辑不变。注入改由另一线程发出后，
  这些事件会经过本进程的低级钩子（Oink 注释记录：Windows 11 不对线程自身的 SendInput 调用
  该线程的 LL 钩子），因此 `filtered`、`wh_inj` 现在也会计入本机接收注入的移动/滚轮。
- `sdk`/`sdk_fail`/`moved`/`unchanged`/`unknown`/`slow_ptr`/`slow_inp` 改在注入线程上、
  围绕实际 SendInput 递增，语义仍是"实际 SDK 调用"；被合并的移动没有 SDK 调用，不计入
  `sdk`，所以 `apply` 可以大于 `sdk`。`loop_stall` 仍在 Native loop 上计。
  `apply`/`rel_apply` 等会话层计数不变。

### 测试与未验证项

`inject_thread` 的纯软件测试（Fake sink，不调用 OS）：顺序保持、光标合并、栅栏等待与唤醒、
失败传播与失败 Up 回收、执行前复查拒绝、队列满、丢弃与停止时线程退出（含在途阻塞时的有界
超时）。`receive_driver` 增加 Fake 异步后端测试：栅栏未完成不 Leave、超时进入
RecoveryRequired、异步失败的 Up 被重试、无新命令时也会发现异步失败。
真实 SendInput 是否仍可能阻塞（例如受保护安装、UAC/安全桌面、其他进程慢钩子）、实际延迟
与 Timeout 是否消失，都需要 Windows 实机 #3/C06 验证。

## Bounded KVM capture handoff

The desktop session allocates a 128-event SPSC ring before installing the
existing native capture. The hook only writes into the ring. The same Windows
message pump or macOS run loop wakes the async consumer after callback dispatch,
before waiting again. Producer teardown publishes closure before waking the
consumer. Input order, the full-queue gate disable, and pair ownership stay in
the existing capture path. The Win/macOS timing and recovery cases remain part
of the physical acceptance matrix.
