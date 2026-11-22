# RW04 系统 H.264 探针

Actions 使用 Windows 的 Media Foundation 软件编码器和解码器处理连续 24 帧合成 NV12，逐帧核对尺寸、时间戳和亮度。另开一对编解码器，按 [Media Foundation 文档](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-low-latency)开启低延迟解码。两组都报告排空前的编码、解码数和最大积压，低延迟组最多积压两帧。macOS 使用 VideoToolbox 验证一帧并记录实际选中的软硬件路径。

这里验证系统编解码入口。Windows 硬件编码、WebRTC codec factory 接线、端到端延迟、完整许可及实际 GPU 性能仍待验证。

- [Media Foundation H.264 编码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder)
- [VideoToolbox 压缩会话](https://developer.apple.com/documentation/videotoolbox/vtcompressionsession-api-collection)
