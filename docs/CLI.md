# CLI and diagnostics

See [快速开始](GETTING_STARTED.zh-CN.md) for the GUI. Addresses below are examples.

## Explicit two-peer enrollment

On each computer create a **different** identity:

```sh
peerglide init --identity identity
```

Exchange only `node.crt.der` through a separately trusted channel. Independently
compare its printed BLAKE3 fingerprint with the other computer. Never share
`node.key.der`. Store the other certificate as `peer.crt.der`. This manual
enrollment is an advanced fallback for the pairing GUI. There is no blind TOFU mode.

Test without input permission, on A:

```sh
peerglide listen --peer-cert peer.crt.der
```

On B (replace the LAN IP):

```sh
peerglide connect 192.168.1.10:24870 --peer-cert peer.crt.der --bind 0.0.0.0:0 --probe
```

`--probe` checks mutual TLS, version negotiation and a reliable roundtrip without
injecting input. Permit UDP 24870 for the process if the OS firewall prompts;
do not disable the firewall. By default the CLI exits on disconnect; optional
trusted reconnect is described below.

## Experimental native control

Both ends use `--native`. Add `--allow-control` on each computer permitted to
receive remote input. Listener/connector roles only establish the connection;
they do not create a permanent input master. `--edge` specifies the peer's side.

For A left of B:

```sh
# A
peerglide listen --peer-cert peer.crt.der --native --allow-control --edge right
# B
peerglide connect 192.168.1.10:24870 --peer-cert peer.crt.der --bind 0.0.0.0:0 --native --allow-control --edge left
```

- Release held keys/buttons before crossing the primary display edge.
- Keyboard follows the pointer. Move back over the remote entry edge to return. Edge rearming requires interior movement.
- **Ctrl+Alt+Right**, then release the chord, toggles the local control target.
- **Ctrl+Alt+Esc** immediately stops capture and requests remote release.
- The first physical action at the receiver only requests local takeover; its key/button pair is consumed, with no replay. Release physical keys/buttons before continuing locally.
- Capture watchdog: 1 second. Receiver lease: 1.5 seconds. Exit/error paths
  release tracked remote keys and buttons.

macOS requires Accessibility and Input Monitoring permission for the exact
app/terminal running the prototype, followed by restart. Secure Input remains
outside its control. Windows input cannot cross privilege boundaries or control
UAC/secure desktop. Keep OS security protections enabled.


## 多台可信电脑的监听

```text
peerglide listen-peers --identity PATH --bind 0.0.0.0:24870 --peer-cert A.der --peer-cert B.der
```

每个证书必须分别线下核对。对方使用已有的 `connect --probe` 检查
同一个端口。默认所有连接只运行 v5 通信诊断。Windows 和 macOS 可以
额外指定 `--native-peer-cert A.der --edge right`，让该证书的首次连接
进入现有原生会话。只有再加 `--allow-control` 才允许它接管本机。
其余已登记连接继续提供通信诊断。切换原生目标需先 Ctrl+C 停止，再
重新指定证书启动。真实多机输入留到集中验收。

## 从一台可信电脑播放音频

Windows 和 macOS 提供显式播放入口。指定已线下核对的音频源证书，
可用 `peerglide audio-output-devices` 查看本机输出编号或 UID，再通过
`--device` 选择。应用音量只影响该播放流。会话通过独立音频连接校验身份，Ctrl+C、
断线或无音频超时会停止。此入口只接收音频，系统采集由单独命令和
本机许可控制。

```text
peerglide play-audio --identity PATH --peer-cert SOURCE.der --volume 65
```

Windows 的 `share-system-audio` 使用默认播放设备的 WASAPI 回环。
当前支持 48 kHz 双声道 PCM16 或 Float32。接收端需先明确开启
`play-audio` 并核对发送方证书。系统音频与麦克风权限分别处理。

```text
peerglide share-system-audio RECEIVER_IP:24874 --identity PATH --peer-cert RECEIVER.der
```

macOS 13 或更新版本的同一发送命令使用 ScreenCaptureKit 的音频输出。
系统可能要求录制权限。这个入口不打开麦克风或注册屏幕画面输出。
真实权限提示、不同设备格式和结束后静音留到集中验收。
