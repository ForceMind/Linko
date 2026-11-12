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


## 多台可信电脑的通信诊断

```text
peerglide listen-peers --identity PATH --bind 0.0.0.0:24870 --peer-cert A.der --peer-cert B.der
```

每个证书必须分别线下核对。对方使用已有的 `connect --probe` 检查
同一个端口。监听可保持多条 v5 诊断连接，实际键鼠许可始终关闭。
Ctrl+C 停止监听及这些连接。

## 查询已核验电脑的屏幕表

Windows/macOS 发送端可明确临时共享一次屏幕位置、比例和方向。两台电脑需分别
核对证书，查询使用独立的 v5 认证探针与布局连接。

```text
peerglide listen-displays --identity PATH --peer-cert REQUESTER.der --share-display-layout
peerglide probe-displays HOST:24876 HOST:24877 --identity PATH --peer-cert HOST.der
```

屏幕编号只用于本次查询。此命令不授予键鼠控制权限。
