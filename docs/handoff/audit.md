> 原始研究报告（2026-11-10/11，基线提交 `c199dcd`），由接手文档 `docs/HANDOFF_CLAUDE_TO_GPT.zh-CN.md` 引用。只读调查，当时没有编译或运行任何代码；行号和部分结论可能已过期，使用前先对照当前代码。文中提到的 `scratchpad/oink-src` 是本机临时目录，已不在仓库里，Oink 源码请从固定提交 `3ca82127e61f9d7106a70574a82cc63566d7a572` 自行取得。

# Linko 只读代码审查报告

审查基线：工作树 HEAD e0f75c6（在 c199dcd 之上仅多一个 input_diagnostics.rs 的 Default 修复提交；我读到的就是该版本）。
方式：只读文件。没有编译、没有运行任何测试或程序，没有读取 tokio/quinn 源码。凡涉及运行时表现的结论，证据等级都不高于“从代码推断”。
证据等级含义：亲自读到代码 = 该事实（行为、数值、行号）是我在本次会话读到的原文；从代码推断 = 由读到的代码推出、未运行；猜测 = 无直接代码依据的假设。
“受保护不变量”指 AGENTS.md 所列（单一捕获生命周期、回调内不得识别/分配/IO、v5/ALPN 兼容、G1 关闭、不覆盖用户配置、恢复与自有注入清理等）。

## 总览

| 严重度 | 条数 |
|---|---|
| P0 | 0（未发现能直接确认的数据损坏/崩溃） |
| P1 | 4 |
| P2 | 14 |
| P3 | 8 |
| 合计 | 26 |

每条在标题方括号内标注类别（正确性、内存/性能、结构/可维护性、未完成功能）。

## 已读范围内“做得好、不需要改”的部分（避免重复劳动）

- capture_thread.rs：锁文件校验、保留位、reaper 线程 join 语义完整，不建议动。
- GUI 读取 worker 的 stdout/stderr 用独立线程持续排空（main.rs:348-444、884-905），worker 侧阻塞式 `println!` 不会被 GUI 反压卡住；日志 `VecDeque` 上限 100、`sync_channel(128)` 有界。
- 设备图标纹理在 context 内缓存（visual_assets.rs:80-96），没有每帧重新解码。
- session.rs 与 receive_driver.rs 非测试代码中几乎没有 `unwrap/expect`（仅 session.rs:360 一处在 `is_some` 之后）。

---

## P1

### F01 [内存/性能 + 正确性] 接收端每个运动数据报都做一次“线程往返”，且按先进先出逐个处理，没有“只取最新”
- 位置：apps/peerglide-desktop/src/session.rs:837-858（数据报分支，846 行 `recovery.pointer(m.x,m.y).await`）；session.rs:215-246（`Recovery::pointer`）；crates/peerglide-platform/src/receive_driver.rs:325-360（`command()`：每次新建 `oneshot`、`try_send` 入 128 槽队列、`wake_loop()`、`tokio::time::timeout(250ms)` 等待回复）；crates/peerglide-transport/src/lib.rs:235（数据报接收缓冲 64 KiB）。
- 证据等级：调用链与数值“亲自读到代码”；“积压后光标滞后、CPU 偏高”是“从代码推断”（未运行、未测量）。
- 问题：源端对每个鼠标事件发一个绝对位置数据报（见 F09）。接收端 async 任务每收到一个，就要唤醒原生线程（Windows 为 `PostThreadMessageW`，macOS 为 `CFRunLoopSourceSignal+WakeUp`），原生线程执行 `guard_failure`（内部再取 `SystemTime::now()` 等）和 `SendInput/CGEventPost`，再通过 oneshot 回复，期间 async 循环整体被挂起。若源端是 1000 Hz 鼠标，单个往返超过 1 ms 时，数据报会在 quinn 缓冲（64 KiB，每包约 90 字节 JSON，约 700 包）里排队，光标按过期位置依次回放，滞后可达数百毫秒。运动是“绝对位置、丢包可由下一包修复”的语义，处理过期包没有任何价值。
- 另一条后果：`Recovery::pointer` 遇到 `ReceiveError::Timeout`（原生线程 250 ms 内未回复，例如系统短暂卡顿、休眠唤醒）会走 `Err(error) => return Err(error.into())`（session.rs:239），随后 `InputStatus::checked`（session.rs:441-447）把状态锁成 `RecoveryUnconfirmed`，会话终止并要求用户处理。一次纯延迟被升级成“恢复未确认”。
- 最小修复：(1) 读到一个数据报后，用非阻塞方式继续读空本地缓冲，只对最新一个（序号最大且 `receiver.motion` 通过）发起 pointer；(2) 为运动单独走“最新值槽位”（`Mutex<Option<ReceivePoint>>` 或两个 `AtomicU64`）+ 复用现有 `wake_loop`，由 `pump` 读取并应用，不再每包 oneshot+timeout，失败通过共享标志上报；(3) 运动路径的 `Timeout` 视为“丢这一包”，由下一包修复，只有 Begin/Input/Release 超时才升级为错误。
- 触及受保护不变量：否（不碰回调、不碰捕获生命周期；但改动 ReceiveEngine 命令协议，需保留“单一原生所有者”“取消不遗留孤儿准入”这组已有测试，评审要点是 `CancelRequest` 语义）。
- 建议归属：platform（receive_driver）+ desktop（session）。

### F02 [正确性] 多显示器时位置被 clamp 到 0..1，光标进入副屏即被判为“在边缘”，会误触发切换
- 位置：apps/peerglide-desktop/src/session.rs:888-899（`nx/ny` 先 `clamp(0.0,1.0)` 再判断 `distance(edge,nx,ny)<=0.001`）；crates/peerglide-platform/src/macos.rs:329-338（`Position` 用 `CGDisplayBounds(CGMainDisplayID())` 的主屏尺寸，x/y 为全局坐标减主屏原点）；crates/peerglide-platform/src/windows.rs:444-451（`e.pt` 为虚拟桌面坐标，尺寸取 `SM_CXSCREEN/SM_CYSCREEN` 即主屏）。
- 证据等级：亲自读到代码（三处逻辑）；“会误触发”是从代码推断。
- 问题：具体输入——Windows 主屏 2560×1440，副屏在其右侧（x≥2560），用户配置的对端边为 Right：光标越过主屏右缘进入副屏（x=2600）时，`nx=2600/2559` 被 clamp 成 1.0，`distance=0` 判为“在边缘”，立刻 Acquire。副屏在左侧同理会在 Left 配置下，x 为负时 clamp 成 0 触发。docs 声明“仅主显示器”，但程序没有拒绝多屏，行为是把输入送到另一台电脑。
- 最小修复：在 session 里用原始 `x,y` 判定，超出 `[0, size-1]` 范围就视为“不在边缘”（并 `edge_gate.update(false,…)`），不要先 clamp 再判定；可选地在 Position 事件里直接携带“是否越界”。
- 触及受保护不变量：否。
- 建议归属：desktop（session）。

### F03 [正确性，与已知“Mac 不能移动 Windows 光标”现象相关的假设] 回程判定的余量过窄：进入位置 0.002，回程阈值 0.001
- 位置：session.rs:1044-1062（`returned_to_source` 用 `distance(source_edge.opposite(),x,y) <= 0.001`；`entry_position` 把进入点放在 0.002 或 0.998）；session.rs:909-919（每个 Motion 先累加再判回程，成立即 `restore_at_boundary` 并发 Release）；session.rs:897、906（`position` 在 Acquire 时就设为进入点）。
- 证据等级：算术与分支“亲自读到代码”；“它造成已知症状”是猜测（无运行证据）。
- 问题：以 Mac 为源、Windows 在右侧：进入点 x=0.002，Mac 一侧按自身宽度（例如 1512 点）归一化，`dx/1512`。提交后第一个 Motion 只要 `dx ≤ -1.5` 点就满足 x≤0.001，立刻向 Windows 发 Release（带终点光标）而不是发运动数据报。这与 STATUS.zh-CN.md 记录的“Mac 摘要两次交接均没有后续移动发包”在现象上吻合：回程分支不走 `send_datagram`，`SendOk` 计数不增。ADR-0003 允许“首个反向运动即回程”，但该设计没有为提交瞬间的手抖/加速度反向留余量。
- 验证办法（不改代码）：看 KVM-DIAG 里 `return`（EdgeReturn）计数是否在两次交接后各为 1 且 `tx`（SendOk）为 0。
- 最小修复（若证实）：回程判定增加“提交后至少 N 毫秒或累计前进 ≥ 余量”的宽限；或把进入点移到更靠内（如 0.02）而回程阈值保持 0.001。
- 触及受保护不变量：否；但与 docs/ADR/0003 的“无最小行程”文字相冲突，需要 ADR 补充说明。
- 建议归属：desktop。

### F04 [未完成功能/健壮性] 短暂的系统状态变化被当作永久“捕获源丢失”，整个会话直接失败且不重连
- 位置：crates/peerglide-platform/src/lib.rs:904-921（`SourceHealth::sample` 每 ≥100 ms 调用 `context::capture_source_available()`，为假则 `gate.source_lost()`）；lib.rs:470-474（`source_lost` 置 `STOPPED` 与 `healthy=false`，之后再没有任何清除路径）；crates/peerglide-platform/src/context_macos.rs:88-93（辅助功能、输入监控缺一，或“安全输入”`IsSecureEventInputEnabled` 为真，即不可用）；context_windows.rs:189-191（输入桌面与线程桌面名称不同即不可用，例如锁屏、UAC 安全桌面）；macos.rs:218-222、232-234（循环每轮都 sample；`healthy=false` 则退出线程）；apps/peerglide-desktop/src/session.rs:859-863（通道关闭 → `bail!("native capture channel closed")`）；apps/peerglide-desktop/src/reconnect.rs:62-88（`retryable` 不包含该错误）。
- 证据等级：各处逻辑“亲自读到代码”；“用户日常遇到的频率”是推断。
- 问题：macOS 上，在任何应用的密码框里输入（或某些应用遗留了 Secure Event Input），空闲状态下也会在下一个 100 ms 采样点把整个原生捕获永久置为丢失，worker 以失败退出；Windows 上锁屏/UAC 同理。解锁或关闭密码框后不会恢复，必须手动停止再开始。docs 把它描述为“fail closed”的有意设计，所以这不是 bug，但对“日常 KVM 体验”是一个硬缺口。
- 最小修复：区分“永久丢失”（权限被撤销、tap 创建失败）与“暂不可用”（安全输入、非输入桌面）。暂不可用时：若无物理持键债务（`pairs.debt==0`）且不在 Receiving，则让 gate 进入“暂停”（保持 Local、不转发、拒绝准入），源恢复后自动恢复；有债务时仍按现状 fail-closed。
- 触及受保护不变量：是（恢复/自有注入清理、单一捕获生命周期的健康模型）。需要先写 ADR 并重新跑 G03 与接收端恢复测试，不能作为顺手改动。
- 建议归属：platform。

---

## P2

### F05 [内存/性能 + 正确性] 本地路由下每个鼠标移动都入队 Position，队列满会禁用 gate 并取消待提交的切换
- 位置：crates/peerglide-platform/src/lib.rs:1311-1337（`movement`：本地路由也 `emit`）、1342-1349（`emit` 失败即 `gate.disable()`）、1296-1310（本地滚轮同样 `emit`）；apps/peerglide-desktop/src/session.rs:603（`mpsc::channel(128)`）、888-899（Position 处理）；lib.rs:350-382（`disable` 每次调用都会把 generation 加一并清 `COMMIT_ARMED`）；lib.rs:114-116（generation 仅 21 位，耗尽即 `STOPPED`）。
- 证据等级：亲自读到代码；“高报告率鼠标下队列可能被填满”是从代码推断。
- 问题：未处于边缘时的 Position 对 session 毫无用途（只在边缘判定与 `size` 更新用到），但每个事件都唤醒 tokio 运行时。session 主循环在等待可靠流 `send`（最长 250 ms，session.rs:275-278）期间无法取队列，1000 Hz 鼠标约 128 ms 就会把 128 槽填满；一旦 `try_send` 失败，回调里 `disable()` 会使 generation 加一，待提交的 `CaptureCommitToken` 失效，表现为“切换被本机准入保护取消”（session.rs:715-724 的提示）。
- 最小修复：回调只发“在边缘附近”的 Position，或者对 Position 做“脏标记合并”（`AtomicU64` 存最新坐标、`AtomicBool` 表示已有未消费通知，仅在标记由 0 变 1 时 `try_send`）。本地路由的滚轮不应入队。
- 触及受保护不变量：不违反；但改动在原生回调内，需满足“回调内无识别/分配/IO”，只用原子操作即可。
- 建议归属：platform + desktop。

### F06 [正确性 + 性能] 亚刻度滚轮会发出 `Scroll{0,0}` 并走可靠流，且没有被过滤
- 位置：windows.rs:466-475（`WM_MOUSEWHEEL` 经 `WheelState::steps` 换算；亚 120 增量返回 0）、544-549（`wheel_steps`）；lib.rs:1296-1310（`scroll_routed` 对 x=y=0 仍 `emit`，并因 `route.remote()` 返回 true 吞掉原事件）；session.rs:932-939、937（已有 `ScrollZero` 计数）；crates/peerglide-protocol/src/lib.rs:43（`Scroll{0,0}` 合法）。
- 证据等级：亲自读到代码。
- 问题：触摸板或高精度滚轮产生的 30、60 这类增量，前几次换算后为 0，仍作为一条可靠消息发送（编码+两次写+250 ms 超时），接收端再做一次定位加一个 0 增量注入。对用户无任何效果，却占用可靠流与接收端往返；同时让“Windows→Mac 滚轮是否真的发出过有效滚动”更难从日志判断。
- 最小修复：`scroll_routed` 里当 x、y 都为 0 时不 `emit`，仍返回“吞掉”（remote 路由）。
- 触及受保护不变量：否。
- 建议归属：platform。

### F07 [正确性] 两端滚轮单位没有标定：Windows 发“刻度数”，macOS 发“行数”，接收端各自乘/不乘系数
- 位置：windows.rs:544-549（换算成每 120 一刻度）、587-600（Windows 接收注入时 ×120）；macos.rs:364-370（macOS 源直接转发 `field 11/12` 并 clamp 到 ±1200）、521-533（macOS 接收以“行”为单位注入，值不缩放）。
- 证据等级：亲自读到代码；“实际手感倍率”是推断，需实机核对。
- 问题：macOS 触控板一次快速滑动在 field 11 上可达几十，Windows 接收端会换成几十个刻度（每刻度 120），放大约数倍到数十倍；反向（Windows 滚轮 1 刻度 → macOS 1 行）手感偏小，且 macOS 的惯性/动量事件会被逐个转发。两个方向的单位语义靠约定，没有协议字段说明。
- 最小修复：不改线协议。在源端或接收端统一约定“1 单位 = 1 个机械刻度”，macOS 源端把行/像素增量除以每刻度行数并保留余量（沿用 `WheelState` 的做法）；macOS 接收端对每刻度注入固定行数（例如 3）。
- 触及受保护不变量：否（v5 帧语义不变，但属于行为变化，需要两端同时更新并更新 docs）。
- 建议归属：platform。

### F08 [未完成功能 + 正确性] 遇到映射表之外的键，就中止整个远程控制
- 位置：lib.rs:1350-1394（`unsupported_key`：禁用 gate、发 `UnsupportedInput`）；windows.rs:791-817、791-795（`VK_PAUSE`、`VK_SNAPSHOT` 直接视为不支持；其余靠 `from_scan`）、663-765（`KEYS` 表只到 0x64，缺 0x46 PrintScreen、0x47 ScrollLock、0x48 Pause、0x65 Application/Menu、0x68-0x73 F13-F24 等）；macos.rs:539-641（同样缺少）、303-319（`flagsChanged` 对 Fn/Globe 键，code 63，`from_code` 为 None → `unsupported_key`）；macos.rs:194-196（事件掩码不含系统定义事件，媒体键不会被截获）；session.rs:880-887（收到 `UnsupportedInput` 就恢复本机并释放）。
- 证据等级：亲自读到代码。
- 问题：在 MacBook 上，控制 Windows 时按一下 Fn/Globe 键，或在 Windows 上按 Menu/PrintScreen/Pause，会立刻结束控制并回到本机；macOS 媒体键则直接作用于本机而不是被控端。`Input::valid` 已允许 0x04..0x73（protocol/lib.rs:36-39），所以补充映射不需要改协议，由 `InputCapabilities` 协商即可保持向后兼容。
- 最小修复：(1) 为 0x46-0x48、0x65、0x68-0x73 补充两端映射；(2) 对 Fn/Globe 这类“无 HID 对应、不应影响对端”的键，改为“吞掉或放行本机并继续控制”，不要终止会话（需沿用现有 `Consumed` 配对账本，保证 Down/Up 对称）。
- 触及受保护不变量：补映射不触及；改变“不支持输入的处理策略”会触及配对所有权/恢复语义，需要保留 G03 的配对测试。
- 建议归属：platform。

### F09 [内存/性能] 运动数据报逐事件发送，JSON 编码，没有合并
- 位置：session.rs:909-931（每个 `Captured::Motion` 一次 `encode(&m)` 与 `send_datagram`）；crates/peerglide-protocol/src/lib.rs:96-98（`serde_json::to_vec`）、83-87（`Motion{lease,sequence,x,y}`，两个 f64 以十进制文本写出）。
- 证据等级：亲自读到代码；“字节数约 90、1000 Hz 的 CPU/网络开销”是估算。
- 问题：每个鼠标事件一次堆分配加浮点格式化加一次 UDP 包；因为是绝对位置，发 1000 个与发 125 个效果一样，却多 8 倍包与唤醒。
- 最小修复：在 session 里把运动合并为“每 4-8 ms 发送最新一个”（序号仍单调递增）；不改线格式（受 v5 约束），可选把 x、y 量化到 1/65536 再编码，文本更短且 `decode` 仍兼容。不要为此改 serde_json。
- 触及受保护不变量：否（v5 兼容保持）。
- 建议归属：desktop。

### F10 [内存/性能 + 不变量] 回调内存在潜在分配：`BTreeSet` 与 tokio `mpsc::try_send`
- 位置：lib.rs:1001-1002（`keys: BTreeSet<u16>`、`buttons: BTreeSet<u8>`）、1221、1384、1412（回调路径上的 `insert`）；macos.rs:666-692、windows.rs:832-858（初始集合同为 BTreeSet）；lib.rs:1342-1349（回调里 `self.tx.try_send`，`tx` 为 `tokio::sync::mpsc::Sender`，session.rs:603 创建）；对照 gesture_queue.rs:1-3、100-125（已有预分配 SPSC，注释写明回调内只 `try_push`）；lib.rs:2624-2670（分配计数测试夹具）仅被 gesture_capture.rs:1162、1264 使用。
- 证据等级：BTreeSet 与 mpsc 的使用点“亲自读到代码”；“BTreeSet 首次插入、tokio mpsc 跨块时会分配”是通用知识，未在本仓库核实，等级为“从代码推断”。
- 问题：AGENTS.md 要求原生回调内没有分配，但遗留（KVM）路径仍用这两种容器，且没有对应的分配回归测试，只有 G1 的 gesture 路径被测试覆盖。
- 最小修复：键与按钮集合改为定长位图（键 4×u64 存 0..255，另有未知键 1024×u64 已有）；把 `tokio mpsc` 换成已有的 `gesture_queue`（需让该模块在 macOS 也编译）加一个 `tokio::sync::Notify` 或 `AtomicWaker` 唤醒；并给 HookState 的 key/button/scroll 路径加一个 `allocation_checks::count` 断言测试。
- 触及受保护不变量：是，但方向是“恢复遵守不变量”；仍按“不新增钩子、不改捕获生命周期”的方式做，分步提交。
- 建议归属：platform。

### F11 [正确性/健壮性] macOS 事件 tap 因超时被系统禁用时，被当作永久源丢失
- 位置：macos.rs:265-278（`kind >= 0xffff_fffe` 时：非停止状态一律 `source_lost()` 并发 `Emergency`）；macos.rs:218-244（主循环没有重新 `CGEventTapEnable(tap, true)` 的路径）。
- 证据等级：亲自读到代码；“0xFFFFFFFE=超时、0xFFFFFFFF=用户输入禁用”是系统常量知识，仓库内未定义这两个名字。
- 问题：当回调所在的运行循环被占用（系统负载高、`pump` 内同步调用）使系统因超时禁用 tap，会话没有任何自动恢复，整个 Mac 端控制失效，需手动重启。
- 最小修复：对“超时禁用”：若无持键债务，重新启用 tap 并 `gate.disable()` 回到 Local，记一个计数；对“用户输入禁用”才维持当前 fail-closed。
- 触及受保护不变量：是（恢复路径语义），需评审。
- 建议归属：platform。

### F12 [正确性/可能与已知滚轮现象相关] Windows 把所有“带注入标志”的滚轮事件一律旁路，触摸板驱动注入的滚动不会被转发
- 位置：windows.rs:36-46（`mouse_origin`：`LLMHF_INJECTED` 或 `dwExtraInfo==TAG` 都算 `Injected`）、366-397（`Injected` 分支只计数，不进入转发；已有 `WheelInjected` 与 `WheelPhysical` 两个计数）。
- 证据等级：代码“亲自读到”；“精密触摸板/驱动滚动带注入标志”是通用知识，未实测，等级为猜测。
- 问题：自有注入已有 `TAG` 可区分，没有必要再把所有 `LLMHF_INJECTED` 都视为“非物理”。若用户用笔记本触摸板或某些驱动做平滑滚动，滚轮可能全部落在旁路里，源端什么都不发。
- 验证办法（不改代码）：对照 KVM-DIAG 的 `wh_phys` 与 `wh_inj`，若 `wh_inj>0` 而 `wh_phys=0` 且 `scr_tx=0`，即可确认。
- 最小修复（若证实）：KVM 路径下，仅用 `dwExtraInfo==TAG` 判定“自有”，其余注入的滚轮按物理事件转发。G1 手势路径保留现行的“注入来源”处理（AGENTS.md G03 条款不变）。
- 触及受保护不变量：与“honor injected origins”相邻，仅限 KVM 路径；需评审，不要影响 `local_gestures` 分支。
- 建议归属：platform。

### F13 [结构/可维护性] session.rs 主循环是一个 ~270 行、单行多语句、`select!` 里的巨型 async 块，rustfmt 不会格式化
- 位置：session.rs:672-943（`let result: Result<()> = async { ... loop { ... tokio::select! {...} } }.await`），其中 748-943 每个分支内联所有状态机逻辑。
- 证据等级：亲自读到代码。
- 问题：变量 `pending/outgoing/position/size/edge_gate/recovery/sequence/last_pong/local_return` 都是局部变量，无法单测单个分支；一行里夹 `if let ... { ...; send(...).await?; }`，评审与回归都困难（上述 F02、F03 恰好都藏在这里）。
- 最小修复：新增 `struct SessionState`，把每个 `select!` 分支提取为 `async fn on_tick/on_message/on_datagram/on_captured`，保持现有测试通过、行为不变，分多个小 PR。
- 触及受保护不变量：否。
- 建议归属：desktop。

### F14 [结构/可维护性] lib.rs 混合了门状态机、会话、HookState、配对账本，并与 macos.rs/windows.rs 通过 `#[cfg]` 交织
- 位置：lib.rs:104-521（`CaptureGate`）、536-746（`CaptureSession`）、748-865（`NativeReceiveBackend`）、992-1472（`HookState`）；lib.rs:1212 之后约 1800 行为内联测试；macos.rs:163-264 与 windows.rs:133-304 是近乎相同的“采样 → pump → keep_running → 消息泵”循环；macos.rs:157-160、windows.rs:112-115 重复。
- 证据等级：亲自读到代码。
- 问题：`HookState` 里同时有 `#[cfg(windows)] local_gestures` 字段与 Windows 专属分支；两个平台文件各自重复启动/退出序列，改一边容易漏另一边。
- 最小修复：机械拆分为 `gate.rs`、`hook_state.rs`、`session.rs`（同一 crate、不改公共 API），把测试移到 `tests/` 子模块；循环骨架提取为 trait（`NativeLoop::sample/pump/wait`）。注意 AGENTS.md 要求小 PR，不要一次提交。
- 触及受保护不变量：否（纯移动），但评审要核对“只移动不改变量”。
- 建议归属：platform。

### F15 [内存/性能] 设备插图的解码结果常驻静态内存，并在上传纹理时又整体 clone 一次
- 位置：apps/peerglide-gui/src/visual_assets.rs:21-22（`static LAPTOP/MONITOR: OnceLock<Option<DecodedDevice>>`）、25-67（`ColorImage::from_rgba_unmultiplied` 保存整张图）、93（`load_texture(name, (*source.image).clone(), ...)`）；assets/devices/*.png 实际尺寸 1254×1254 RGBA（`file` 命令输出）。
- 证据等级：代码与尺寸“亲自读到”；字节数为计算值：1254×1254×4≈6.3 MB/张，两张常驻≈12.6 MB，上传时另有一次 6.3 MB 临时拷贝；GPU 纹理另计。
- 问题：纹理上传后，CPU 侧的整张解码图不再被使用（只用到 `uv` 与 `aspect`），却一直保留。
- 最小修复：解码时只保留 `uv/aspect`，把 `ColorImage` 在首次上传后丢弃（或把素材预缩小到实际显示尺寸，如 512×512，约 1 MB）。上传时不要 `clone`，改成按值移交。
- 触及受保护不变量：否（素材许可与来源说明 `assets/devices/SOURCE.md` 保持）。
- 建议归属：gui。

### F16 [内存/性能] 17.8 MB 的完整 CJK 可变字体被以三个字重各装载一份
- 位置：apps/peerglide-gui/src/main.rs:75-110（循环三次 `FontData::from_static(include_bytes!("../assets/fonts/NotoSansSC.ttf"))` 并设置不同 `wght`）；assets/fonts/NotoSansSC.ttf 大小 17,772,300 字节（`ls -la` 输出）。
- 证据等级：代码与文件大小“亲自读到”；“运行期 RSS 的具体增量”是猜测（需要实际测量）。
- 问题：二进制多出 17.8 MB；三个字重各自解析并各自生成字形图集，使用到的汉字很可能只有几千个。
- 最小修复：用 OFL 允许的方式对字体做子集化（仅保留界面用到的汉字、拉丁、符号），并保留 OFL.txt 与 SOURCE.md；若子集化后字重轴无需保留，三个字重只保留实际用到的两个。分发时遵守 AGENTS.md“字体许可证与声明随发布一起提供”。
- 触及受保护不变量：否（但字体声明须随发布，见 AGENTS.md 末条）。
- 建议归属：gui。

### F17 [内存/性能] 承载钩子的原生线程上，系统状态轮询过于频繁
- 位置：lib.rs:915-919（原生循环每 ≥100 ms 调用一次 `capture_source_available`）；context_macos.rs:68-93（每次三次系统查询：`AXIsProcessTrusted`、`CGPreflightListenEventAccess`、`IsSecureEventInputEnabled`）；context_windows.rs:172-191（`OpenInputDesktop` 加两次 `GetUserObjectInformation`）。
- 证据等级：亲自读到代码；“每次查询的开销，尤其 macOS TCC 查询是否涉及进程间调用”是猜测。
- 问题：10 Hz 的系统查询发生在承载钩子的同一线程上；其耗时直接叠加在钩子延迟上（Windows 低级钩子有超时）。
- 最小修复：在 Local 且无债务、无 Receiving 时降到 ~1 Hz；有债务或 Receiving 时保持 10 Hz。
- 触及受保护不变量：与 F04 同一模型（健康检测），仅调整频率不改语义；仍需保证“债务期间不会放宽”。
- 建议归属：platform。

### F18 [内存/性能 + 健壮性] 传输配置的数据报发送缓冲未设限（仅为通用知识，需核实）
- 位置：crates/peerglide-transport/src/lib.rs:227-238（`transport()` 只设了 `datagram_receive_buffer_size(64 KiB)`、接收窗口、空闲/保活；没有设置 `datagram_send_buffer_size`）。
- 证据等级：配置项缺失“亲自读到”；quinn 0.11 的默认发送缓冲为 1 MiB 是通用知识，本仓库内未核实（Cargo.lock 只说明版本为 0.11.12），等级为猜测。
- 问题：如果默认值如此，链路拥塞或 Wi-Fi 卡顿时，运动数据报会在发送侧累积到 1 MiB（约万级），恢复后以旧位置回放，光标“拖尾”。
- 最小修复：把 `datagram_send_buffer_size` 设为 ~16 KiB（与 F01、F09 配合）。
- 触及受保护不变量：否（QUIC 传输参数，不改 ALPN/帧）。
- 建议归属：transport。

---

## P3

### F19 [内存/性能] 回调固定成本里有三处按“整表”扫描
- 位置：lib.rs:1071-1073（`mouse_debt()` 每个鼠标移动回调都遍历 256 项 `buttons`，见 1326）；lib.rs:1092-1096（`suppressed_modifiers()` 遍历 `unknown: [u64;1024]` = 8 KiB，被 `consumed_modifiers_held()` 在每次 `guard_failure` 中调用，见 receive_driver.rs:724、1005）；macos.rs:329-330（每个本地移动都调用 `CGDisplayBounds(CGMainDisplayID())`）；windows.rs:437（远程路由每个移动调用 `GetCursorPos`）。
- 证据等级：亲自读到代码；耗时为估计，没测量。
- 最小修复：`PairLedger::replace` 已维护 `debt`，同样维护 `mouse_debt` 与 `unknown_count` 两个计数；macOS 的屏幕边界缓存并在显示配置变化时刷新；Windows 记住 `pointer(0.5,0.5)` 的停靠点。
- 触及受保护不变量：否（只读缓存/计数，不改所有权规则）。
- 建议归属：platform。

### F20 [内存/性能] macOS 循环使用 `returnAfterSourceHandled=true`，每个事件都会跑一遍“采样→泵→借用 STATE”
- 位置：macos.rs:235-243。
- 证据等级：亲自读到代码；“影响大小”是推断。
- 最小修复：保持语义，必要时在事件密集时批量处理或改为只在定时器/唤醒源触发时才执行 `sample/pump`。
- 触及受保护不变量：谨慎（捕获循环结构），需评审。
- 建议归属：platform。

### F21 [内存/性能] desktop worker 使用默认的多线程 tokio 运行时
- 位置：apps/peerglide-desktop/src/main.rs:181-182（`#[tokio::main]`，workspace 的 tokio 特性含 `rt-multi-thread`，见根 Cargo.toml）。
- 证据等级：亲自读到代码；“线程数与内存增量”是推断。
- 最小修复：`#[tokio::main(flavor = "multi_thread", worker_threads = 2)]`；不要用 current_thread，因为会话里有阻塞式 stdout 写入（main.rs:296、349、387 的 `std::io::stdout().lock()`）会拖住 I/O。
- 触及受保护不变量：否。
- 建议归属：desktop。

### F22 [内存/性能] 可靠流帧读写有冗余的小分配与两次写
- 位置：crates/peerglide-transport/src/lib.rs:284-292（先 `write_u32` 再 `write_all`）、317-336（先读 1 字节再读 3 字节，再 `vec![0;size]`）；session.rs:275-278（外层 250 ms 超时）。
- 证据等级：亲自读到代码。
- 最小修复：把长度前缀与正文合并成一次写；接收端复用一个 4096 字节缓冲。注意：超时取消发生在两次写之间会留下半个帧，目前依赖“一旦错误就结束会话”，不要改变这一点。
- 触及受保护不变量：否（帧格式不变）。
- 建议归属：transport。

### F23 [内存/性能] GUI 在原生会话期间固定 10 Hz 重绘
- 位置：apps/peerglide-gui/src/main.rs:1350-1370（`request_activity_tick`：`child.is_some() && active_mode==Native` 就 `request_repaint_after(100ms)`）；注释 1351-1353 说明了稳定通信期已改为由输出唤醒。
- 证据等级：亲自读到代码；docs 记录过 Mac 空闲 CPU 已从 39.7% 降到 1.43%（DEVELOPMENT_PLAN.md 记述，未复测）。
- 问题：Native 在线时窗口隐藏或最小化也在以 10 Hz 跑 `logic`；每次都会走完整 `ui`。
- 最小修复：窗口隐藏/最小化时把周期放宽到 500 ms，健康轮询由 worker 事件与 `child.try_wait` 的较低频率承担。
- 触及受保护不变量：否（仅 GUI 刷新策略；保留“退出/停止/恢复”轮询）。
- 建议归属：gui。

### F24 [结构/可维护性] `App` 是一个约 100 个字段的“上帝对象”，主文件 3443 行里约一半是测试
- 位置：apps/peerglide-gui/src/main.rs:421-640（`struct App` 字段定义），1710 起为测试；desktop_ui.rs、workspace_ui.rs 都是 `impl App`。
- 证据等级：亲自读到代码。
- 最小修复：先提取 `WorkerSession`（child、events、worker_state、stopping、logs、input_state、switch_state），再提取 `Pairing`、`Layout` 子状态，使 UI 函数只借用所需子结构；测试移入 `tests/` 目录。
- 触及受保护不变量：否（GUI 配置存储 `config_store`/`enrollment_store` 的有界版本化与锁语义不能动）。
- 建议归属：gui。

### F25 [未完成功能] 数据协作与 G1/G2 相关 crate 尚未接入任何可执行程序
- 位置：crates/peerglide-transfer（约 2781 行）在 apps/*/Cargo.toml 中没有被引用（对 `peerglide-transfer` 的 grep 仅命中它自己的 Cargo.toml）；peerglide-gestures/actions 只被 GUI 用作“预览/规则/手势实验室”；peerglide-core/src/gesture_broker.rs 与 windows.rs:116 的 `pub fn capture(...local_gestures...)` 在生产路径无调用（lib.rs:938-969 只用 `capture_receiving`）；gesture_capture::capture 也没有应用侧调用。
- 证据等级：亲自读到（grep 与 Cargo.toml）。
- 说明：这与 docs（STATUS.zh-CN.md“尚未实现”）一致，不是遗漏，只是提醒“编译与测试成本已在，但零运行收益”。如需缩短 CI，可把它们放进可选 feature；功能推进顺序仍按 DEVELOPMENT_PLAN。
- 触及受保护不变量：G1 保持关闭，不要因为“接线”而打开。
- 建议归属：其他。

### F26 [未完成功能] 发现模块只处理 IPv4；输入仅限主显示器
- 位置：crates/peerglide-discovery/src/lib.rs:5、39-47（`Ipv4Addr`、仅保留私有/链路本地 IPv4）；windows.rs:656-661（绝对移动不带 `MOUSEEVENTF_VIRTUALDESK`）；macos.rs:431-466。
- 证据等级：亲自读到代码。
- 说明：与 docs 的“多显示器/多设备待实现”一致，记录以便排期；F02 是它在现状下的安全问题。
- 建议归属：其他。

---

## 与已知两个现象的关系（线索，不是结论）

1. Mac 不能移动 Windows 光标：F03 的回程余量算术与“两次交接后没有后续移动发包”的现象一致，可用 KVM-DIAG 的 `return` 与 `tx` 计数核实；F01/F05 给出另外两种不依赖该假设的候选（接收端往返积压、本地队列溢出取消提交）。未读到足以确定根因的证据。
2. Windows→Mac 滚轮无效：先用 `wh_phys / wh_inj / scr_tx / scr_zero / scr_apply` 区分（F12、F06）；F07 说明即使发送成功，macOS 侧每刻度只有 1 行，手感很可能“几乎不动”。

## 建议的小 PR 次序（每个都应独立评审，遵守 AGENTS.md 的小分支/小 PR 约定）

1. F02（越界不判边缘）、F06（不发 0,0 滚动）：改动小、测试易写、无受保护项。
2. F03 证实后再改；先由现有诊断核实。
3. F01 + F09 + F18：接收端“只取最新”与发送端合并，一起验证延迟。
4. F10：把回调内容器换为定长位图/预分配队列，并补分配回归测试。
5. F04/F11/F17：健康模型调整，需先写 ADR 并跑 G03 与恢复测试。
6. F15、F16：GUI 内存。
7. F13、F14、F24：结构拆分，分步、只移动不改行为。

## 未覆盖范围（未读或仅略读）

- 完全未读：crates/peerglide-platform 的 gesture_capture.rs、gesture_recovery.rs、context_actions*.rs、context_events_*.rs、context_watch.rs、tray*.rs、application_menu*.rs；crates/peerglide-transfer 全部；crates/peerglide-actions 全部；crates/peerglide-gestures 全部；crates/peerglide-core/src/gesture_broker.rs；crates/peerglide-protocol 的 worker.rs、pairing_worker.rs、layout_ipc.rs、capabilities.rs；crates/peerglide-transport 的配对（OPAQUE）部分与身份存储测试；apps/peerglide-desktop 的 pairing_worker.rs、layout_control.rs；apps/peerglide-gui 的 workspace_ui.rs、desktop_ui.rs（仅 grep）、pairing_panel/pairing_supervisor/pairing_receipt/enrollment_store/config_store/resource_check/worker_permissions（仅 grep 与片段）、gesture_lab、gesture_rules、layout_updates。
- 仅部分阅读：platform/lib.rs（1-1473 行非测试部分已读；测试 1474-3014 未读）；receive_driver.rs（1-1073 行已读，测试未读）；session.rs（1-1063 行已读，测试未读）；desktop/main.rs（1-780 行已读）；gui/main.rs（60-130、330-480、860-930、1100-1160、1330-1700 行已读）；discovery/lib.rs（1-140 行）；context.rs、context_macos.rs（60-140）、context_windows.rs（150-245）。
- 所有测试代码、docs 中除 ARCHITECTURE/ROADMAP/CAPTURE_LIFECYCLE/DEVELOPMENT_PLAN 末段与 STATUS.zh-CN 前 60 行之外的文档均未读（VALIDATION.md、LOCAL_CODEX_HANDOFF.zh-CN.md 也未读）。
- 没有任何运行或构建结果；所有“性能/内存”结论都需要实际测量（建议沿用 resource_check 的 CPU/RSS 采样）。
