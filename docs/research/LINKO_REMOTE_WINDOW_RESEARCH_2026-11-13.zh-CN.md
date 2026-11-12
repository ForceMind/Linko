# Linko 跨设备窗口研究与选型

核查日期：2026-11-13（Asia/Shanghai）。状态：**待评审的工程方案，不是已实现或已验收的功能**。

阅读顺序：本文 → [技术架构](LINKO_REMOTE_WINDOW_ARCHITECTURE_2026-11-13.zh-CN.md) → [实施与验收](LINKO_REMOTE_WINDOW_EXECUTION_2026-11-13.zh-CN.md)。本次仅提交原创研究和方案；不引入第三方源码、二进制、依赖、驱动或运行时配置。

## 1. 结论

保留 Linko 的 Rust/eframe、可信设备身份、QUIC/TLS、输入 v5 和唯一输入生命周期。在它们之上增加独立授权的远程画面能力，不把 TwinDesk、Viewflow 或 Deskflow 整体接成第二套产品内核。

推荐组合：

- TwinDesk：参考显式发送窗口、代理窗口、返回来源、按需启停等产品机制，不复制实现。
- Viewflow：参考 Surface/几何代次、输入租约、显示确认、资源边界及失败恢复的建模，不复制协议和源码。
- Apple/Microsoft 官方 API：实现 Windows/macOS 两端采集、编解码和呈现。
- 原生 libwebrtc：作为首选媒体传输候选，借用既有 RTP/RTCP、拥塞与丢包恢复能力；通过受监督原生 worker 接入，不把 GUI 改成 Electron。
- Linko 原运行时：继续唯一持有设备身份、授权、输入仲裁、逃生键、watchdog 和按键释放账本。

“成熟”指尽量采用已有稳定基础设施、明确接口和工程门槛；**不是宣称本组合已经集成、测过或能够免适配直接发布**。尤其 libwebrtc 原生构建、硬件 codec factory、帧元数据贯通和 eframe 呈现需要先完成技术验证。

首个可用切片是“显式选一个窗口 → 对端只读观看”，随后增加独立授权的整机级控制，再增加可恢复窗口交接。自然边缘拖窗、虚拟显示器、透明多窗口合成、移动端和公网分别放行，不捆绑成一次大重写。

## 2. 已核对的 Linko 基线与并行工作

| 入口 | 本轮观测 | 使用方式 |
| --- | --- | --- |
| main | `921ea7075b0f14d7f697e2198b2bb709cf2d07c4`；tree `1a916aa16aa19895387c0bff4ab7ba7c37c28cc4` | 本次文档提交的父基线；不能代表全部研发候选 |
| main CI | Rust CI `42446237274`，上述 SHA，completed/success | 仅证明该旧基线；不是新文档提交的 CI |
| [PR #69](https://github.com/ForceMind/Linko/pull/69) | open；head `a72355445e0596b300dd7f78351cfa43882c4a36` | 现有远程工作空间研究与离线 Rust lab，不能在 main-only checkout 中假定存在 |
| [PR #180](https://github.com/ForceMind/Linko/pull/180) | open/draft；本轮观测 head `0dfabef1dc4f9462d69ec8baf3d8de9c2d941c8b`，分支 `roadmap-integration` | 活跃集成工作；持续变化，实施前重新读取精确 SHA、差异和 CI |
| [Issue #3](https://github.com/ForceMind/Linko/issues/3) | open | Windows/macOS 输入、DPI、权限及故障恢复的真实验收仍独立保留 |

已阅读主线 AGENTS、文档索引，以及集成分支 DEVELOPMENT_PLAN、STATUS 和 #69 的实现计划。集成分支中 STATUS 的旧状态段含历史公开版本记录，不将其逐项当作最新代码缺口。#180 的集成范围也不等于 main 已合并或稳定版已发布。

本轮发现的并行工作包括 #267–#269 的 Wayland 输入、#270 的原生边沿文件发送、#271 的触控板诊断、#272 的原生能力前提和 #273 的 macOS 文件承诺。这些是不同的 PR，不被本方案取代，也不因存在 PR 就视为物理验收通过。

现有 #69 已有 `geometry/session/routing/scheduling/media/handoff` 离线基础和 RW01–RW09 编号。本方案是增量选型：**不重编号、不重建 lab、不移除原 C/P/U/G/D/R/S 路线、不调整 MIT**。

固定交接入口：

- [既有远程实现计划](https://github.com/ForceMind/Linko/blob/a72355445e0596b300dd7f78351cfa43882c4a36/docs/LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md)
- [既有远程交接](https://github.com/ForceMind/Linko/blob/a72355445e0596b300dd7f78351cfa43882c4a36/docs/LINKO_REMOTE_WORKSPACE_HANDOFF.zh-CN.md)
- [主线开发约束](https://github.com/ForceMind/Linko/blob/921ea7075b0f14d7f697e2198b2bb709cf2d07c4/AGENTS.md)
- [主线 CI 记录](https://github.com/ForceMind/Linko/actions/runs/42446237274)

## 3. TwinDesk：产品完整度高，但不能把交互声明当验收

固定核查：`InukaAsith/TwinDesk@38ed66f05e35e77215b5f3db49e6f4b2d8ea6336`。

其 README 描述 Electron 应用、平台原生 helper、Rust tdfast 输入通道、独立文件处理与 WebRTC 媒体路径。对 Linko 最有价值的是模块分工、按需启停及统一设备体验，而非替换现有 Rust/eframe 或引入另一套输入协议。[T1]

本轮实际读取 `mac/movewin.js` 和 `shared/movewin.js`：

- 跨机的不是进程。来源仍运行应用，接收端展示流化窗口。
- Mac 路径优先把来源窗口放到虚拟显示区域，否则尝试移到屏幕右侧之外。
- shared 路径中的 `touch` 会在远端操作时把窗口放回来源位置，空闲后再 park。因此“源端始终看不见且完全不受影响”并非该文件能证明的性质。
- 这两个协调模块包含定时放置、best-effort helper 请求等路径。它们不能单独构成 Linko 所需的“系统效果已确认”的恢复证据；这不是对整个上游的完整安全审计。
- 弹窗处理使用窗口/进程关系启发式。Linko 不把同 PID 自动等同为用户已批准分享的窗口族。[T2][T3]

Issue #18 于 2026-11-04 关闭为 completed，证明上游将需求标为完成；**不等于我们验证了任意 Windows/macOS 组合、隐藏采集、IME 和异常恢复**。[T4]

取舍：保留“发送窗口/来源徽标/关闭代理只返回”的体验；首次交付保持源窗口可见。将来源停放和自然拖动放在独立能力门后，不用 offscreen 坐标技巧作为默认承诺。

## 4. Viewflow：窗口系统模型值得研究，仍是实验工程

固定核查：`gfhdhytghd/viewflow@762fcc9c9d1827e3adda9fc07e09a3129b8c411b`。

README 明确其为实验性跨设备窗口合成器。它包含认证 QUIC、窗口/几何代次、输入租约、有界媒体处理、颜色与 alpha 等设计；不能据此宣称完整三平台产品或将性能目标当基准结果。[V1]

`docs/macos-window-sharing.md` 给出单独的 `vf-window-peer`：macOS 使用 ScreenCaptureKit、VideoToolbox、Core Image/Metal，列出 macOS↔Windows 的来源/接收组合。该文同时指出实机采集、输入、混合缩放、拖动/缩放和应用兼容性仍需用户验收；自动弹窗族、应用音频、IME 集成也不在该后端的完成范围。[V2]

源码检索可定位 `control.proto` 中的 geometry_epoch、presented_frame 和目标窗口字段，以及 raw_session 中按几何代次注册窗口的路径。本轮据此核对接口模型，不声称逐行审计整个仓库。[V3][V4]

取舍：吸收状态机和不变量；首版不采用 Hyprland 插件、NVIDIA/NVENC 特定链路、图集编码、跨端精确模糊或 alpha 双流。它们会扩大首批 Windows/macOS 的适配、许可和验收面。

## 5. Deskflow、Synergy 与 Looking Glass 的正确定位

| 项目 | 能力边界 | 对 Linko 的用途 |
| --- | --- | --- |
| Deskflow | 官方说明为不含视频的软件 KVM，支持键鼠、剪贴板、TLS | 研究按键兼容、切换及恢复场景；不作为远程窗口引擎 |
| Synergy | 官方同样强调键鼠/剪贴板协作，并说明商业发行与 Deskflow 的关系 | 产品易用性、布局与问题矩阵参考，不代表另一种窗口迁移技术 |
| Looking Glass | KVM 虚拟机与宿主的 GPU 直通场景；通过 IVSHMEM 共享内存传递画面相关数据 | 参考低拷贝与独立光标呈现；不当作两台独立 Mac/Windows 的网络方案 |

以上取自各项目官方资料。[D1][S1][L1][L2] 这些分类不依赖原帖作者身份或演示具体版本，本次不重新声称某条视频已经证明了某仓库某提交。

## 6. 媒体选型：为什么首选原生 WebRTC 而非自造 QUIC 视频协议

| 候选 | 收益 | 代价/边界 | 决策 |
| --- | --- | --- | --- |
| 原生 libwebrtc + 平台采集/codec | 利用既有实时媒体、拥塞控制、反馈与跨平台接入；为后续移动端保留路径 | C++ 构建/ABI、硬件编码适配、包体、许可清单和版本维护；不是零适配 | 首选，先做 RW04 技术验证 |
| Quinn + 自定义 H.264 datagram/stream | 与现有 Rust 传输技术相近，可精确控制协议 | 自行承担分片、MTU、丢包依赖、关键帧、拥塞、公网与浏览器适配 | 保留实验备选，不与首选同时产品化 |
| 直接嵌入 TwinDesk/Viewflow | 能快速看到上游能力 | 许可、运行时、身份、输入 owner 与平台耦合，不符合当前主线 | 不采用 |
| Electron 整体媒体/GUI 壳 | 可利用 Chromium 已有媒体实现 | 重建 GUI 和生命周期，扩大分发/资源面 | 不采用为 Linko 主架构 |

现有 #69 曾保留 QUIC stream/datagram 的比较选项。本方案进一步提出“QUIC 管控制与授权，WebRTC 管视频”的候选决策；需架构评审后写入正式 ADR，**本次没有改变运行时协议或锁文件**。若技术验证不过，提交对照测量与新决策，不悄悄切换自研传输。

## 7. 许可与供应链准入

TwinDesk 当前文件头为 GPL-3.0；Viewflow README 明确 GPL-3.0-only；Deskflow 显示 GPL-2.0。为保持 Linko 既有 MIT 路线，本次只研究行为和接口，以官方平台 API 独立实现，不复制、翻译或改写这些项目的受保护源码。[T2][T3][V1][D1]

不要把“放成子进程/插件”当作自动规避 GPL 的结论。GNU FAQ 指出，是否为组合程序还涉及通信语义等因素；将来需要分发 GPL 组件时另做许可证评估、对应源码及通知审查。[G1] 此处是工程准入规则，不提供特定分发形式的法律保证。

libwebrtc 根许可证是 BSD-style，但其依赖、codec、专利授权和构建选项须逐项记录；根许可证不能代表所有分发内容。首个技术验证须固定 commit、DEPS、编译参数、SDK/编译器、补丁、校验值和第三方声明，不下载来源不明的预编译库。[W1][W2]

## 8. 证据索引

除固定提交外，官方在线文档按核查日读取；实施时再次核查 OS/API 和依赖版本。本轮未运行 TwinDesk/Viewflow 二进制，未完成上游全仓审计，也没有通过视频估算性能。

- [T1] [TwinDesk README](https://github.com/InukaAsith/TwinDesk/blob/38ed66f05e35e77215b5f3db49e6f4b2d8ea6336/README.md)
- [T2] [TwinDesk mac/movewin.js](https://github.com/InukaAsith/TwinDesk/blob/38ed66f05e35e77215b5f3db49e6f4b2d8ea6336/mac/movewin.js)
- [T3] [TwinDesk shared/movewin.js](https://github.com/InukaAsith/TwinDesk/blob/38ed66f05e35e77215b5f3db49e6f4b2d8ea6336/shared/movewin.js)
- [T4] [TwinDesk Issue #18](https://github.com/InukaAsith/TwinDesk/issues/18)
- [V1] [Viewflow README](https://github.com/gfhdhytghd/viewflow/blob/762fcc9c9d1827e3adda9fc07e09a3129b8c411b/README.md)
- [V2] [Viewflow macOS/window peer](https://github.com/gfhdhytghd/viewflow/blob/762fcc9c9d1827e3adda9fc07e09a3129b8c411b/docs/macos-window-sharing.md)
- [V3] [Viewflow control schema](https://github.com/gfhdhytghd/viewflow/blob/762fcc9c9d1827e3adda9fc07e09a3129b8c411b/protocol/viewflow/v1/control.proto)
- [V4] [Viewflow raw_session](https://github.com/gfhdhytghd/viewflow/blob/762fcc9c9d1827e3adda9fc07e09a3129b8c411b/crates/viewflowd/src/raw_session.rs)
- [D1] [Deskflow 官方仓库](https://github.com/deskflow/deskflow)
- [S1] [Synergy 官方介绍](https://symless.com/synergy)
- [L1] [Looking Glass B7](https://looking-glass.io/docs/B7/)
- [L2] [Looking Glass IVSHMEM 技术说明](https://looking-glass.io/docs/B7/tech_faq/)
- [W1] [WebRTC 原生 PeerConnection/codec factories](https://webrtc.googlesource.com/src/+/refs/heads/main/api/create_peerconnection_factory.h)
- [W2] [WebRTC LICENSE](https://webrtc.googlesource.com/src/+/refs/heads/main/LICENSE)
- [W3] [WebRTC VideoFrame：RTP timestamp、repeat mode、id 边界](https://webrtc.googlesource.com/src/+/refs/heads/main/api/video/video_frame.h)
- [A1] [Apple ScreenCaptureKit 单窗口过滤](https://developer.apple.com/documentation/screencapturekit/sccontentfilter)
- [A2] [Apple ScreenCaptureKit 音频范围及采集说明](https://developer.apple.com/videos/play/wwdc2022/10155/)
- [M1] [Microsoft CreateForWindow](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createforwindow)
- [M2] [Microsoft screen capture：buffer 生命周期、HDR、尺寸变化](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture)
- [M3] [Microsoft SendInput：UIPI 边界](https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-sendinput)
- [M4] [Microsoft Indirect Display Driver](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)
- [G1] [GNU FAQ：组合程序与插件](https://www.gnu.org/licenses/gpl-faq.en.html#MereAggregation)
