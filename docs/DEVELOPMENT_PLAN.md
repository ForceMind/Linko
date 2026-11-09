# Development plan

当前状态（2026-11-09）见 [功能与交付状态](STATUS.zh-CN.md)。公开诊断预览为 0.3.22-dev。双机修复先从两端摘要定位断点，再修改发生故障的环节。下文保留历史任务。

当前迭代 0.3.23-dev／f8cce5d：Mac 源端取消居中投递与连接中安全修改方向已接入同一候选。方向经原有私有管道发送，由实际 worker 检查输入归属和持键状态后确认，再更新界面和配置。新增回归、三平台真实 NoNative 进程检查、完整五项和两项 GUI 资源检查通过，仍待双机物理复测。可信发布者签名与新版视觉尚未验收，本轮不扩展数据协作或全局手势。

1. Reproducible M0 tests and native CI; fix compile/link failures.
2. Executable loopback probe and negative authentication checks.
3. Native input sessions, extended HID and shortcut mappings, permissions UX.
4. Two-way edge return, local reclaim, collision denial, held-key disconnect
   recovery on real Windows/macOS computers with Bluetooth/USB and mixed DPI.
5. Functional eframe control panel using the same runtime; explicit persisted
   public-certificate trust and independent permissions.
6. mDNS, authenticated pairing UI, backoff reconnect with fresh leases.
7. Clipboard/files with grants, size limits, separate streams and consent.
8. Installable preview after physical P0 acceptance; signing/notarization and
   Linux backend. No hosted service deployment.

Use incremental branches and PRs, fmt, clippy with denied warnings, tests and
native matrix builds. Do not force updates or change repository visibility.

## Input correctness gate (before M2 expansion)

1. Retain/retry failed native releases and fail the session visibly; add injected
   transient/permanent failure tests. Implemented in the recovery follow-up.
2. Preserve Windows high-resolution wheel remainder. Implemented; native runtime
   acceptance still pending.
3. Edge-ordering regression reproduced on protocol v2 and fixed in v3 using
   source-originated reliable release. Real desktop acceptance remains pending.
4. Define unknown-key/capability behavior; expand layout and extended-key tests.
5. Add explicit capture-thread shutdown and reclamation before in-process reconnect.
6. Replace GUI stdout-prefix state parsing with structured worker events; allow
   immediate disconnect-and-revoke.
7. Run physical Windows/macOS acceptance matrix #3. No date or completion claim
   substitutes for recorded real desktop results.

## 2026-11-01 additive execution baseline

See [PRODUCT_EVOLUTION.zh-CN.md](PRODUCT_EVOLUTION.zh-CN.md) for task IDs,
priorities, dependencies, test matrices, release slices and continuing-work rules.
The earlier steps and input correctness gate above are retained, not replaced.

Immediate work:

1. **C01 / #10:** restore macOS CI at baseline `5617438`. Run `41226383167`
   fails the GUI headless-page test with filesystem error 2; Windows/Linux pass.
   Diagnose fixture isolation and cleanup before assigning a root cause.
2. **C02–C05 / #3 #5:** unknown-key policy, capture-thread ownership and teardown,
   structured worker events/revocation, and ordered-return/recovery regressions.
3. **C06 / #3:** authorized physical acceptance with exact OS/hardware evidence.
   If hardware is unavailable, record blocked and continue only independent work.
4. **P01–P04 / #4, U01–U02 / #5:** trust/grants, discovery, authenticated pairing,
   fresh-lease reconnect, desktop lifecycle and topology slices.
5. **G01–G05 / #11:** optional local-only gestures and typed actions on the shared
   input runtime; no script engine and no remote gesture interception in G1.
6. **D01–D04 / #6, R01–R03 / #12:** separately permissioned data collaboration and
   target-bound semantic actions. Native OS file drag remains its own gate.
7. **S01–S03 / #7:** security, packaging/signing and measured release acceptance
   throughout; optional expansion must not block an accepted scoped core preview.

Source pointers: `crates/peerglide-platform/src/lib.rs`, native adapters,
`apps/peerglide-desktop/src/session.rs`, `apps/peerglide-gui/src/main.rs`,
`peerglide-core`, `peerglide-protocol` and `peerglide-transport`.
`peerglide-actions` now contains pure model/preflight logic (G01);
`peerglide-gestures` contains bounded trajectory recognition and synthetic regression
tests (G02). No action executor or gesture hook is enabled.

Each development PR states task IDs, changed behavior, protocol impact,
regression evidence, untested platforms and rollback. Keep physical acceptance
separate from CI. Do not log key/clipboard contents, introduce a second global
hook stack, silently enable imported actions, or use a docs-only change to claim
that a runtime failure is fixed. See [ADR-0002](ADR/0002-unified-input-actions.md).


## Implementation checkpoint: 2026-11-03

This table records engineering status; physical acceptance remains #3. Latest
runtime evidence and historical failures are in VALIDATION.md.

| Task | Status | Evidence / remaining gate |
|---|---|---|
| C01 | implementing | #14 fixture isolation + macOS stress passed; historical root cause #10 still open |
| C02 | implementing | #15 recovery + #19 physical/keypad mapping; full OEM/media pending; input capability handshake added |
| C03 | implemented-unverified | #16 stop/join/reservation CI passed; native resource counts/revocation pending |
| C04 | implemented-unverified | #17 structured events/revoke CI passed; native interaction pending |
| C05 | implementing | v4 reliable terminal cursor + cleanup/expiry regressions; physical fault acceptance pending |
| C06 | blocked | No authorized physical Windows/macOS desktop pair in this environment |
| P01 | implementing | Bounded material loads, key-pair and exact enrollment-intent checks; OS storage/ACL and independent grants pending |
| P02 | implementing | Opt-in untrusted IPv4 mDNS hints and explicit listener publication; actual LAN/multiple NIC/permission acceptance pending |
| G01 | implementing | Local action/rule/preflight/result coordinator plus independently consented native window Backend (#37); keyboard owned injection/global capture and real effects remain absent or unverified |
| G02 | implementing | Bounded direction-preserving logic and synthetic corpus; human corpus/physical accuracy remain pending |
| G03 | implementing | Pure model plus optional Windows shared-capture short path/SPSC wiring (product disabled); sealed Windows short-click recovery added (native-unverified); worker, keyboard ownership, macOS physical provenance and C06 remain pending |
| G04 | implementing | Offline lab, exact app-rule editor/preview, versioned settings with backup/recovery; native app-ID/focus sampler and on-click permission diagnostics implemented (#35); continuous target invalidation/global worker/overlay pending |
| D03 | implementing (isolated foundation) | Offline bounded manifests/chunks/disk resume/integrity/no-overwrite commit; network/grants/GUI/quotas absent, M4 not accepted |


Protocol v5 is the current development baseline; both peers must update. The v3
source-ordered return safety remains, with a reliable terminal cursor added.
[ADR-0003](ADR/0003-terminal-cursor-v4.md) records migration and failure semantics.

P01 input capability slice is implemented; see ADR-0004. Discovery, interactive pairing, grants beyond input and reconnect are separate gates.


P04 fixed-address trusted reconnect is implemented behind an opt-in setting.
Fresh sessions/capabilities/control state, pin-change rejection and cancellable
backoff have automatic tests. Real interface/suspend/input acceptance, address
rediscovery and interactive pairing remain pending; see ADR-0005. G1 has isolated
G01 action-model and G02 recognizer foundations; G2 remains planned. No gesture interception
is enabled by this connection work.

## Independent data increment (2026-11-04, #32)

D01/P01 now have offline text policy/backend coordination and session-local,
per-direction consent/ticket invalidation. D03 has a real-file manager with
bounded admission and late commit authorization. Isolated data framing checks
size before allocation. These are independently executable without native input.

Production authenticated data streams, persistent/global quota/history and UI
remain implementation work, not hardware blockers. No M4/native gate is closed.
See CLIPBOARD_ENGINE.md, TRANSFER_MANAGER.md and DATA_FRAMING.md. Exact candidate
CI results belong in VALIDATION.md; input v5 remains unchanged.


D03 receive-root admission now inventories legacy staging and persists immutable
bounded reservations before receiving new data (PR #33). Cooperating Managers
share a root lock, and exact explicit resume never refunds/recharges prior usage.
Cancelled/failed/published reservations survive sessions/restarts; malformed/future
storage fails closed. Aggregate durable quota is implemented; persistent detailed
history, free-space/reclamation policy, network/GUI and native media remain open.
See TRANSFER_MANAGER.md and exact-candidate evidence in VALIDATION.md.

## Oink功能与设计整合方案（2026-11-06，用户已确认）

本节回答“什么时候把Oink的功能、设计都整合进来”。用户已确认整体路线，并明确
界面参考Oink后融入屏跃、减少工程化表达。本节不是已经完成的功能列表，也不授权
提前安装服务、插件、开机启动或发布稳定版。保留上文M0–M5、G1/G2及C/P/U/G/D/R/S
任务编号，不另起工程。当前展开A/B，先交付B1设备工作区与真实状态/操作接线。

### 1. 事实基线与本次目标

- 当前运行候选0.2.4-dev/fd8c071；后续至68c7797仅补记录。main核验仍为5d947637，
  后续增量尚未合并，不能把main与候选混为一谈。
- 真实Windows↔Mac的认证通信探针已过；2026-11-06用户重新登记正确App后，Mac
  **实际worker的辅助功能/输入监控均Granted**，未检测到Secure Input。尚未启动本轮
  Native，物理C06仍全部未验收；GUI曾占6.83GB的根因仍未知。
- 现有GUI已经按用户确认的五页中文线框落地；不是尚无界面。Oink首批仅吸收资源与
  生命周期机制，尚未整合其完整设备工作区、认证短码、数据协作、原生拖拽与音频。
- Oink参考固定[3ca82127](https://github.com/oinkvm/oink/tree/3ca82127e61f9d7106a70574a82cc63566d7a572)。
  其[README](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/README.zh-CN.md)
  的产品声明、源码存在和PG实测分开。全模块覆盖/许可/取舍见REFERENCE_RESEARCH。

交付目标：先让用户的Windows+macOS、每台一个主显示器完成安全、可恢复的日常键鼠
工作，再逐版增加本机增强和Oink的协同能力。不能等音频/拖拽全部完成才交付基础版。

### 2. 整合方法与保留项

采用**选择性移植原创模块、适配现有边界、逐能力验收**：可以复用用户已获MIT许可的
Oink原创实现及算法，不要求所有内容从零重写；先筛选文件及第三方来源，记录固定SHA、
作者归属、修改和许可，再按PG接口接入。缺少完整许可链的材料先独立实现或暂缓。

保留Rust/eframe、中文五页导航、GUI最低Rust1.95、MIT、现有配置/身份与输入v5。
Oink的Tauri UI/Rust1.99、整个agent及wire协议不直接替换现有工程，避免同时存在两套
输入owner/全局hook、信任库、配置存储和退出流程。将有价值的UI状态/收据模型及布局
算法移到Rust模型层，外观/交互用现有egui实现。逐文件移植仍须在1.95编译与审查。

原生回调只做有界非阻塞投递；识别、上下文、I/O和动作均在回调外。保留逃生键、
watchdog、注入标记、不确定Down/失败Up账本、SourceLost限制与恢复未知状态。
输入、文本、图片、文件、音频、G1和G2各自默认关闭/独立同意，不照搬full-trust默认共享。

### 3. 功能覆盖与落点

| 功能域 | PG当前状态 | 整合交付及主要落点 | 批次 |
|---|---|---|---|
| 对等键鼠、四边切换、键盘跟随/滚轮 | 原生适配已实现，物理未验收 | platform/core/desktop：两向实机、安全恢复、快捷键及DPI矩阵 | A |
| 真实owner权限、后台生命周期、空闲资源 | 0.2.3/0.2.4部分已落地 | GUI worker_permissions/main、package：稳定入口、重复启停/退出与长驻证据 | A/B |
| 设备卡、引导、主题与明确能力状态 | 五页中文GUI已有，完整工作区未完成 | desktop_ui及页面模型：设备/屏幕同页、统一状态与操作回执、可访问性 | B |
| 自动发现、短码配对、信任与重连 | mDNS提示/公开信息交换/固定地址重连已有；短码未实现 | transport/protocol/desktop：认证配对、地址变化、取消/过期/身份变化拒绝 | B |
| 托盘/菜单栏、可选开机启动 | 未接通 | GUI/平台适配：真实后台owner，关闭/暂停/退出分开，启动项由用户明确选择 | B/H |
| 本机鼠标G1（PG原路线） | 规则/画布/识别/动作与上下文基础已有；全局worker未通 | gestures/actions/gesture_broker/platform/GUI：唯一捕获仲裁、真实应用与焦点、默认关闭worker | C |
| 文本、图片剪贴板 | 离线文本引擎/独立grant模型已有；系统/网络未通 | clipboard/data/transport/GUI：原生后端、来源版本、方向权限、冲突与回环处理 | D |
| 文件发送/接收、续传/历史 | 离线接收管理及持久配额基础已有；生产发送/网络/GUI未通 | transfer/data/GUI：offer/同意/发送/校验/非覆盖提交、历史、跨重启配额 | D |
| 多设备、多显示器、布局档案 | 当前一个对端、每台主屏 | core/platform/desktop/GUI：设备/屏幕ID、负坐标/DPI/热插拔、确定性归属和配置档案 | E |
| 触摸板原生手势/手感 | 基础滚轮不等于完整触摸板手势 | platform/protocol能力协商：缩放/旋转/滑动逐项支持，Windows独立验收 | E |
| 光标定位、拖尾、布局投影 | 未接通 | 有界覆盖层，复用已有输入来源，不新增hook；默认关闭 | E |
| 跨设备原生文字/图片/文件拖放 | 未实现，普通文件发送不能代替 | 平台drag/drop、transfer、Mac FileProvider：Finder/Explorer/目标应用真实投递 | F |
| 系统音频聚合、选择播放出口、音量 | 未实现 | 独立audio模块/认证数据流/GUI：背压/抖动、出口代次、断线静音恢复 | G |
| 麦克风跨机、虚拟音频设备 | 未实现 | 独立许可/OS录音权限/可选原生插件；不能由扬声器授权推导 | G扩展 |
| 安装、更新回滚、开机和可选系统服务 | 实验包已有；正式签名/更新/服务未交付 | tools/CI/OS集成：产品/架构/来源校验、事务恢复；服务另行授权 | H |
| 本地诊断、崩溃与资源报告 | 有界诊断/资源采样已有 | 脱敏状态、实际owner、受限本机报告；不默认保存dump或自动上传 | 贯穿 |
| G2、Linux及OCR/工作流 | 仍属原后续路线 | G2类型化独立授权；Linux按X11/Wayland验收；OCR/脚本不进入本次KVM主线 | 后续 |

Oink原生触摸板转发与PG本机鼠标轨迹识别是两种能力，不能混称“手势已完成”。其
显示器布局/投影也不等于视频远程桌面，本计划不新增屏幕视频流或任意远程shell。
赞助外联、默认遥测、对方信任根/更新端点/品牌资源不纳入整合。

### 4. 中文GUI与交互方案

沿用已确认的五页线框，吸收Oink“设备与屏幕为中心、能力状态可见”的组织方式。
音频完成后作为协作区的独立子页/设备详情能力呈现，未实现时不展示可开启的假开关。

```text
屏跃                              通信：已连接  输入：本机控制  [紧急恢复]
总览          设备与屏幕
设备与屏幕    [这台Mac：权限就绪]       [Windows：已核验／等待允许]
本机鼠标      ┌──Mac主屏──┐  ⇄  ┌──Windows主屏──┐
剪贴板与文件  └───────────┘     └───────────────┘
设置          [开始键鼠协同] [暂停] [断开]       [添加电脑]
              允许对方控制本机：关闭   对方允许本机：只读状态
              键鼠｜文字｜文件：分别显示当前状态与方向
              无法开始？就地显示一项具体原因及处理入口
              高级：地址／网络角色／证书／原始诊断
```

上图是**目标线框，非当前运行截图**。设备卡仅来自实际后端事实；未知就显示未知。
同一个卡片显示“已发现/未核验、已登记/离线、连接中、仅通信、可开启键鼠、控制中、
恢复中、恢复未确认”及相应操作。绿色连接不能同时代表已授权、已接管与清理完成。

主流程按以下闭环实现：

1. **首次添加**：两台打开屏跃→局域网发现→选择电脑→在一端输入另一端显示的
   一次性配对码→两端确认身份→分别选择允许的能力→摆放屏幕→明确开启键鼠。
   一次配对记住身份，正常重连不再搬证书/填路径；找不到时提供手动地址回退。
   短码尚未实现前，现有公开信息交换继续可用，界面如实标明。
2. **开始工作**：设备卡就地检查真实worker OS权限、连接和对端许可；按钮显示
   “需要本机权限/等待对端允许/可以开始”。网络监听与主动连接不再作为主流程术语。
   已准备会话仍由用户发起切换；自动重连回到本机，不恢复旧按键、控制或动作。
3. **暂停/断开/退出**：暂停撤销活跃控制；断开保留设备信任；移除设备单独撤销信任
   和功能授权。托盘接通前关闭窗口继续合作停止；接通后明确询问/设置关闭行为，
   菜单栏始终能暂停/恢复本机/退出，退出等待账本与worker收尾。
4. **G1**：独立同意→权限→录制→动作→窗口内预览→保存→单独启用全局worker。
   远控/失焦/撤权/配置变化取消旧动作；普通右键与原生拖动兼容为放行门槛。
5. **数据**：按设备/方向开启文字；文件选择→对端同意位置/预算→进度→校验→完成。
   暂停、取消、续传和撤销未来访问分开。原生跨屏拖拽必须另有真实OS投递结果。

首批设计交付包含：设备工作区、添加/配对向导、首次权限检查、控制/恢复状态、托盘
菜单五组线稿/状态说明；后续数据、G1、布局、音频各先补状态线稿再接真实后端。
不另做只有演示设备的长期第二套界面，不把原型测试当全局功能或原生效果验收。
中文采用统一组件、间距、排版、语义颜色和键盘焦点；图标旁有中文状态，不能只靠颜色。
错误保留草稿/退路；等待/取消/迟到回执绑定请求代次，旧结果不能覆盖新选择。

### 5. 排期、版本与可见交付

以下是**工程量估计，不是已承诺的日历发布日期**：按一个主要实施者持续有效研发、
复用现有代码、CI可用、两台测试机可获得估算；不假定聊天结束后仍有后台开发能力。
以确认方案并开始下一迭代为T0；工作日指实际有效研发日。首次实机发现P0、许可/签名
资源、人工验收等待会使日历时间延长，届时以可复现问题重新估计，不降低放行门槛。
下列版本只是交付切片建议，不提前升级运行版本或保证每个patch恰好对应一次PR。

| 批次/建议版本 | 用户能看到/能做什么 | 估计有效工作日 | 必须放行的条件 |
|---|---|---:|---|
| A / 0.2.x基础候选 | 同一双机身份直接继续试跑；双向键鼠、四边、停止/重连；收敛内存问题 | 3–5 | C06相应实机记录、修饰键/拖拽/滚轮/接管/断网/退出/睡眠/撤权恢复；资源原因或有证据的处理结论 |
| B / 0.3.x连接与设计 | Oink式设备/布局工作区、认证短码、权限引导、菜单栏/托盘；日常KVM体验 | 5–8 | 不搬证书可配对；旧/取消/重放结果不授信；真实后台/退出与身份变化安全 |
| C / 0.4.x本机增强 | 无需第二台电脑的本机G1、应用规则、上下文、覆盖层/暂停 | 7–10 | 单hook、独立同意、普通右键/失焦/满队列/撤权/退出恢复，实际原生动作与误触发验收 |
| D / 0.5.x数据协作 | .0文本、.1显式文件发送/接收/续传、.2图片及历史；分批交付 | 10–15 | 真实Win↔Mac系统后端/网络、独立授权、重启配额、完整性/取消/背压与输入优先 |
| E / 0.6.x多设备与屏幕 | 三台以上、每台多屏、布局档案/热插拔、光标定位；触摸板能力逐项交付 | 10–15 | 租约冲突/分区、负坐标/旋转/DPI、几何变化取消与真实触摸板矩阵 |
| F / 0.7.x原生拖拽 | Finder↔Explorer及目标应用的文字/图片/文件真实拖入 | 10–20 | 原生drop/应用接收、延迟/取消/失焦/源文件变更；仅复制，不默认执行源删除/MOVE |
| G / 0.8.x音频 | 多电脑音频到指定出口；下一切片增加麦克风/可选虚拟设备 | 10–20 | 真实双向设备、录音独立许可、抖动/设备切换/断线恢复；插件与签名资源另计 |
| H / 0.9.x发布候选 | 一致安装入口、升级回滚、签名/公证、明确支持矩阵 | 5–10 | 获授权凭据/安装方案、实物包验证/回滚、24h耐久和独立审查；稳定发布另行批准 |

**可见节奏**：A现在已具备Mac实际worker许可，可另约连续短测；不是还需等整合完
才能测试。B的首个设备页/向导线稿及实际GUI切片目标为该批前2–3个有效工作日。
A+B合计约8–13个有效工作日交付易连接的基础日常候选；A通过时即可先交付明确范围
的双机键鼠预览，窗口需保持打开等限制必须明说。C/D每个子切片完成即可单独用。

全表合计60–103个有效工作日，约12–21个有效研发周；不是要求用户等这么久才能
两台电脑工作。该总量包含当前未实现的原生拖拽、音频/虚拟设备和多屏多端，不能以
“已有参考源码”把适配、许可和真实平台验收全部算成几天。首次A/B数据会用于校正
后续工程量；音频插件/系统服务/正式签名缺资源时继续保留阻碍，不拖延已通过核心。

### 6. 下一迭代只展开A/B的必要任务

1. A1：保持同一0.2.4和身份，按Windows实际准备状态分别完成Mac→Win、Win→Mac
   短测。物理切换/按键/接管由用户操作，仅读状态元数据。问题先最小复现，不新增
   与现有捕获竞争的钩子，也不借扩大系统权限绕过。
2. A2：对发现的问题修复、完整CI/对应物理回归；建立GUI可见/隐藏及worker分账的
   RSS/可获得physical footprint/CPU曲线，采样缺失不算通过。验证至少8h基础会话与
   现有C06次数目标；没有资料不能宣布6.83GB修复。
   已补累计CPU/区间曲线与真实GUI启动的NoNative worker、peer helper分账（PR64），
   按单逻辑CPU=100%记录，缺测不填零；RSS/可选physical footprint口径独立。分账发现
   稳定通信10Hz重绘，现已接输出唤醒，保留Native健康/EOF/停止计时；首轮Mac约30秒
   CPU由39.659049%降到最终1.427281%，精确候选/平台结果见PR64及VALIDATION。
   PR65首轮Mac NoNative已通过实际AppKit隐藏→菜单显示/停止→worker回收及30秒分账；
   该程序化SDK结果不代表物理点击/Native或Windows托盘。分账physical footprint、
   8h/24h耐久仍单独补验，短时CI不定位6.83GB根因。
3. A3：日常使用前沿用既有平台存储机制，展示工作资料目录、迁移预览和冲突处理。
   现有私有tmp试跑profile不能当持久日常资料；若迁移须单独确认源/目标、保护已存
   身份且保留原副本，不重生成密钥或覆盖现有profile。
4. B1：先更新上述五组线稿/状态及当前GUI组件。把证书/网络角色下沉，保留高级回退；
   先接实际设备事实与收据，再接按钮，未实现能力明确不可用。
5. B2：在现有身份固定上增加独立认证配对握手。采用经审查的成熟PAKE实现及双端
   transcript/角色/身份绑定、短码过期/尝试限制、取消不授信、提交回执与撤销；不从
   固定ID/时间戳生成认证，不自创未审查密码学或接受任意TLS签名。
   0.3.3-dev已将现有人工登记/撤销接入可恢复本地事务（ADR-0008），完成三平台
   进程中断回归与本机中文只读提示检查；这不表示生产短码或组合协议审查已完成。
   PR56补齐真实短码认证网络实验基础：独立TCP/OPAQUE/同socket双向TLS与有界取消。
   发布依赖审计失败后固定官方修复源码资格验证，未ignore；预发布密码依赖与独立
   审计边界见ADR-0010。当前GUI关闭，下一批接私有后台worker、中文向导、原状态
   快照复查和两端事务回执；未接通这些步骤之前仍不满足“不搬证书即可配对”。
   PR57已接只读配对worker/有界私有管道，三平台进程与宿主回环通过，无GUI入口或
   Commit命令。父监督器须绑定代次/原身份配置，再完成两端事务/回执；不得将IPC
   Authenticated伪造成手工确认，退出/换目标后不得沿用旧结果。
6. B3：单owner的托盘/菜单栏和明确关闭语义。重启只恢复受许可的待命状态，原生
   开启仍按独立明确同意，不把保存偏好或通信重连当执行许可。冻结最终包后集中
   处理OS确认；固定路径本身不能保证ad-hoc跨build授权，正式签名另行批准。
   0.3.4-dev单机已验中文应用菜单、无Native回环Stop/再连、正常AppKit退出及用户
   实际Dock退出；固定a791445运行包继续双机前置核验，不把单机退出当物理C06完成。

### 7. 兼容、数据与原生依赖决策

- **输入v5保留**：不混入Oink wire/ALPN。键码、触摸板、多端的新能力先做ADR与显式
  协商；旧端无法协商时保持已有KVM或明确拒绝该能力。确需新版本时提供两端匹配包
  与迁移说明，不悄悄改变已有帧语义。
- **D首选隔离数据ALPN/认证连接**：复用同一真实设备身份与固定证书核验，先协商
  数据schema/能力/限额/方向，再使用PGD1；不向现有v5输入reader直接投PGD1。
  旧端不支持时只有数据不可用，输入仍可用；以ADR确定监听/分发、协商与两端迁移。
  独立连接仍竞争链路，必须有全局字节预算/超时/取消/限速，让输入优先得到证据。
- **持久预算**：保留既有跨Manager/重启预约账本，历史、剩余空间与回收单独补齐。
  重连、重建对象、取消namespace均不能退还仍实际占盘的预算或自动撤销持久token。
  新文本/文件/音频动作不沿用键鼠授权，来源/版本/代次变化立即失效旧票据。
- **F/G/H系统依赖**：Mac FileProvider/AppGroup/音频插件、Windows虚拟音频/服务、
  正式签名与开机启动可能需要额外开发凭据、系统安装和人工同意；逐项给出包、路径、
  必要权限和卸载/恢复方式。未获批准前只开发/测试仓库内部分，不购买、不安装。
  不绕过Secure Input、UAC、安全桌面、防火墙或系统防护；普通KVM不承诺高权限桌面。

### 8. 验收、提交与确认范围

每个可审阅增量为一个小分支/PR，实际可交付批次更新GUI/CLI可见版本并提交推送。
每次推送回读远端SHA；主线合并需用户授权并核对受测树与合并树。文档/线稿改动
不重打应用包、不升级应用版本，不强推，也不把未合并堆栈称作main已经交付。

代码批次运行fmt、严格workspace Clippy、locked测试/build及相关真实进程/停止/
重连/GUI检查，最终候选Windows/macOS/Linux、最低Rust1.95和依赖审计完整CI。
GUI/浏览器验证遵守已有授权范围；新增浏览器测试另行明确同意。当前本机按用户
决定不安装Rust，依赖CI构建并在真实平台运行匹配产物，不把工具缺失藏起来。

沿用PRODUCT_EVOLUTION第7节验收数字：包括双向500次切换、关键恢复场景100轮、
8h预览/24h正式候选耐久、G1人工正负样本及数据满负载输入优先目标。只有实际
具备的设备/场景记为已验；Bluetooth/USB/DPI/中文IME/睡眠/撤权等未运行就明确列出。
方案中的规模/排期/性能目标均不当作测量成绩；CI与范围化代码复核不充当完整独立
安全审计，Issue #10历史根因也不因近期CI成功而关闭。

每批交付明确四类：①实现且自动验证；②实现但原生未验；③尚未实现；④客观受阻。
完整整合路线已确认，当前迭代展开A/B，其余按上表顺序连续推进。
确认研发范围不等于批准系统安装、凭据采购、合并受保护主线或发布稳定版。

### 当前优先交付：实际GUI重设计（2026-11-07）

用户指出前期工作仍偏工程、UI/交互整合没有交付到位，当前立即优先B1实际界面。
chinese-device-workspace从已完整核验PR65/f4351e9分支继续；Windows PR66/3792cb2
已提交保留，其Windows早退分类为renderer_failed，相关软件GL研究尚未实施，
不让额外诊断继续挤占本次UI。目标沿Oink3ca82127的设备列表/屏幕画布/上下文操作
重构中文实际GUI，保留五页入口、既有配对/权限/Native同意/停止恢复边界。先交付
可查看的真实GUI与空/已登记/通信/只读/恢复状态，CI实际截图读取后再声明视觉验收。


### 当前迭代：Linko拓扑首页（2026-11-07，用户已确认）

用户查看PR69/bfd739c后明确要求按其重新设计。新分支linko-topology-ui沿用已核验ef12d1e实际GUI与旧设计历史；先提交00db519效果图/三张线稿/图标，再接入0.3.14-dev。唯一设计入口为docs/design/linko-v1，不再采用modern-v1的01–04色稿。当前实现六项中文导航、默认拓扑、右侧真实上下文、缩放/适配、既有方向保存和按需详情；补充恢复/待回执禁改及拒写回滚。运行检查、最终精确CI和原生PNG尚需完成后记录。两台主屏之外的多屏几何、窗口桥、移动/远程视频工作空间均不扩大到本批。

本迭代验证完成：74e442b代码通过完整五项42362134479、资源42362134407和上游42362134421；实际原生PNG6张已读，两端包已核验。交付为0.3.14-dev拓扑/导航切片；人工Win/Mac界面、物理C06和未接通能力仍独立记录，不据此扩大到窗口桥或远程视频。

后续0.3.15小批次：用户给出Linko参考图并明确要求统一项目/仓库名称。现有GitHub仓库已rename为ForceMind/Linko且同ID/main/PR保留，b071f78先提交canonical引用。当前只细化原创主屏示意、真实设备卡、右侧快捷操作与Linko新包入口，保留所有既有权限/存储/恢复机制；不引入图中示例指标或扩到远程视频。新候选等待平台CI和实际PNG，PR69独立远程实验不并入此批。


视觉验收纠正：用户否定0.3.15实图与参考图的差距，d45功能检查通过不等于视觉完成。当前0.3.16在独立linko-visual-fidelity分支真正重做设备素材、字体层级、1280×800分栏、正式设备卡与右侧分组；已有安全逻辑保留，下一验收必须读取真实应用窗口截图逐项对照。不会用参考里的第三台设备/指标/未接通开关补画面。

0.3.16当前交付：cb27bce代码的五项CI/资源/上游均过，六张真实窗口图已读，标准/紧凑主布局完整、准备/关闭弹窗可见；两端包均已核验。具体视觉改善和保留差异见VALIDATION/PR72，用户认可和物理C06仍独立，不以CI替代。后续仅沿用户实际图面反馈继续，不扩大窗口桥/远程视频。
