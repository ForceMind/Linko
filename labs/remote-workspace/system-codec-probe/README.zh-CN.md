# RW04 系统 H.264 探针

Actions 使用 Windows 的 Media Foundation 软件编码器和 macOS 的 VideoToolbox 编码一帧合成 NV12，核对 H.264 输出类型及非零字节数。VideoToolbox 同时记录实际选中的软硬件路径。探针只输出状态、路径和字节数。

这里验证系统编码入口。Windows 硬件编码、解码、WebRTC codec factory 接线、多帧低延迟、完整许可及实际 GPU 性能仍待验证。

- [Media Foundation H.264 编码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder)
- [VideoToolbox 压缩会话](https://developer.apple.com/documentation/videotoolbox/vtcompressionsession-api-collection)
