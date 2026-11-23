# RW04 系统 H.264 探针

Actions 使用 Windows 的 Media Foundation 软件编码器和解码器处理连续 24 帧合成 NV12，逐帧核对尺寸、时间戳和亮度。每组新开编解码会话，对照默认配置、低延迟解码、低延迟解码加单线程低延迟编码。三组都报告排空前的编码、解码数和最大积压。两端均启用低延迟的组最多积压两帧。[Media Foundation 低延迟文档](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-low-latency)

macOS 使用 VideoToolbox 验证 128×96 单帧和 90 帧 1920×1080。1080p 分别尽快输入和按 30 fps 定时输入，两组都在运行中将已编码样本交给解码器。每组使用同一对编解码器，核对时间戳、分辨率和两处亮度，记录实际软硬件路径、排空前的解码数、逐帧耗时分位数和吞吐量。

macOS 另用同一种带 IOSurface 的 BGRA 合成缓冲测试 224×144、350×262，以及在 350×262 基础上把宽度、高度或两者对齐到 16 的倍数。各组独立进程，每轮重新创建硬件编解码器并解码三帧，再释放会话。用与 WebRTC 对照相同的 Mach 采样器报告第 10 至第 100 轮的虚拟内存映射变化。

350×262 另跑两组仅编码和仅解码。编码组每轮验证三个 H.264 样本并释放，解码组只在采样前生成一次相同素材的三个编码样本，随后每轮重新解码并校验结果。两组都要求实际走 VideoToolbox 硬件路径。

仅编码组另用同尺寸、同亮度的 IOSurface NV12 缓冲复跑，与 BGRA 组比较第 10 至第 100 轮的映射变化。

350×262 BGRA 单独硬件编码另在独立进程跑 500 轮。相对第 10 轮停止后的基线，报告第 100、250、500 轮停止后的 RSS 和分配标签差值。

Windows 另用双端低延迟配置处理连续 90 帧 1920×1080 NV12。输入带 30 fps 时间戳并尽快送入编解码器，检查两处亮度、可见区域尺寸、时间戳和最大积压。日志报告缓冲区与可见区域各自的尺寸、提交到解码输出的 P50/P95/P99 和不按时间戳节流的吞吐量。测量涵盖编解码，真实窗口采集、网络和显示耗时待独立验证。[Media Foundation 可见区域](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-mt-minimum-display-aperture-attribute)

Windows 托管桌面测试还会通过 WGC 捕获一个测试窗口缩放前后的两帧。检查关闭后清空预览，再将这两帧的中央区域转换成 NV12，交替送入同一对低延迟编解码器，检查解码后亮度、色度和时间戳。测试画面只留在进程内存。

这里验证系统编解码入口。Windows 硬件编码、WebRTC codec factory 接线、端到端延迟、完整许可及实际 GPU 性能仍待验证。

- [Media Foundation H.264 编码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder)
- [VideoToolbox 压缩会话](https://developer.apple.com/documentation/videotoolbox/vtcompressionsession-api-collection)
