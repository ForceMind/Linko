# RW04 WebRTC 源码探针

Actions 从固定的 WebRTC 和 depot_tools 提交同步源码及 DEPS，构建一个最小的 VideoFrame 探针。程序生成 I420 测试帧，从 WebRTC API 读回尺寸、采样时间和回绕前后的 RTP 时间戳。两个平台分别上传源码、DEPS 和许可证文件的 Git 标识，以及构建参数和探针结果。仓库只保存探针和配置。

这是 RW04 构建与接口验证。平台编码器、媒体网络、帧呈现及完整分发许可仍须验证；当前构建明确关闭 H.264 和专有 codec。产物仅含验证报告。

- [固定 WebRTC 提交](https://webrtc.googlesource.com/src/+/692e07f2cb2f3602af5a8083b695f4f3bc2c9a6f)
- [固定 depot_tools 提交](https://chromium.googlesource.com/chromium/tools/depot_tools/+/1b0972d3d5e5b1e78055147cb3576361e274b9ed)
- [现有 RW04 技术方案](https://github.com/ForceMind/Linko/pull/274)
