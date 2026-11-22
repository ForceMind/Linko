# ADR-0020 — RW04 单窗口媒体和控制面

状态：提案，等待架构、安全及分发审核。适用范围为 RW04 的首个只读局域网候选。

相关方案见 [远程窗口架构](../research/LINKO_REMOTE_WINDOW_ARCHITECTURE_2026-11-13.zh-CN.md)和[实施验收清单](../research/LINKO_REMOTE_WINDOW_EXECUTION_2026-11-13.zh-CN.md)。

## 选择

- 媒体采用固定源码的原生 libwebrtc。现有验证基线为 WebRTC `692e07f2cb2f3602af5a8083b695f4f3bc2c9a6f` 和 depot_tools `1b0972d3d5e5b1e78055147cb3576361e274b9ed`。同步与构建核对实际提交、DEPS 标识和编译参数。
- 首个候选仅承载用户明确选择的一个窗口。默认关闭，按平台复用公开的 WGC 或 ScreenCaptureKit。首轮验证目标是一路 1080p30 SDR，H.264 使用各平台系统编解码器。接收端依据实际 codec 能力协商。不支持时显示远程画面不可用。
- 媒体传输使用 WebRTC DTLS-SRTP，首轮仅启用局域网 host candidates。原型的加密回环限制在 `127.0.0.1`。公网、STUN/TURN 服务、音频、DataChannel 与输入均不在首轮范围。
- 信令拟用独立 QUIC 控制连接，复用既有已配对证书和固定身份校验。候选 ALPN 为 `linko-remote-control/1`，注册及兼容协商待审核。v5 输入/ALPN 与现有恢复 owner 保持不变。旧端继续使用既有键鼠与数据能力。
- Rust 端管理授权、生命周期和展示。原生媒体边界收敛为窄 ABI，窗口句柄留在来源端。媒体进程与呈现端的关闭必须能释放已提交资源。探针目前在同一进程，进程隔离及监督需另行实现并验证。

## 身份、授权与撤销

1. 先检查 QUIC 握手的真实设备证书与配对记录。拒绝未经登记或证书已变的设备。发现地址、ICE 候选和实验标识只作路由或测试输入。
2. 来源单独授予观看权限，绑定来源、查看端、连接、窗口实例、观看代次与期限。每次重建媒体要增加媒体代次；重新授权或重连使旧授权失效。观看授权不授予键鼠、剪贴板和文件权限。
3. 从同一份即将应用的 SDP 提取当前 DTLS SHA-256 指纹、ICE 凭据和媒体方向。与已认证控制连接上的声明以及来源 offer、查看端 answer 顺序核对。错误方向、旧代次、参数不一致或材料超限时拒绝，连接失败不回退到未认证的模式。[RFC 8122](https://www.rfc-editor.org/rfc/rfc8122) · [RFC 5764](https://www.rfc-editor.org/rfc/rfc5764)
4. 帧元数据继续走现有 RW04 模型，绑定窗口、观看和媒体代次、轨道、SSRC 与 RTP 时间戳。配对失败或过期时丢弃。实际呈现发生后才提交收据。未经认证的保活、旧画面和重绘不刷新来源有效期。
5. 主控制连接关闭、用户停止、来源关闭、权限撤销或媒体故障时，立即停止接纳旧帧和新提交，清空缓存和已显示内容，再退出对应编解码及采集工作。每个阶段独立确认完成。回滚沿用同一路径，保留已有身份、配置、键鼠恢复和数据通道。

## 构建及分发审查

- 使用同一固定构建图检查 Windows/macOS、锁定工具链、原生 ABI、编解码器能力和降级。Windows 当前固定 Chromium 工具链要求 SDK `10.0.28000.0`，现有 GitHub 官方 Windows runner 缺少它；对应 WebRTC 作业止于预检。另行批准后才能在临时 runner 修改 SDK 环境。[实际预检日志](https://github.com/ForceMind/Linko/actions/runs/45049600112/job/137291940638)
- 正式分发前，按最终实际链接的目标生成第三方组件及 notices 清单，核对每个组件的许可证、专利声明和 H.264 分发适用性。系统 API 探针成功只确认技术可用。Linko 保持 MIT，不复制 Oink 的代码或资产，依赖审计继续拒绝全部警告。[WebRTC LICENSE](https://webrtc.googlesource.com/src/+/692e07f2cb2f3602af5a8083b695f4f3bc2c9a6f/LICENSE) · [WebRTC PATENTS](https://webrtc.googlesource.com/src/+/692e07f2cb2f3602af5a8083b695f4f3bc2c9a6f/PATENTS)
- 保留单窗口限制、画面和凭据不落日志、丢帧和编解码队列限额。对尺寸、stride、断连、重建、取消、降级以及来源关闭进行故障注入。按路线图记录持续运行、反复启停、资源峰值和实际双向呈现延迟。

## 证据及放行条件

| 已做验证 | 证据 |
| --- | --- |
| Windows/macOS 托管桌面实采本机单窗口，缩放后继续，关闭清屏 | [RW03 独立预览](https://github.com/ForceMind/Linko/actions/runs/45030757462) |
| Windows 系统 H.264、macOS VideoToolbox 对合成帧编解码 | [系统 codec 实验](https://github.com/ForceMind/Linko/actions/runs/45032679504) |
| macOS 连续 H.264 视频、真实窗口渲染，以及独立回环加密收帧和新连接恢复 | [固定源码 macOS 探针](https://github.com/ForceMind/Linko/actions/runs/45049600112/artifacts/14765711373) |
| 观看信令代次、参数替换、撤权和过期负例 | [三平台离线故障回归](https://github.com/ForceMind/Linko/actions/runs/45050232648) |

Windows 的系统 codec 结果独立于 libwebrtc 工厂。托管验证覆盖单机；真实设备信令、Windows WebRTC、本机完整链路与局域网双向观看分别在候选中补证据。三端应用的既有键鼠、剪贴板和文件授权路径继续独立验收。RW05 的整机控制需通过 C06 的双机输入与恢复验收，RW04 不提前打开输入。
