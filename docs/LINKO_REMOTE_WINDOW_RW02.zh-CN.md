# Linko 远程窗口：详细技术方案、RW02 实现与原生接线契约

状态：**已提交可编译的 RW02 离线协调代码及故障回归；原生采集、媒体传输和产品界面仍待实现。** 不能将本文件理解为“完整远控已完成，只缺真机测试”。

本文件为本轮研发入口。仓库为 `ForceMind/Linko`；实现 PR 为 [#286](https://github.com/ForceMind/Linko/pull/286)，分支 `feat/remote-window-rw02-coordinator`。依赖 [#69](https://github.com/ForceMind/Linko/pull/69) 的 `a72355445e0596b300dd7f78351cfa43882c4a36`，不是从 main 复制第二份 lab。研究选型见 [#274](https://github.com/ForceMind/Linko/pull/274)。不自动合并 #69、#274、#180 或 main，不改 MIT、现有原生输入 owner、v5 协议和 eframe。

## 1. 目标、范围与成熟技术的使用方式

### 1.1 产品要解决的问题

应用继续在来源电脑运行；查看端显示来源窗口的画面，并在另外授权后通过原有输入系统操作来源电脑。它不是跨操作系统迁移进程，也不把应用安装、账号和未保存内容搬到目标电脑。

第一条闭环是“显式选择窗口 → 来源授权观看 → 查看端显示 → 另行授权整机控制 → 停止并确认清理”。第二条闭环才是“显式发送窗口 → 可恢复的来源停放 → 返回来源”。自然边缘拖动、虚拟显示器、手机、多面板、公网均在相应基础验收后增量接入。

默认镜像且来源窗口保持可见。代理窗口关闭默认停止分享，不关闭来源应用。分享单窗口画面不意味着能安全地将所有键鼠输入限制在该窗口；本次协调器一律拒绝 `WindowOnly`，只读和明确的 `WholeDesktop` 控制分开提供。

### 1.2 借鉴对象与取舍

详细源码记录已在 #274 固定上游提交，而不是只依据演示视频：

- [研究与证据](https://github.com/ForceMind/Linko/blob/72e58568ae9319e46ee0e636d445dd2ac2fcfdb5/docs/research/LINKO_REMOTE_WINDOW_RESEARCH_2026-11-13.zh-CN.md)：TwinDesk `38ed66f05e35e77215b5f3db49e6f4b2d8ea6336`；Viewflow `762fcc9c9d1827e3adda9fc07e09a3129b8c411b`。
- [增量架构](https://github.com/ForceMind/Linko/blob/72e58568ae9319e46ee0e636d445dd2ac2fcfdb5/docs/research/LINKO_REMOTE_WINDOW_ARCHITECTURE_2026-11-13.zh-CN.md)。
- [RW01–RW09 与 A01–A18 验收](https://github.com/ForceMind/Linko/blob/72e58568ae9319e46ee0e636d445dd2ac2fcfdb5/docs/research/LINKO_REMOTE_WINDOW_EXECUTION_2026-11-13.zh-CN.md)。

TwinDesk 用于研究窗口发送、代理展示与回退交互；Viewflow 用于研究窗口/几何/媒体/输入的代次及恢复边界。既有研究没有把它们认定为可直接移植的成熟产品内核。本次 Rust 代码基于 Linko 已有模块独立编写，没有拷贝上游 GPL 源码，也没有把“放到子进程”当作免除许可义务的办法。

成熟基础设施与待验证集成必须区分：系统采集 API、WebRTC 媒体基础设施可以作为组件候选；Linko 的 SDK 构建、硬件编码适配、帧元数据贯通、断线恢复和 UI 集成仍是自己的工程任务。

### 1.3 实际交付对照

| 范围 | 本次状态 | 代码或后续门槛 |
| --- | --- | --- |
| 单查看端、多来源的会话/视图协调 | 已实现离线 Rust | `coordinator.rs` |
| 独立观看/控制授权、代次失效 | 已组合接入 | 复用 `SessionGate`，不生成生产授权 |
| 有界原始帧缓存与展示收据 | 已实现离线 Rust | 复用 `LatestFrame`，新建一次性 `PresentationTicket` |
| 单目标输入、释放屏障、空闲检查 | 已组合接入 | 复用唯一 `InputRouter`，不新增原生钩子 |
| 窗口停放/恢复事务协调 | 已实现命令模型 | 默认禁止停放，效果仍由未来原生后端确认 |
| 连接级故障广播、清理后注销 | 已实现离线 Rust | `close_binding`、`close`、`retire` |
| Windows/macOS 单窗口采集 | 未实现本轮原生适配 | RW03 |
| WebRTC/C++ bridge、编解码、网络媒体 | 未实现 | RW04 |
| 原生输入/展示/窗口管理接线 | 未实现本轮接线 | RW05 / RW09，原有 KVM 工作保持独立 |
| GUI、多面板、手机、公网 | 未实现 | RW06–RW08 |
| 进程崩溃后的持久恢复账本 | 未实现 | 来源停放模式上线前的阻断门槛 |
| 双机、性能、兼容性验收 | 未执行 | C06 与原生/媒体矩阵分别记录 |

## 2. 架构与代码布局

```text
已存在的可信设备身份 / 授权 UI / 唯一输入运行时
                       │ 可信本地决策、单调时钟、明确生命周期事件
                       ▼
                Coordinator (RW02)
       ┌───────────────┼──────────────────┐
       ▼               ▼                  ▼
  SessionGate     LatestFrame        InputRouter
  view/control    1个待显示原始帧     1个活动目标
  各自代次        +1个展示收据        +释放确认屏障
       │               │                  │
       └───────────────┼──────────────────┘
                       ▼
                  Handoff 模型
          准备 / 请求停放 / 远端 / 请求恢复 / 本地
                       │
             后续原生、媒体、GUI适配器
      （当前lab没有捕获画面、建立网络或注入输入）
```

| 路径 | 责任 |
| --- | --- |
| `labs/remote-workspace/src/coordinator.rs` | 本次新增的统一生命周期和副作用意图协调 |
| `labs/remote-workspace/tests/coordinator_faults.rs` | 本次新增的跨模块故障集成测试 |
| `labs/remote-workspace/src/lib.rs` | 导出 `coordinator`，其余错误/身份类型保持兼容 |
| `session.rs` / `routing.rs` / `media.rs` / `handoff.rs` | 既有策略、输入、缓存和恢复模型，直接复用 |
| `geometry.rs` / `scheduling.rs` | 既有几何与资源调度基础；生产资源调度接线仍待完成 |
| `labs/remote-workspace/README.zh-CN.md` | 执行命令、能力边界与本文件入口 |

本次不创建第二套 GUI/权限库/全局键鼠 hook，不改变主工作区依赖。实验包保持独立 Cargo workspace、独立 lockfile、零第三方依赖和 `forbid(unsafe_code)`。不要直接把含集合、分配和遍历的协调器放进原生输入回调。

## 3. 状态、身份和代次设计

### 3.1 标识分工

| 标识 | 来源 / 生命周期 | 阻止什么 | 不证明什么 |
| --- | --- | --- | --- |
| `Binding` | 现有可信运行时给定 source、viewer、连接 nonce | 不同来源/查看端/连接数据混用 | 字节非零不等于完成密码认证 |
| `SurfaceKey` | 来源本地注册表给定对象 ID 与 generation | 同一 ID 的新窗口重用旧上下文 | PID/窗口标题不能代替对象生命周期验证 |
| `ViewId` | 本协调器分配，不回收重用 | 迟到回调命中新注册视图 | 不是网络授权 token，不能跨进程持久化 |
| `view_epoch` | `grant_view` / 撤销递增 | 重授观看后旧缓冲视频复活 | 不能绕过来源端独立校验 |
| `policy_epoch` | 观看/控制/几何策略变化递增 | 旧输入票据继续使用 | 不是原生按键已释放的证据 |
| 几何 revision | 可信平台几何变化递增 | 尺寸、DPI、旋转变化后的旧坐标 | 不等于 OS 窗口已完成调整 |
| Placement revision | 协调器对相机/面板布局变化递增 | 缩放平移后沿用旧映射 | 不修改来源系统显示器拓扑 |
| `PresentationTicket` | 单次从缓存交给 presenter 时产生 | 伪造、重复、过期或跨视图展示确认 | 持有票据不代表 GPU/屏幕已显示 |
| `HandoffToken` | 本协调器递增 attempt + ViewId | 旧事务 ACK 完成新事务 | 不认证远端；不能当跨重启凭据 |

协调器只服务一个本地 viewer，可以注册多个来源；一个协调器仅有一个 InputRouter。**跨多个远端查看者争抢同一来源桌面的全局仲裁仍由来源原有运行时/租约负责**，不能用多个各自独立的 Coordinator 冒充全局互斥。

新连接必须由可信运行时提供新 Binding 和新注册。库不维护无限历史 nonce 撤销数据库；禁止适配器把断开的旧 gate 重置后继续使用。所有迟到回调携带原 ViewId，不能通过“找同名窗口”“找当前最新连接”重新定向。

### 3.2 生命周期

`register` 创建 connected 但默认未授权的条目。`grant_view` 只放行观看；`grant_control` 是另一项明确决定，并且不能超过观看授权期限。确认新画面已展示之后才可能通过控制新鲜度检查。

`close` 对当前 ViewId 是终态：撤销、清空媒体、停止输入、取消交接。再次 close 幂等，不清除未确认恢复。`retire` 只有在连接已关闭、匹配目标的输入释放已确认、窗口事务回到 Local 后才删除条目；因此容量不足不能通过“忘记危险窗口”解决。

`close_binding` 对同一精确 Binding 的所有仍连接窗口执行 close；影响范围不能只取当前选中的面板。其他连接保持不变，已有恢复义务也不被覆盖。一个 worker 承载多个 Binding 时，监督器应枚举自己实际拥有的全部 Binding 分别关闭。

## 4. 对外接口与可信调用者

| API 组 | 可信调用者 | 关键契约 |
| --- | --- | --- |
| `register` | 已认证会话与来源注册表适配层 | 核验 source/viewer/nonce、原生对象生命周期、实际几何 |
| `grant_view` / `grant_control` | 本机授权决策层 | 用户明确同意且来源端独立同意；远端布尔值不得直接调用 |
| `revoke_*` / `close` / `close_binding` | 权限监听、会话或 worker 监督器 | 撤权、锁屏、物理接管、断连、源关闭及时送达 |
| `revise_geometry` | 来源几何/采集事件适配器 | 先登记新几何，再发送新版本帧；不允许 UI 随意声明源尺寸 |
| `relayout` | 查看端 GUI | 相机/面板改变必须关闭旧输入路由 |
| `receive` | 已认证、已解码的媒体适配层 | 只有匹配窗口、连接和观看代次的数据才能进入 |
| `take_for_present` | presenter | 一次只持有一张有效待确认票据，另行约束外部纹理占用 |
| `acknowledge_presentation` | presenter 的可信完成路径 | 不能在网络收到包、解码完成、提交 GPU 命令时提前确认 |
| `request_content_check` / `confirm_unchanged` | 已认证来源健康确认适配层 | 仅确认精确画面、精确挑战且采集健康；不是一般 keepalive |
| `focus` / `pointer` / `authorize_input` | GUI→唯一输入 owner 桥 | 成功只是预检，不等于 OS 已执行；保留原生二次核验 |
| `pending_release` / `acknowledge_release` | 现有按键账本 owner | 需要实际释放或可靠租约清理证据，不能超时自动确认 |
| `allow_parking` / `begin_handoff` | 用户同意 + 已验收的窗口能力层 | 默认关闭，不能因输入授权而隐式开启 |
| `window_intent` / 窗口 ACK | 来源窗口后端 | 立即重核验当前意图、身份和几何，实际完成后才确认 |
| `poll` | 单线程控制循环/watchdog | 空闲也调用，不能依赖新鼠标事件触发过期清理 |
| `snapshot` | GUI 状态读取 | 只读，不执行 watchdog、不做原生恢复 |

## 5. 视频路径与真实展示确认

### 5.1 当前原始帧闭环

```text
来源/解码适配器提供受信 FrameStamp
  → receive(view, frame, receive_now)
  → 单帧缓存，更新序号水位与原始接收时间
  → take_for_present(view, now)
  → Presentation { frame, ticket }
  → presenter 真正完成对应画面的展示
  → acknowledge_presentation(ticket, completion_now)
  → SessionGate 记录展示新鲜度
```

收到包和取出帧都不授予控制。每个 View 只允许一张尚未确认的展示票据；重复 ACK、已撤权 ACK、旧几何 ACK、已注销 View 的 ACK 都拒绝。接收后经过 2000ms 的帧/票据在边界处失效；从缓存晚取出不能把旧帧的等待时间清零。新观看授权重建缓存并撤销旧票据，不能因分辨率未变化而复用旧画面。

当前帧新鲜性只覆盖本地模型收到/展示/来源确认语义。它没有跨机器时钟校准和真实采集时间证明。生产媒体桥必须携带来源帧标识、采集状态、媒体代次，并防止网络长缓存被重新标记为“刚采集”；对无法可靠关联的帧仅允许只读或停止，不能放行控制。

`poll` 还会回收没有被 renderer 消费的过期队列帧，不能只在取帧时丢弃。库无法回收已经交给外部 renderer 的纹理；撤权路径必须显式取消展示、遮盖旧画面和回收外部引用。

### 5.2 静止画面

静态文档可能不产生新帧。使用 `request_content_check` 产生一次性挑战，并由经过认证的来源确认：采集仍健康，目标对象/观看授权/几何没有变化，指定画面确实仍然有效。随后调用 `confirm_unchanged`。挑战超时、重复消费、更新画面或重授权限都使旧挑战失效。

本地定时重绘、TCP/QUIC 心跳、媒体包重复到达都不能刷新这个证明。暂停、最小化、受保护内容或来源 SDK 无法确认健康时，不能把画面未变化猜测成有效。

## 6. 输入切换与恢复屏障

A 持有输入时选择 B，首先停止 A 的路由并产生 ReleaseRequest。B 即使已授权也必须等待 A 的可靠清理确认；B 未授权也不能继续把用户动作发给 A。对 ReleaseRequest 的任意等待超时仅用于显示警告，永远不等于释放成功。

```text
focus(A) → 输入票据 A
选择 B → A 路由立即关闭 → ReleasePending(A)
现有输入 owner 执行自己的按键/按钮释放
owner 确认实际清理 → acknowledge_release(exact_request)
重新显式 focus(B) → 输入票据 B
```

相同目标重复 focus 不重置事件序号。迟到的 A 输入不允许影响 B；重复旧释放 ACK 不允许清除新的释放屏障。`pointer` 使用协调器私有保存的当前 Placement 与 SourceGeometry，调用者不能把旧几何包装成新输入。

空闲 watchdog 会在授权过期、画面失效或策略版本变化时关闭输入。单调时钟倒退同样停止路由且保留清理义务。原生 owner 仍负责物理接管、注入标记、真实按键账本、系统权限及最终执行前检查；不要另起一套捕获 hook 或在媒体 worker 内注入键鼠。

## 7. 窗口交接与恢复事务

### 7.1 默认不隐藏

`parking_allowed=false`。仅在用户独立同意、平台停放方法已确认持续可采集、来源恢复入口可靠时允许启用。它在观看重授、几何变更及终态故障时清除。不具备这一前提的版本保持镜像，产品界面不能承诺“已把源窗口移走”。

### 7.2 事务时序

```text
begin_handoff → Preparing（源窗口保持可见）
目标真实呈现且有效控制授权 → destination_ready → HideRequested
原生后端重核验当前意图并完成停放 → acknowledge_hidden → Remote
取消/失焦/切目标/撤权/断线/几何变化 → RestoreRequested
原生后端确认原窗口恢复至可用显示区域 → acknowledge_restored → Local
```

准备阶段超时：没有提出隐藏要求，可回 Local。隐藏请求发出后超时：即使尚未收到成功回包，也必须假设隐藏可能发生，保持 RestoreRequested。隐藏迟到 ACK 不能重新进入 Remote。恢复失败或无明确结果时持续显示“恢复未确认”；禁止删除条目或开始覆盖它的新事务。

输入释放屏障存在时不能启动新停放。失焦或切目标时，旧活动目标的停放事务取消；来源恢复和输入释放是两条独立义务，必须分别确认。

### 7.3 原生 adapter 不得省略的检查

`WindowIntent` 是待执行意图，不是可永久缓存的指令。执行 HideSource 前重新取得当前意图并确认完整 token 仍相等，同时校验原生对象未重建、实际系统权限、会话/授权/几何和源捕获健康。来自旧队列的 HideSource 在撤权后不得执行。

恢复不能盲目把原 Rect 写回已经拔掉的显示器，也不能把复用的 HWND/CGWindowID 当作原应用窗口。来源窗口已经退出时应报告明确的关闭/不存在结果；不能移动新窗口。原生完成和“已提交 API 请求”必须分开记录。

**本次尚无持久恢复账本或独立恢复服务。** Coordinator 进程崩溃会失去内存状态。产品启用停放前还需要来源本机恢复记录、资源所有权、启动修复与崩溃实验；未完成时必须保持停放关闭。测试中的模拟 ACK 不构成此项证据。

## 8. 失效矩阵

| 事件 | 视频/展示票据 | 输入 | 窗口 | 条目生命周期 |
| --- | --- | --- | --- | --- |
| grant_view 重授 | 清空旧队列与旧票据、换观看代次 | 撤旧控制、释放屏障 | 取消旧事务，停放许可清除 | 保留 |
| revoke_control | 观看不自动撤销 | 关闭并要求释放 | 取消旧事务 | 保留 |
| revoke_view | 立即清空并禁止新帧 | 关闭并要求释放 | 保留恢复要求 | 保留 |
| 几何变化 | 旧版本全部失效 | 旧票据/坐标失效，重新授权控制 | 取消且关闭停放许可 | 保留 |
| 相机/面板改变 | 不假造来源新帧 | 旧映射失效，重新聚焦 | 取消相关交接 | 保留 |
| 展示超时/静止确认失败 | 不刷新新鲜度 | watchdog 停止 | 必要时要求恢复 | 保留 |
| 连接/worker故障 | 清空所有所属 View | 关闭活动路由 | 保留每个窗口恢复要求 | close 终态 |
| 物理接管/锁屏/权限丢失 | 保守关闭当前条目 | 保留清理义务 | 保留恢复要求 | 需要新注册/同意 |
| 用户停止 / 失焦 | 不自动扩大观看权限 | 路由立即关闭 | 取消活动目标交接 | 保留 |
| retire | 不允许残留内部媒体 | 必须已确认释放 | 必须 Local | 删除且 ViewId 不重用 |

GUI 必须显示 connected、viewing_allowed、control_policy_ready、input_active、release_pending、handoff 和 fault 的区别。不能把“策略允许控制”显示成“正在控制”，也不能把 connected 显示成“已经授权”。

## 9. 生产原生方案：明确接入步骤，不冒充已实现

### 9.1 RW03：Windows/macOS 本机只读单窗口预览

Windows 候选采用 Windows.Graphics.Capture。官方 `IGraphicsCaptureItemInterop::CreateForWindow` 能为指定窗口创建捕获对象；此 API 的最低系统要求不等于 Linko 全部产品功能的最低系统要求。[官方接口](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createforwindow)。

Windows 切片：显式选源 → 本地注册表绑定 HWND 与实际对象生命周期 → 建立捕获对象和 D3D 缓冲 → 在专用线程处理帧、关闭、尺寸与设备丢失 → 仅在本机预览。失败不自动扩大到整个显示器，不静默提升权限，不绕过安全桌面。CPU 回读仅作为可测量的诊断路径，不能据此声称零拷贝或低 CPU。

macOS 候选采用 ScreenCaptureKit；Apple 示例给出 `SCShareableContent`、单窗口 `SCContentFilter(desktopIndependentWindow:)`、SCStream 和输出帧状态处理。[官方示例](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos)。注意当前示例涉及新 SDK 能力，工程最低版本须按实际用到的 API 单独核对，不直接照抄示例的所有配置。

macOS 切片：明确系统授权 → 用户选 SCWindow → 固定单窗口 filter → 专用队列接收有效完整帧 → 从实际 contentRect/scale 产生 geometry revision → 本机预览。不能写死 Retina=2，不能因窗口缩放而让旧帧与新坐标混用。权限撤销、停止、源退出和输出错误必须驱动 close/geometry 事件。

放行结果：每个平台仅允许声明本机预览已验证；还不能声明双向跨机器可用。记录普通窗口、遮挡、最小化、全屏、混合 DPI、源关闭及权限拒绝的实际结果。

### 9.2 RW04：原生 libwebrtc worker 验证与局域网观看

选型保留原生 libwebrtc 候选，避免首版自研全套实时媒体可靠性与拥塞处理。[官方 factory 接口](https://webrtc.googlesource.com/src/+/refs/heads/main/api/create_peerconnection_factory.h) 接受视频 encoder/decoder factory，但不会自动保证我们需要的系统硬件编解码已完成接入。

必须先固定 libwebrtc revision、DEPS 和构建工具版本，完成 Windows/macOS 原生构建、依赖与分发许可清单、ABI 和异常边界检查。不得把 main 浮动 URL 当生产依赖锁定，也不把本文件当成已批准的协议 ADR。

建议窄桥：Rust 负责状态与权限；C++ worker 负责媒体线程、WebRTC 对象和编解码生命周期；平台捕获/编解码资源留在所属原生线程。跨边界只传版本化句柄、明确所有权的帧引用、错误码和有限状态事件，不跨 FFI 暴露 Rust 容器或 C++ 对象布局。Windows 验证 Media Foundation/D3D11，macOS 验证 VideoToolbox；硬件不可用时明确拒绝或经预算批准的软件回退。

先合成视频自环，后本机捕获桥，最后真实双机。初始观看目标为 1080p30 SDR 单窗口；它是工程目标不是已测结果。透明窗、HDR、应用音频、4:4:4、HEVC/AV1 后置，避免与首闭环捆绑。

信令通过现有设备身份认证的独立控制会话。新增控制 ALPN、媒体版本、最大消息/会话预算和兼容退路需要正式 ADR；不把 SDP/视频塞进旧 v5 input reader。绑定 source/viewer/connection、SurfaceKey、view_epoch、media_generation、方向和 DTLS fingerprint；旧描述或被替换指纹必须拒绝。媒体握手成功本身不能代替设备信任。

帧关联至少包含来源对象代次、观看代次、几何版本、媒体代次和单调帧序号。RTP timestamp 回绕、SSRC/track 重建和解码输出乱序都要测试；不能仅拿短 frame ID 当唯一身份。不能把 raw RGBA 的“覆盖旧帧”直接套用到压缩参考帧。

worker IPC 只允许能力、开始/停止、几何更新、帧/展示关联和健康状态等窄命令；默认无监听，显式启动。验证对端进程/会话和版本，限制消息及共享缓冲，禁止任意文件路径、脚本或命令执行。worker 退出通过 close_binding 关闭其所有相关 View，不能静默恢复控制。

### 9.3 RW05：接入已有输入 owner

保持现有 v5 输入协议和唯一捕获/注入生命周期。协调器的 authorize/pointer 只是提交前预检；桥接层还应核验源端租约、实际权限、目标和本机物理接管状态，并使用原账本记录与清理拥有的合成按键。

先提供明确的整机控制，界面显示“正在控制来源电脑”，不宣传窗口沙箱。多 View 可以观看，只有一个输入焦点；多个 viewer 的来源侧控制权需要全局租约仲裁。C06 实机输入验收不能被本实验的绿色 CI 替代。

### 9.4 RW09：显式交接，再做自然拖窗

来源可见的窗口分享通过后，实现“发送到设备/返回来源”。只有经过持续采集与恢复验证的平台组合才允许停放；否则保持源窗口可见。加入有所有权约束的恢复日志、原生窗口代次与显示器热插拔修复，完成 GUI/媒体/来源进程崩溃实验。

最后将修饰键+拖动/边缘意图接入这个已验证事务。自然拖窗仅是触发层，不能另造绕过恢复的快捷路径。虚拟显示驱动和 macOS 私有显示接口保持可选单独评审，不作为原生窗口分享的必需项。

### 9.5 RW06–RW08：多面板、手机与公网

多面板在已授权 Surface 上建立布局；先接入现有 scheduling 模型与真实 GPU/内存预算，不承诺无限并发。当前 Coordinator 每注册项一个 View；同源多 View 的安全解码共享尚未在它内部接通。

手机先作为查看/控制客户端，不推导为 iOS 可被任意远程控制。公网先定义直连和中继策略及费用/隐私边界，默认不部署服务器、不默许文件或视频走中继；没有网络条件证据不承诺所有 NAT 均能直连。

## 10. 资源和线程约束

| 项目 | 当前模型边界 | 生产补充 |
| --- | --- | --- |
| 注册条目 | 构造时 1–16；含未清理的终态条目 | 根据可测内存/GPU预算进一步降低，不能默认16路实况 |
| 单原始帧 | 最大16MiB，尺寸还需匹配实际RGBA长度 | 生产编码、stride、plane、格式和共享内存范围校验 |
| 队列 | 每View一个待显示帧、一个有效展示票据 | 外部纹理、捕获池、decoder参考帧、在途帧独立限额 |
| 队列/展示票据期限 | 接收起2000ms，精确边界失效 | 更紧的交互延迟预算和来源帧健康证据 |
| 输入 | 每本地Coordinator一个活动目标 | 来源侧全局owner、释放/停止最高优先级 |
| 事务 | 每View最多一个未完成handoff | 持久恢复义务不得因容量/重启被丢弃 |
| 调用线程 | 单一串行控制上下文，借用规则限制并发修改 | 原生回调只入有界队列，不能阻塞做网络/分配/日志 |

仅内部待显示像素上界可估为条目上限乘16MiB，这不包括外部 renderer、解码器和临时帧，更不是整程序内存保证。生产需将 scheduling 预算实际绑定到注册、启动和重配置，保留紧急停止通路，不允许视频/文件流量阻塞输入释放。

## 11. 测试与验收

### 11.1 本次可执行测试

`tests/coordinator_faults.rs` 有36项跨模块测试，加上原有53项单元测试，共89项（最终结果仍以相应提交的真实 CI 日志为准）。其中一个测试遍历6类故障，另一个执行100次授权/撤销循环；这些循环不是100次原生进程启动，也不是100次物理跨屏。

| 测试组 | 已覆盖的行为 | 未覆盖的证据 |
| --- | --- | --- |
| 默认拒绝/授权 | 观看与控制分离、WindowOnly拒绝、重授与精确到期 | 真实系统授权弹窗、源端网络鉴权 |
| 展示/媒体 | 收包和取帧不等于显示、一次性票据、原接收时限、错来源/旧代次、队列替换/过期 | 编码视频、GPU展示、网络帧元数据真实性 |
| 输入 | 目标切换释放屏障、迟到输入隔离、重复focus防重放、旧释放ACK拒绝、空闲失效 | 实际按键释放、Bluetooth/USB、IME、Secure Input |
| 几何 | 新几何与布局废除旧票据、非法几何不污染正常状态、黑边/越界拒绝 | 真机DPI、系统窗口动画、显卡重建 |
| 故障 | 6类终态故障、连接内多窗口关闭、不干扰其他连接、回调代次 | OS/worker真实崩溃、IPC卡死、断网时序 |
| 恢复 | 默认不隐藏、准备超时与隐藏超时分离、迟到ACK、重授/失焦恢复、禁止未清理注销 | 原生停放/恢复效果、显示器拔出、持久账本 |
| 资源 | 注册配额、尺寸/字节上限、100次模型周期无内部残留 | 真实GPU/系统句柄泄漏、8h压力、功耗和码率 |

与 #274 的 A01–A18 对照：本次主要落实 A03、A05、A07–A11、A13 的纯逻辑部分，以及 A18 的模型循环；不能因此勾选完整原生/网络验收。A01/A02 的密码身份、A06 的RTP关联、A12的实际恢复、A14拥塞、A15弹窗范围、A16端版本协商、A17同源多View共享还需相应实现与测试。

### 11.2 可复现命令

在干净且保留用户未提交工作的 checkout 中切换本分支，先按项目原有 preflight 核对环境。实验包不需要联网安装依赖：

```sh
cargo test --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline
cargo clippy --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline -- -D warnings
cargo fmt --manifest-path labs/remote-workspace/Cargo.toml --all -- --check
cargo run --manifest-path labs/remote-workspace/Cargo.toml --example workspace_simulation --locked --offline
cargo build --manifest-path labs/remote-workspace/Cargo.toml --release --all-targets --locked --offline
```

Rust 1.95 或更高。新测试在第一条命令内运行；旧 workspace_simulation 仍是其原有模块组合示例，不把它改称为真实远程桌面演示。实验包 CI 和原 Rust CI 都保留，不能关闭检查来获得绿色状态。

### 11.3 已观察到的候选与证据

第一候选 `caa17e9564b1368f78b3e583c480f8e394d1a18d` 的 [run 43817061806](https://github.com/ForceMind/Linko/actions/runs/43817061806) 在三平台测试、严格Clippy、模拟及release build通过，但格式检查失败；它不是全绿候选。

修正并增加故障覆盖后的代码基线 `a4f23e0178aade220f23186136ceadc0850b5a10`，实验包 [run 43817627210](https://github.com/ForceMind/Linko/actions/runs/43817627210) 已核对三平台各步骤通过。原 Rust CI [run 43817627214](https://github.com/ForceMind/Linko/actions/runs/43817627214) 单独核查；本文件落笔时尚在运行。后续文档提交也应检查自身精确 head 的 CI，不能将上述 run 写成另一 SHA 的结果。

编写环境未获得本地 Rust 工具链/可用克隆网络；编译证据来自 GitHub Actions，不虚构本地执行。最终精确 head 的状态、提交范围与结果记录到 #286 验证评论。全部 Windows/macOS 真实交互、视频性能、物理显示与来源恢复均为 NOT_RUN。

### 11.4 后续双机放行

分别记录 Windows→macOS、macOS→Windows 的系统build、GPU/驱动、屏幕/DPI、输入设备、网络、codec硬软编、完整SHA和包校验值。先单窗口1080p30目标，再增加复杂窗口/负坐标/多屏热插拔、权限拒绝、睡眠、锁屏、源关闭、拔网线与双方进程崩溃。

RW05接入后才能测完整 input-to-photon。120ms P95仅作为已有方案的初始目标，同时报告P50/P95/P99、最坏卡顿、失败率、文字质量、帧率、CPU/GPU、内存/显存和码率。8h会话与100次真实启停/恢复单独执行，不用本次内存模型循环代替。

## 12. 集成顺序、交接与回滚

本 PR 是 #69 的依赖型小 PR，差异仅包括协调代码、测试和说明。#274 是另一个文档 PR；先检查其实际合并状态再整合，不能因链接存在就假设它已在当前 branch 文件树内。#180 的并行研发不由本 PR 自动导入。

建议接手顺序：读本文和当前 lab → 核对 #69/#274/#286 与实际目标分支 → 重跑两个CI/本地preflight → 评审纯逻辑 → 按RW03原生只读、RW04媒体桥、RW05输入桥、RW09显式恢复分小PR推进。正式新增协议/依赖时先在当前仓库挑选未占用的 ADR 编号，不抢占并行编号。

恢复义务不能靠卸载/关闭标签页自动忘记。当前离线实验回滚可撤销本PR新增模块和测试及lib导出，不涉及数据、证书、配置或协议迁移；未来原生版本回滚前需先停止捕获、完成自己拥有的输入/窗口恢复和worker退出。

**本轮产物是：可以在原工程继续开发的协调器、可执行故障测试，以及把它接到真实平台的详细契约。它不是已完成的跨系统拖窗产品，也不包含任何用户机器上的部署或权限变更。**
