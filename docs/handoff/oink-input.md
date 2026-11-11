> 原始研究报告（2026-11-10/11，基线提交 `c199dcd`），由接手文档 `docs/HANDOFF_CLAUDE_TO_GPT.zh-CN.md` 引用。只读调查，当时没有编译或运行任何代码；行号和部分结论可能已过期，使用前先对照当前代码。文中提到的 `scratchpad/oink-src` 是本机临时目录，已不在仓库里，Oink 源码请从固定提交 `3ca82127e61f9d7106a70574a82cc63566d7a572` 自行取得。

# Oink 输入机制与 Linko 逐项对照（只读研究）

对照对象：Oink 固定提交 3ca8212（GPL-3.0，只读、未运行任何脚本或构建）；Linko 工作树 linko-unidirectional-cursor-diagnosis-3c5f18（HEAD c199dcd，只读）。
本报告全部用自己的话描述机制，Oink 只给 路径:行号 与必要标识符。没有 cargo，没有编译或运行任何代码。

路径简写：`O:` = Oink `crates/oink-input/src/`；`OA:` = Oink `crates/oink-agent/src/`；`OF:` = Oink `crates/oink-foundation/src/`；`L:` = Linko `crates/peerglide-platform/src/`；`S:` = Linko `apps/peerglide-desktop/src/session.rs`。

证据等级：**两边**（亲自读到两边代码）、**一边**（只读到一边）、**推断**（行为靠 OS 知识或代码间接推断，未经本次验证）。

---

## 0. 先说结论

1. **没有在 Oink 里找到能直接解释症状 (1) 的"缺失机制"。** Mac 位移取法与 Oink 同源（都读 CGEvent 的 MouseEventDeltaX/Y 双精度字段，字段号 4/5，见 M1），所以"Linko 读错字段"不成立。已知事实把故障缩到 Mac 侧 `tx=96` 之后、Windows 光标之前；而 Windows 端计数一份都没有，所以**现有证据不足以指认首个分歧**。
2. 症状 (1) 有两个**可被现有 /3 计数在一次实测里证伪**的候选，来源是 Oink 做了而 Linko 没做的事：
   - **候选 A（Mac 解耦）**：Oink 在远程阶段隐藏并解耦光标（`O:macos/cursor.rs:241-244`），Linko 完全不处理光标，Mac 光标留在出口边缘且仍与鼠标关联。假设：向出口方向继续推鼠标时，位移字段被边缘夹成 0，只有反向才有非零位移。这能同时解释"Windows 不动"和"约 97 个事件后触发回程"。**仅为推断，Oink 注释只证明"冻结（解耦）后位移字段不受边缘影响"（`O:macos/cursor.rs:6-7`），没有证明关联状态下会被夹零。** 判据见第 3 节：`mv_pos` 远小于 `mv_neg`，且 `zero` 很高。
   - **候选 B（Windows 在钩子线程上自注入）**：Oink 的注入跑在独立注入线程（`OA:threads/inject.rs:1-5`），Linko 的 `SendInput` 与 `WH_MOUSE_LL` 钩子在同一线程（`L:windows.rs:189-190,217-219,656-660`）。**推断，无任何证据表明它出问题**；因为 Mac 的 `grant=1`，而 Windows 只有在入口 `recovery.pointer(...)` 成功后才回 Granted（`S:` 的 Acquire 分支），入口那一次 SendInput 大概率成功过。判据：Windows 的 `sdk≈apply` 且 `sdk_fail=0` 而 `moved=0 unchanged>0`。
3. 症状 (2)（Windows→Mac 滚轮）：Linko 的 Mac 注入与 Oink 的区别（Line 单位、整格、无阶段、NULL 事件源、未显式设位置）**都不是已知会让滚轮完全失效的差异**；最可能的是更靠前的环节：滚轮事件由驱动软件注入（带 LLMHF_INJECTED，被 Linko 放行回本机而不转发，`L:windows.rs:366-397`）。Oink 对同类事件默认处理相同，但有来源识别（`O:windows/wheel_origin.rs`）。`wh_phys`/`wh_inj`/`scr_tx`/`scr_apply` 一次实测即可定位。
4. 顺带发现一个与两症状都无关的**横向滚轮方向疑点**（M10）和两处**阈值脆弱**（M5：入口 0.002／回程 0.001）。

---

## 1. 总览表（细节见第 2 节）

| # | 机制 | 能解释症状 | 证据 | 最小候选 |
|---|---|---|---|---|
| M1 | Mac 位移取法 | (1) 不太可能 | 两边 | 暂不改；可加原始位移符号计数 |
| M2 | Mac tap 位置/选项/掩码 | (1) 可能（低） | 两边 | 先看计数，不改 |
| M3 | Mac 光标冻结/隐藏/解耦 | (1) 可能（中，待证伪） | 两边+推断 | 远程阶段在泵循环里解耦并保证恢复 |
| M4 | warp 后首个位移修正 | (1) 不可能 | 两边 | 无 |
| M5 | 位置模型：相对累加 vs 绝对数据报，入口/回程阈值 | (1) 可能（低~中） | 两边 | 回程加迟滞 |
| M6 | Windows 绝对移动坐标映射/DPI/虚拟桌面 | (1) 不太可能 | 两边 | 暂不改 |
| M7 | Windows 注入线程与钩子线程 | (1) 可能（低，推断） | 两边+推断 | 只靠计数判断，暂不改 |
| M8 | 注入事件自识别 | (1) 不可能；(2) 无关 | 两边 | 可选：补 LOWER_IL 位 |
| M9 | Mac 滚轮注入（单位/阶段/位置/源） | (2) 不太可能 | 两边 | 仅在 scr_apply>0 仍无滚动时才改 |
| M10 | Windows 滚轮折算/余量/零事件/横向符号 | (2) 不太可能；横向方向另有疑点 | 两边 | 不发 0/0 滚轮；核对横向符号 |
| M11 | 驱动软件注入的滚轮 | (2) **可能（最高）** | 两边 | 用 wh_inj 证实；再决定是否转发 |
| M12 | Mac 滚轮捕获字段 | (1)(2) 无关（Mac→Windows 滚轮另论） | 两边 | 暂不改 |
| M13 | 释放屏障与重连 | (1) 无关 | 一边为主 | 无 |
| M14 | 事件队列背压 | (1) 不可能（full=0） | 两边 | 无 |
| M15 | tap 被系统停用 | (1) 可能（低） | 两边 | 加计数，不改行为 |

---

## 2. 逐项对照

### M1 Mac 位移取法

- **Oink**：普通移动事件读 `MouseEventDeltaX/Y` 双精度值，原因是慢速移动的小数部分不能丢（`O:macos/tap.rs:2759-2762`）。对"被证明是普通硬件帧"的移动，再用 IOHID 附带的加速指针子事件的位移替换路由输出（`O:macos/physical_motion.rs:111-153`，选择规则 `O:ordinary_motion.rs:59-94`）。拖动事件（左/右/其他键拖动）没有这棵树，仍用 CGEvent 位移（`O:macos/physical_motion.rs:4-9`）。游戏模式另读不加速位移字段 `EventUnacceleratedPointerMovementX/Y`（`O:macos/tap.rs:2264-2270`）。
- **Linko**：`CGEventGetDoubleValueField(event, 4/5)`，只读这一对字段，远程时直接作为 `Captured::Motion`（`L:macos.rs:322-327`）。
- **差异**：基础字段一致；Oink 另加 HID 子事件替换与原始位移流。Oink 的 HID 子事件替换是为了避开 warp 污染（M4），不是因为 CGEvent 位移"不可靠"。**这纠正了 VALIDATION.md 末节的说法**（"Oink 注释称其在部分帧不可靠"）：我读到的注释只说明拖动帧无 HID 树、以及 warp 会混进位移字段，没有"部分帧位移为零/不可靠"的陈述。
- **症状**：(1) 不太可能（字段号与 Oink 相同，且 Linko 不 warp）。
- **证据**：两边。
- **最小候选**：暂不改取法。若实测证实候选 A，再考虑改读 170/171 不加速字段（需实机比较其在边缘是否也被夹零）。
- **计数**：`zero` 与 `cap` 之比、`mv_pos`/`mv_neg`。第二轮 Mac 摘要 `zero=27/cap=97`，约 28% 为零位移，单看不能判断是否异常。

### M2 Mac tap 位置、选项与事件掩码

- **Oink**：`SessionEventTap` + 头部插入 + 默认（可改写）选项（`O:macos/tap.rs:1393-1395`）。掩码比 Linko 多：0（空）、14（系统定义/媒体键）、18-20、29-32（触摸板手势类）（`O:macos/tap.rs:243-250`）。注释强调键盘过滤留在 session 层（`O:macos/tap.rs:1375`）。
- **Linko**：`CGEventTapCreate(0, 0, 0, …)`，即 **HID 层**头部插入、默认选项（`L:macos.rs:197`）。掩码 `[1,2,3,4,5,6,7,10,11,12,22,25,26,27]`（`L:macos.rs:194`），不含 14 与手势类。
- **差异**：Linko 在更早的 HID 层截获；Oink 在 session 层。两者都是主动 tap。位移字段在 HID 层是否与 session 层语义一致，**两份代码都没有给出证据**。
- **症状**：(1) 可能（低）。若 HID 层事件的位移在光标被夹边时与 session 层不同，会落入候选 A 的机制。其余差异（手势、媒体键不被吞）不影响指针位移。
- **证据**：两边（代码事实）；位移行为差异为推断。
- **最小候选**：不改位置（改位置属于改变捕获，违背"单一捕获生命周期"的谨慎要求，需 ADR 级别论证）。先看计数。
- **计数**：同 M1/M3。无法单独证实或否定位置选择。

### M3 Mac 光标冻结、隐藏、解耦

- **Oink**：进入远程模式立即隐藏光标并解耦（`CGDisplayHideCursor` + `CGAssociateMouseAndMouseCursorPosition(false)`，`O:macos/cursor.rs:241-244`，隐藏部分 `:83-110`），再由一个透明的非激活面板接管光标所有权，避免光标在应用窗口上方露出（`O:macos/cursor_owner.rs:1-12`）。远程期间不 warp，因为 warp 会被算进下一个位移（`O:macos/cursor.rs:3-6`）。冻结在出口边缘时"事件位置仍被夹在边上，位移字段不受影响"（`O:macos/cursor.rs:6-7`）。这些同步 WindowServer 调用不在 tap 回调里执行，而是推迟到捕获线程循环（`O:macos/tap.rs:1-3,683-780`）。退出钩子保证恢复（`O:macos/cursor.rs` 的 `install_exit_hooks`，在 `:50`、`:79` 处调用）。
- **Linko**：Mac 侧没有任何隐藏、解耦、warp（在 `L:macos.rs` 与 `L:lib.rs` 检索 `CGAssociate`、`CGWarp`、`HideCursor` 无命中）。远程阶段靠回调返回 NULL 吞掉鼠标事件（`L:macos.rs:376-379`）。开始远程时 Mac 刻意**不**发绝对移动，理由是"macOS 读原生位移，避免绝对移动污染下一事件"（`L:lib.rs:630-635`，`:661-663` 同理）。回程用 `restore_checked` 发一次带 TAG 的绝对鼠标移动（`L:lib.rs:721-740`，`L:macos.rs:431-466`）。
- **差异**：Oink 解耦+隐藏；Linko 关联+可见+靠吞事件。
- **症状**：(1) 可能（中），**待证伪**。机制假设（推断，未验证）：关联状态下光标被夹在屏幕边缘时，事件位移字段反映夹后实际位移，向出口方向推鼠标得到 0，只有向内（反向）才有非零位移，于是 Windows 侧累计位置不增加，反向回拉到 ≤0.001 即触发回程。这与第二轮 Mac 摘要吻合但不是证明：`cap=deq=97, tx=96, return=1, zero=27`。反例：若 `mv_pos` 很大，则候选 A 被否定。
- **证据**：两边（代码）+推断（OS 行为）。
- **最小候选**（不违反不变量）：在**捕获线程的泵循环**（`L:macos.rs:213-232` 的循环，回调之外）进入/离开远程阶段时调用 `CGAssociateMouseAndMouseCursorPosition`，并在 stop、Emergency、source_lost、进程退出各路径保证恢复为关联；不新增钩子、不在回调内做 IPC。风险：崩溃时遗留"解耦"状态，需要 Oink 式退出钩子与启动时自检，要先评估是否与 AGENTS.md 的"保留本机恢复"相容。**先不改，等计数。**
- **计数**：`mv_pos`/`mv_neg`/`zero`/`cap`。预测（若候选 A 成立）：一次"持续向右推、不回拉"的实测里 `mv_pos≈0` 或 `zero≈cap`，且 `return` 只在回拉时出现。`moved/unchanged` 在 Mac 恒为 0，不能证明 Windows 光标状态。

### M4 warp 后首个位移修正

- **Oink**：一整套 `WarpDeltaFix`：warp 或自己投递的绝对移动会被系统算进下一个硬件移动事件的位移（量 = 目标 − 上一个 HID 事件位置），所以回到本机后第一个事件要修正；包含"warp 尚未对该事件生效"的判定、最多等 2 个事件（`MAX_PENDING_EVENTS`）、冻结时投递移动的折入与扣除、时间戳早于 warp 的事件整体吞掉（`O:warp_delta.rs:1-45,102-195`；调用点 `O:macos/tap.rs:2123-2200`）。
- **Linko**：Mac 源端不 warp；回程的绝对移动发生在本机路由内，之后第一个物理移动走 `Captured::Position`（绝对位置，`L:macos.rs:328-338`），不使用位移，所以不受该污染。远程阶段开始时有意不发绝对移动（`L:lib.rs:630-635`）。Windows 作源时则在开始时 `pointer(0.5,0.5)`（`L:lib.rs:633-636`）。
- **差异**：Linko 的结构规避了这个问题，代价是依赖"远程阶段开始前没有残留的程序位移"。
- **症状**：(1) 不可能。第一次跨屏时没有任何此前的合成移动。极端情形：先 Windows→Mac（Mac 作接收端注入了上千个绝对移动，`sdk=1256`），再立刻 Mac→Windows，残留位移也已在 Local 阶段的 `Position` 路径上被消耗。
- **证据**：两边。
- **最小候选**：无。
- **计数**：无直接。

### M5 位置模型与入口/回程阈值

- **Oink**：普通移动是**相对位移、可靠流、带合并**：输入线程把一个窗口内的 dx/dy 累加成一条，遇到非移动消息先冲刷（顺序屏障），受速率预算限制（`OA:threads/input/driver.rs:2146-2200,2202-2247`）。只有游戏模式的累计原始计数走数据报（`oink-routing/src/input.rs:158-163`，`is_datagram` 仅对 `RelativeMotion` 为真）。目标侧按自己的几何换算。
- **Linko**：源端把 Mac 位移除以**源屏尺寸**累加成 0..1 的归一化绝对坐标，夹到 [0,1]，经 **QUIC 数据报**发送（`S:909-931`，累加在 `S:912`）；离散输入走可靠流并携带当时的光标快照修复丢失（`S:932-940`）。入口位置固定为 `0.002`（对侧）、回程判定为到达源边缘 `≤0.001`（`S:1044-1062`）。
- **差异**：(a) Oink 可靠+相对，Linko 数据报+绝对，所以 Linko 对数据报路径（`tx_unsupported/disabled/oversize/closed`）有额外依赖；(b) 入口与回程之间只有 0.001 的余量：以 1512 点宽的 Mac 为例约 1.5 点。**入口后累计位移只要向回 1.5 点即回程**，并且 `tick` 分支在输入队列为空时也会据此回程（`S:757-762`）。这对"起手轻微回抖"极其敏感，但不能解释 96 个事件之后才回程的数据，只能解释体验上"拉不动"。
- **症状**：(1) 可能（低~中）。M3 的候选 A 如果成立，M5 的阈值就是把"推不动"放大成"立刻回程"的另一半。
- **证据**：两边。
- **最小候选**：回程加迟滞：进入远程后，累计位置先越过入口向内一个阈值（如 0.01）才允许回程判定；或要求回程判定前至少过去数百毫秒。属 session 层逻辑，不动线上协议。**须先用计数确认确有"过早回程"再改，上一轮"取消居中投递"的改动已经证明不看数据猜根因会无效。**
- **计数**：`return`、`tx`、`mv_pos`/`mv_neg`、`deq`。`deq=tx+return` 的恒等关系说明那一次回程由第 97 个事件触发；若 `mv_pos` 占绝对多数而仍回程，则不是方向问题。

### M6 Windows 绝对移动：坐标映射、DPI、虚拟桌面

- **Oink**：`MOVE|ABSOLUTE|VIRTUALDESK`，坐标以**像素**为单位，先在虚拟桌面范围内用 `normalize` 反算最小满足 `floor(n*extent/65536)==pixel` 的 n（逐像素在 Windows 上核对过）；虚拟桌面原点/尺寸每次从 `SM_XVIRTUALSCREEN` 等读取，线程在 PMv2 下（`O:windows/inject.rs:4-5,38-49,139-146`；`O:windows/mod.rs:515-535`；进程级 PMv2 见 `O:windows/dpi.rs:1-35`）。
- **Linko**：`MOVE|ABSOLUTE`，**无 VIRTUALDESK**，`dx=round(x*65535)`、`dy=round(y*65535)`（`L:windows.rs:649-661`）。捕获线程设为 PMv2（`L:windows.rs:144`）。没有读取任何虚拟桌面尺寸。
- **差异**：Linko 的 0..1 映射到**主显示器**；Oink 映射到整个虚拟桌面且保证像素精确。单主屏机器上两者都能移动光标，仅有 ±1 像素量化差异。多屏机器上 Linko 只能覆盖主屏，不是"完全不动"。
- **症状**：(1) 不太可能。入口放置就调用同一函数，成功才会回 Granted。
- **证据**：两边。
- **最小候选**：暂不改。若 Windows 为多屏或非 100% 缩放且计数显示 `moved>0`，再核对。
- **计数**：Windows 的 `sdk`、`sdk_fail`、`moved`、`unchanged`。`moved/unchanged` 每 250ms 采样一次，用 SendInput 前后各读一次光标（`L:lib.rs:795-815`）；SendInput 返回后立即读 `GetCursorPos` 是否反映新位置**未经验证**，若光标由系统异步提交，`unchanged` 可能偏高。结论需与用户肉眼观察交叉。

### M7 Windows 注入线程与 LL 钩子是否同线程

- **Oink**：注入在独立线程，原因是原生注入可能阻塞（`OA:threads/inject.rs:1-5`）；钩子线程上收到重入时用钩子自有状态，不等待队列（`O:windows/hook.rs:399-401`，`O:windows/mod.rs` 的 `on_hook_thread` 分支）。普通发送还有"受保护安装"下经服务助手中转的路径（`O:windows/mod.rs:132-146`），Linko 无此路径。
- **Linko**：接收引擎的 `pump` 与 `WH_MOUSE_LL`/`WH_KEYBOARD_LL` 钩子在**同一捕获线程**，`SendInput` 在该线程的消息循环里直接调用，注释承认"原生效应可能同步重入钩子"（`L:windows.rs:189-190,215-219`，`receive_pointer` 在 `L:windows.rs:649-661`）。
- **差异**：线程模型。
- **症状**：(1) 可能（低，纯推断）。"自己线程上的 LL 钩子会被自己的 SendInput 重入或阻塞"是否会导致注入失败，**我没有任何本次证据**；Windows→Mac 方向 Windows 侧在同一线程上钩子工作正常，而 Granted 的前置条件是入口 SendInput 成功，所以更可能是后续环节。
- **证据**：两边（线程结构）+推断（后果）。
- **最小候选**：不改结构（风险大，且触及单一捕获生命周期）。只用计数判断。
- **计数**：Windows 的 `filtered`（钩子看到的被放行的注入移动数；`L:windows.rs:366-381`）应接近 `sdk`。若 `sdk` 大而 `filtered≈0`，注入没有走到钩子，值得怀疑；若两者都大而 `moved=0`，更像系统层丢弃或光标被他处拉回。

### M8 注入事件自识别

- **Oink**：Windows 双 TAG（普通 `0x4F494E4B`、形状刷新 `0x4F494E53`），钩子分四类：自己、标记、物理、外部（`injected || lower-IL`）（`O:windows/inject.rs:24-31`；`O:windows_external_input.rs:3-21`）。macOS 用事件源 userData（`MAGIC`，`O:macos/inject.rs:53`），另外用 `EventSourceUnixProcessID==0` 判物理（`O:macos/tap.rs:1849-1860`），并按"光标属主代标记"区分内部移动（`O:macos/cursor_owner.rs:50-60`）。
- **Linko**：Mac 每个注入事件在提交前写 `EventSourceUserData=TAG`（`L:macos.rs:393-402`），回调首先放行 TAG 事件（`L:macos.rs:279-292`），不看 PID。Windows 带 `dwExtraInfo=TAG`，钩子对 `LLMHF_INJECTED` 或 TAG 的事件一律放行并只计数（`L:windows.rs:36-46,366-397`），不检查 `LLMHF_LOWER_IL_INJECTED`（Oink 的 lower 位）。
- **差异**：Oink 额外按 PID 与 lower-IL 区分外部注入；Linko 只靠标志位与 TAG。
- **症状**：(1) 不可能（TAG 机制双向对称，Windows→Mac 正常）；(2) 无关。
- **证据**：两边。
- **最小候选**：可选，补 lower-IL 位判断（Windows 文档上该位与 INJECTED 同时置位，收益很小）。
- **计数**：`filtered`。

### M9 Mac 滚轮注入（单位、阶段、连续标志、位置、事件源）

- **Oink**：`CGEventCreateScrollWheelEvent2(source, unit, 2, y, x, 0)`，单位按线上定义取 Line 或 Pixel；事件源是带 userData 的 HIDSystemState 源；有阶段时才设 `IsContinuous=1`、ScrollPhase、MomentumPhase；显式设置事件位置与修饰键；小数部分按轴累积，阶段结束清零（`O:macos/inject.rs:1006-1033`，阶段映射 `:1322-1331`）。线上语义：`dy>0` 为视图向文档顶部移动，注入时原样用（`O:macos/inject.rs:12-14`，`OF:input.rs:163-166`）。
- **Linko**：`CGEventCreateScrollWheelEvent(NULL, 1(Line), 2, y, x)`，整格；无阶段、无 `IsContinuous`；不显式设置位置；随后 `submit` 写 TAG 并发到 HID tap（`L:macos.rs:521-531,396-402`）。可变参数声明与 i32 实参的调用约定正确。
- **差异**：事件源（NULL vs HIDSystemState）、位置、IsContinuous、小数累积（Linko 在 Windows 端折成整格，不在 Mac 端累积）。
- **症状**：(2) 不太可能。Mac 端的移动与按键注入走同一个 NULL 源、同一个 HID tap，且 Windows→Mac 的移动/键盘正常；滚轮是常规的整格 Line 事件。若 `scr_apply>0` 仍无滚动，则按下面候选。
- **证据**：两边（代码）；NULL 源事件在滚轮路径的实际行为为推断。
- **最小候选**：仅在 `scr_apply>0` 却无滚动时改：与 Linko 按钮分支一致，先用 `CGEventCreate(NULL)` 取当前位置并 `CGEventSetLocation`（`L:macos.rs:506-512` 已有同样写法），再考虑 HIDSystemState 事件源。
- **计数**：Mac 的 `rel_rx`、`rel_apply`、`scr_apply`（`S:811-818`），对照 Windows 的 `scr_tx`。`scr_tx>0` 且 `scr_apply≈scr_tx` 但无滚动 → 本行；`scr_tx>0` 而 `scr_apply=0` → 离散输入的 lease/回复问题（`lease`、`replay`、`decode`）。

### M10 Windows 滚轮折算、余量、零事件、横向符号

- **Oink**：钩子里把增量除以 120 作为浮点"格数"上送，高精度滚轮保留小数；横向取反（Windows 横向正值=视图向右，而线上 `dx>0`=视图向左）（`O:windows/hook.rs:1835-1837,1899-1905`）。目标 Windows 注入时 1.0 = 120，不足 1 单位累积到下次，`mouseData` 可小于 120（`O:windows/inject.rs:7-10,102-113`）。
- **Linko**：钩子里折成整格，余量 `total % 120` 留在线程局部状态，路由变化时清零（`L:windows.rs:72-86,544-549`）；`scroll_routed` 在步数为 0 时**仍发 `Scroll{0,0}`**并吞掉原事件（`L:lib.rs:1296-1310`）；Windows 注入乘回 120（`L:windows.rs:587-600`）；横向**不取反**（`L:windows.rs:471-474` 直接传 `WM_MOUSEHWHEEL` 的步数，Mac 端直接传给 axis2）。
- **差异**：(a) Linko 在高精度滚轮/触摸板上会产生大量 0/0 的可靠流消息（`scr_zero`）；(b) 横向符号。按 Oink 的约定，Windows 横向正值（向右）应取反后才是线上/CG 横向的正确符号；Linko 两个方向都没取反，Mac→Windows 方向 Mac 捕获读 axis2（`L:macos.rs:364-369`）直接作为 Windows HWHEEL。若两个系统的约定确如 Oink 注释所述，则 **Linko 两个方向的横向滚动方向都是反的**（**推断**，没有在设备上核对，也未核对 CG 轴 2 的符号）。
- **症状**：(2) 不太可能。垂直方向的符号与 Oink 一致（Windows 正值=向前=向上，CG 正值=向上）。零事件无害但冗余。
- **证据**：两边。
- **最小候选**：(a) 步数为 0 时不发 `Scroll{0,0}`（仍吞掉原事件）；(b) 实机核对横向方向，再决定是否取反。均不改线上协议字段。
- **计数**：`scr_zero` 对 `scr_tx`；`wh_phys`（Windows 钩子看到的物理滚轮事件数）对 `scr_tx`+`scr_zero`。

### M11 驱动软件注入的滚轮（平滑滚动、鼠标驱动、触摸板驱动）

- **Oink**：默认把外部（带 INJECTED 或 lower-IL 标志）的事件**放行回本机**，只有显式开启"转发注入输入"才进路由（`O:windows_external_input.rs:3-21`，`O:capture_loop.rs` 的 `forward_injected_input`）。滚轮单独做了来源识别：把 LL 钩子的滚轮与 Raw Input 报告配对，判定是物理鼠标、无设备（精确触摸板提升）还是未知，有 1 秒窗口、128 项账本（`O:windows/wheel_origin.rs:1-140`）；原生触摸板接管期间对无设备的非注入滚轮扣下不转发（`O:windows/hook.rs:2406-2423,2451-2505`）。
- **Linko**：`LLMHF_INJECTED` 滚轮放行不转发并记 `wh_inj`，物理滚轮记 `wh_phys`（`L:windows.rs:366-381,405-409`）。没有来源识别，也没有转发选项。
- **差异**：默认策略相同；Oink 有识别与可选转发，Linko 没有。
- **症状**：(2) **可能（最高）**。Windows 用户若用带平滑滚动/驱动增强的鼠标或触摸板，滚轮由驱动以注入方式产生，Linko 会把它们全部放回本机，Mac 收不到；而物理鼠标移动与按键不受影响，所以症状与"Windows→Mac 键鼠正常、滚轮不工作"吻合。这是推断，需要计数证实。
- **证据**：两边。
- **最小候选**：先证实。若 `wh_inj>0` 且 `wh_phys≈0`：第一步只告知用户换用/关闭平滑滚动做对照；是否新增"转发注入滚轮"需要单独方案与安全评审（AGENTS.md 要求不得因此放宽注入识别，防回环）。
- **计数**：Windows 的 `wh_phys`、`wh_inj`、`scr_tx`、`scr_zero`；Mac 的 `scr_apply`。判读：`wh_inj` 高、`wh_phys` 低 → 本行；`wh_phys` 高而 `scr_tx=0` → 钩子路径内部（`reject_target`/`NoLease`/`cap_stop`）；`scr_tx>0` → 看 M9。

### M12 Mac 滚轮捕获字段（Mac→Windows 滚轮，非本次症状）

- **Oink**：区分连续（触摸板/Magic Mouse，用点位移字段，带阶段）与离散（滚轮，用 FixedPt 加速行量，另存原始格步），并按 NSEvent 的"方向是否被设备反转"校正原始格步；`MayBegin` 不上报（`O:macos/tap.rs:2718-2735,2808-2860,3066-3082`）。
- **Linko**：只读整数 `DeltaAxis1/2`（字段 11/12），夹到 ±1200（`L:macos.rs:364-369`）。
- **差异**：Linko 丢弃小数、阶段、连续标志与动量。触摸板细滚动会被量化，动量事件按整数照发。
- **症状**：(1)(2) 无关。
- **证据**：两边。
- **最小候选**：不在本轮范围，记入待办。
- **计数**：`scr_tx`/`scr_zero`（Mac 作源时）。

### M13 释放屏障与重连

- **Oink**：每端只管自己写的输入流 I0，重开只有四个原因；"配对已 attach"要求上一次 detach 的按键释放已完成（`OA:streams/input.rs:1-16`，适配器说明 `:62-70`）；backlog 超过 5 秒就丢弃所有排队输入并暂停本机（`OA:threads/input/driver.rs:131-186`）。
- **Linko**：租约制：`Acquire/Granted/Release{cursor}`，Pong 1 秒超时恢复本机，接收端 `LEASE_MS=1500`，接收引擎 `COMMAND_MS=250`、`WATCHDOG_MS=1000`（`S:753-766`，`crates/peerglide-protocol/src/lib.rs:9`，`L:receive_driver.rs:77-78`）；收尾 `drain_capture` 限时 500ms。每个离散输入携带光标快照，`Release` 可带最终位置。
- **差异**：架构不同（流对 vs 租约）。Linko 的恢复更依赖计时器；Oink 更依赖"先释放再 attach"的顺序门。
- **症状**：(1) 无关（第二轮摘要是旧版 /2，没有 `pong_to/peer_rel`，所以这两条结束路径当时没被计数，但该次结束已知是 `return`）。
- **证据**：一边为主（Oink 的释放细节只读了 I0 管理与 backlog 看门狗）。
- **最小候选**：无。
- **计数**：`pong`、`pong_to`、`peer_rel`、`cap_stop`、`g_*`。

### M14 事件队列背压

- **Oink**：路由器跑在捕获线程内，回调与定时器共用一个线程，无跨线程锁；移动在驱动内合并（见 M5）。
- **Linko**：回调 `try_send` 到容量 128 的 mpsc（`S:603`），失败即 `gate.disable()` 并记 `full`（`L:lib.rs:1342-1347`，`movement` 在 `L:lib.rs:1311-1340`）；数据报队列满时丢运动，下一个绝对坐标修复（`S:921`）。
- **差异**：Linko 的"队列满=关闭捕获"更激进；Oink 是合并+5 秒看门狗。
- **症状**：(1) 不可能。第二轮摘要 `full=0`、`stale=0`、`no_lease=0`。
- **证据**：两边。
- **最小候选**：无。
- **计数**：`full`、`stale`、`no_lease`、`deq`。

### M15 tap 被系统停用

- **Oink**：回调识别 `TapDisabledByTimeout/ByUserInput`，立即重新启用 tap，使按键/按钮账本失效，核对放到回调外（`O:macos/tap.rs:1888-1913`）。
- **Linko**：`kind>=0xfffffffe` 一律视为 `source_lost` 并发 `Captured::Emergency`，会结束远程控制；**没有计数**（`L:macos.rs:266-277`）。
- **差异**：Oink 自愈，Linko 退出。
- **症状**：(1) 可能（低）。若回调被系统判超时，远程阶段会无声结束；`Emergency` 路径在 /3 计数里没有对应项（`cap_stop` 只统计能力不支持）。第二轮摘要 `return=1` 说明那次结束是边缘回程，所以不是第二轮的原因。
- **证据**：两边。
- **最小候选**：只加一个原子计数（回调里已有同类 `increment` 先例，`L:macos.rs:279-292`），不改自愈行为（自愈改动属安全生命周期，需另议）。
- **计数**：目前无。

---

## 3. 现有 /3 计数的判读决策表（一次 120 秒同步实测后使用）

前提：两端同包、同一时间窗、取两端摘要。症状 (1) 的 Windows 行从未取得。

Mac 行（作为源）：`cap/deq/tx/zero/mv_pos/mv_neg/return/pong/pong_to/peer_rel/cap_stop`。
- `mv_pos≈0`（或 `zero≈cap`）且 `mv_neg` 明显：候选 A（M3），Mac 位移被夹零。
- `mv_pos` 很大、`return` 为 1：不是方向问题，转看 Windows 行。
- `pong_to>0`：Windows 没回 Pong（lease/心跳路径）。
- `peer_rel>0`：Windows 主动 Release（含本机物理输入接管）。

Windows 行（作为接收端）：
- `rx` 近 0：数据报没到（传输/路径问题，看 Mac 的 `tx_*`）。
- `rx` 高、`lease` 高：租约/心跳（`LEASE_MS=1500`）。
- `rx` 高、`replay` 高：序号问题（`crates/peerglide-core/src/lib.rs:103-109` 的 `motion` 规则：序号必须严格大于已见水位）。
- `retired` 高：看 `g_time/g_state/g_source/g_epoch/g_mods` 哪一项最大。
- `apply≈sdk`、`sdk_fail=0`、`moved=0`、`unchanged>0`：系统层（M6/M7）。
- `filtered≈sdk`：钩子看到了自己的注入，M7 的重入担忧被削弱。
- `rx=decode+lease+replay+apply(+retired+apply_fail)` 应闭合。

Windows 滚轮：`wh_phys`、`wh_inj`、`scr_tx`、`scr_zero`；Mac：`rel_rx`、`rel_apply`、`scr_apply`。

---

## 4. 按优先级排序的候选改动清单

0. **不改代码**：用现有 /3 包做一次同步 120 秒实测，按第 3 节判读。上一轮"取消居中投递"证明在缺数据时改行为无效。
1. **（仅当 Mac 行显示 `mv_pos≈0`）Mac 远程阶段解耦**（M3）：在捕获线程泵循环（回调之外）进入远程时调用 `CGAssociateMouseAndMouseCursorPosition(false)`，所有结束路径恢复；必须同时设计崩溃/退出恢复与启动自检。不新增钩子、不改协议。
2. **（仅当 Windows 行 `sdk≈apply`、`moved=0`）Windows 注入路径复核**（M6/M7）：先核对 `VIRTUALDESK` 与主屏映射；再评估把 `SendInput` 移出钩子线程是否可行（后者触及单一捕获生命周期，需 ADR）。
3. **（仅当 Windows 行 `wh_inj>0`、`wh_phys≈0`）滚轮来源**（M11）：先告知并做对照实验；"转发注入滚轮"另立方案。
4. **回程迟滞**（M5）：进入远程后需先越过入口向内一定距离才启用回程判定。仅 session 层。需 `return` 计数支持。
5. **不发 `Scroll{0,0}`**（M10a）：删除冗余可靠流消息，行为不变，低风险。
6. **横向滚轮方向核对**（M10b）：双机实测后再改符号。
7. **Mac 注入滚轮补位置**（M9）：仅当 `scr_apply>0` 却无滚动。
8. **tap 停用计数**（M15）：一个原子计数，诊断性质。
9. **Mac 注入移动补位移字段**（Oink 注入端做法 `O:macos/inject.rs:796-810`）：Windows→Mac 已正常，低优先，记入待办。

## 5. 需要实机补的最小证据

一次 120 秒同步窗口，同包两端，动作固定：Mac 物理鼠标从屏内向右越边，**越边后继续只向右推 3 秒，不回拉**，期间不点击；结束后取两端 `KVM-DIAG/3` 行（Mac 一行、Windows 一行）。同时让用户回答一个肉眼问题：**越边瞬间 Windows 光标有没有跳到屏幕左边缘？**（Granted 之前入口位置已调用过同一 SendInput，这一问把"放置成功、后续移动失败"与"SendInput 整体无效"分开。）

再做一次 Windows→Mac 滚轮：用一个普通 USB 滚轮鼠标（关闭驱动平滑滚动）向前滚 5 格，取同一对计数；若有触摸板，同样做一次对照。

## 6. 读到与未覆盖的 Oink 模块

已读到相关部分：`macos/tap.rs`（回调、translate、安装、模式切换）、`macos/physical_motion.rs`、`ordinary_motion.rs`、`warp_delta.rs`（至第 200 行）、`macos/cursor.rs`（freeze/hide）、`macos/cursor_owner.rs`（头 60 行）、`macos/inject.rs`（滚轮、delta、头注释）、`windows/inject.rs`（前 175 行）、`windows/mod.rs`（send 与 MoveTo）、`windows/dpi.rs`、`windows/hook.rs`（鼠标处理、滚轮、外部输入分支）、`windows/wheel_origin.rs`、`windows_external_input.rs`（前 120 行）、`capture_loop.rs`（前 100 行）、`OA:threads/input/driver.rs`（合并/发送/背压/看门狗）、`OA:threads/inject.rs`（头 50 行）、`OA:streams/input.rs`（头 80 行）、`OF:input.rs`（前 260 行）、`oink-routing/src/input.rs`（`is_datagram`）。

未读或只扫到：`macos/cursor_reveal.rs`（原生按钮与 ACK）、`macos/cursor_refresh.rs`、`macos/cursor_frame.rs`、`macos/capslock.rs`、`macos/sysevent.rs`/手势类、`macos/contacts.rs`/触摸板接触、`windows/raw_capture.rs`/`rawinput.rs`/`touchpad.rs`/`warp.rs`/`cursor.rs`（Windows 光标冻结）/`service.rs`、`pointer_capture*`、`cursor_clip.rs`、`cursor_reconciliation.rs`、`pointer_transition.rs`、`jitter.rs`、`stale_step.rs`、`session_gate.rs`、`oink-agent` 的 `drag.rs`/`commands.rs`/`faults.rs`/`rate_budget.rs`/`trail.rs`、`OF:input_transform.rs`、`oink-routing` 的路由器本体。

## 7. 没有证据支持、需要避免被当成事实的点

- "Oink 注释称 CGEvent 位移部分帧不可靠"：未找到此陈述，见 M1。
- 候选 A（关联状态下位移被夹零）与候选 B（钩子线程自注入）：都是推断，没有直接证据。
- 横向滚轮方向反了：依据是 Oink 的符号约定注释，未在设备上核对。
- NULL 事件源与 HIDSystemState 源对滚轮是否有差别：未知。
- Linko 行号来自本次会话的读取；`L:lib.rs` 与 `L:input_diagnostics.rs` 的行号在读取后，`input_diagnostics.rs` 被外部改动过（系统提示），报告未依赖其行号。
