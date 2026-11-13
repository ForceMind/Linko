# ADR-0016 麦克风的独立音频会话

状态为传输候选。麦克风发送和播放需两端各自显式同意，并分别核对精确证书。
系统音频、键鼠和其他数据的许可不能用于麦克风。

麦克风使用独立的 `peerglide-microphone/1` ALPN 和方向握手，沿用有界 PCM 帧、
丢包静音、停止和过期边界。系统音频保留既有 `peerglide-audio/1`。麦克风播放先限
一台来源，系统音频聚合的来源表不包含麦克风。两者都不记录音频内容。

本机发送方在对方认证且当次会话成立后才请求操作系统录音权限并打开设备。收到停止、
断线、过期、设备丢失或撤权时停止采集和播放。旧连接和缓冲不得恢复录音。Windows 与
macOS 采用普通用户公开录音接口。虚拟设备、原生插件和系统安装另行评审和授权。

传输测试用内存合成帧核对协议隔离、精确证书和方向。实际麦克风权限、系统提示、
断线恢复、音质与同源双端包由集中验收记录。

## 虚拟设备的系统边界

macOS 的候选方案是独立 Core Audio Audio Server Plug-in。Apple 的公开接口可发布虚拟
设备，驱动包安装路径为 `/Library/Audio/Plug-Ins/HAL`。Windows 的候选方案是独立
WDM 虚拟音频端点，构建需要 WDK，目标设备需要受信任的驱动签名和安装同意。

虚拟麦克风使用独立输出选择和本机同意。设备仅从本机已授权的音频工作器读取有界
PCM 队列。实时回调只复制已就绪的帧或填静音。停止、撤权、断线及工作器退出时清空
当前会话队列，重新连接必须取得新的会话许可。验证覆盖并发读写、旧代次丢弃、缓冲
上限、静音和设备移除。系统验收另行核对签名、安装路径、实际音频客户端和卸载恢复。

- [Apple Audio Server Driver Plug-in](https://developer.apple.com/documentation/CoreAudio/creating-an-audio-server-driver-plug-in)
- [Microsoft SysVAD 接口与部署要求](https://learn.microsoft.com/en-us/samples/microsoft/windows-driver-samples/sysvad-virtual-audio-device-driver-sample/)
- [Microsoft 虚拟驱动签名要求](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-requirements--windows-vista-and-later-)
