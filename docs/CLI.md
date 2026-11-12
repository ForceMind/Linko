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


## Linux Xorg 指针接收候选

```text
peerglide listen-x11 --peer-cert SOURCE.der --allow-pointer-control
```

仅在本机 Xorg 会话使用。接收端先核对来源证书，发送端用已有 v5 `connect --native`。
显式启动后最多接收一次指针租约。本机活动、断线、租约过期或 Ctrl+C 会结束它。
键盘、按钮、滚轮和 Linux 主动发起控制仍待接入。实际 Xorg 设备及接管行为待验收。
