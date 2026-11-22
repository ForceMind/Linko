# RW04 WebRTC 源码探针

Actions 从固定的 WebRTC 和 depot_tools 提交同步源码及 DEPS，构建一个最小的 VideoFrame 探针。程序生成 I420 测试帧，从 WebRTC API 读回尺寸、采样时间和回绕前后的 RTP 时间戳。两个平台分别上传源码、DEPS 和许可证文件的 Git 标识，以及构建参数和探针结果。仓库只保存探针和配置。

macOS 再从独立 C 文件跨过固定布局的 ABI，使用 WebRTC 的 VideoToolbox H.264 工厂对合成关键帧做本机编解码。保留单帧回归，并在同一组编解码器中验证 18 帧连续流。检查原始尺寸、亮度、关键帧与后续帧、同一会话内 RTP 回绕，以及重复时间戳、倒退时间戳和关闭后的拒绝。无效版本、空指针和超限尺寸返明确错误。

该实验调用系统 H.264。WebRTC 内建 H.264 和专有软件 codec 仍关闭。Windows codec factory、媒体网络、实际窗口帧呈现与分发许可继续按 RW04 方案推进。产物仅包含验证报告。

- [固定 WebRTC 提交](https://webrtc.googlesource.com/src/+/692e07f2cb2f3602af5a8083b695f4f3bc2c9a6f)
- [固定 depot_tools 提交](https://chromium.googlesource.com/chromium/tools/depot_tools/+/1b0972d3d5e5b1e78055147cb3576361e274b9ed)
- [现有 RW04 技术方案](https://github.com/ForceMind/Linko/pull/274)
