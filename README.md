# PeerGlide · 屏跃

**Multiple Computers. One Seamless Experience.**

Rust software KVM for independent computers. Each peer can capture local input
and receive authorized remote input. Bluetooth and USB devices stay paired
with their original computer; operating system input is forwarded over LAN.

## Status

Experimental **0.2.1-dev**, not a stable release. CLI, authenticated QUIC transport,
bounded protocol and recovery state machine are runnable. Windows/macOS native
adapters are implemented; real Windows ↔ macOS dual-machine acceptance is
**pending**. Linux currently supports transport/protocol diagnostics only.
See [validation](docs/VALIDATION.md).

Primary-display switching, basic HID keys, three mouse buttons and scrolling
form the initial native prototype. Multi-monitor topology, IME composition,
clipboard, files, pairing codes, tray and signed installers are
not implemented in this milestone. The desktop interface is Simplified Chinese, with an embedded OFL-licensed Noto Sans SC font; no system font installation is needed. A native eframe control panel provides
identity/enrollment, session start/stop, probes, relative screen placement and
dark/light settings.

An offline **手势实验室 (Gesture lab)** lets you draw on a window-local canvas, inspect
recognition/rejection and explicitly save labelled samples. Open 本机鼠标 → 录制与本窗口测试
or run `peerglide-gui --gesture-lab`; no pairing or input permissions are needed.
This is a test/recording page, not global mouse gestures or action execution.
See [gesture lab](docs/GESTURE_LAB.md) for scope and privacy.

Devices also offers opt-in IPv4 LAN discovery. Results are unauthenticated hints;
selecting an address never grants trust/control. Public listener advertisement is
explicit and separate. Actual multicast/permission acceptance is pending; see
[discovery](docs/DISCOVERY.md).

## Build

```sh
cargo build --release --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Use stable Rust (GUI requires at least 1.95). Run `cargo build --workspace --release --locked` to build both
executables. Launch `peerglide-gui` with `peerglide` beside it. The Chinese GUI has
five pages: 总览、设备与屏幕、本机鼠标、剪贴板与文件、设置. 设备与屏幕 provides
identity/enrollment, independent receiving permission, harmless communication/probes
and primary-display direction. Starting a keyboard/mouse session requires a fresh
explicit confirmation; saved preferences never enable native input on a normal
connection. 本机鼠标 edits offline rules and tests window-local gestures; its global
G1 switch remains unavailable. See [actual GUI interactions](docs/GUI_INTERACTIONS.zh-CN.md).
No config file editing is required.

CI artifacts contain an unsigned Windows portable ZIP, macOS app bundle or
Linux portable ZIP plus checksums and notices. Linux GUI runtime libraries
include X11/XKB, EGL/OpenGL (or the corresponding Wayland stack). They are experimental builds,
not stable installers. macOS signing/notarization and Windows code signing are
pending; do not disable OS protections to open them. The Linux GUI can configure
and probe connections, but Linux native input is not implemented.

For an isolated local GUI check, use an explicitly selected directory:

```sh
peerglide-gui --data-dir /path/to/isolated-test-data
peerglide-gui --data-dir /path/to/isolated-test-data --gesture-lab
```

This stores settings, new identities and manually exported gesture samples in that
directory. It never copies existing trust or permissions from the usual profile.
Keep the CLI beside the GUI. No system input permission is needed for the offline
pages or transport probes. Without `--data-dir`, the existing user-profile location
is unchanged. Unknown or incomplete command-line options reject before opening a
window or creating configuration files.

Read-only diagnostics need no identity or network and never request input permissions;
unavailable OS access is reported explicitly:

```sh
peerglide diagnose-context
# Opt in only if you want the exact application identifier in local stdout:
peerglide diagnose-context --include-application-id
```

Settings → 检查当前系统权限 also reads the GUI process's permission snapshot.
These checks never request OS permissions or authorize control/actions. See
[native context](docs/NATIVE_CONTEXT.md) for privacy, supported OSes and limitations.

## Windows + macOS 双机操作（开发版）

本版优先基础键鼠协同，范围为两台电脑的主显示器。真实双机验收仍见
[验证记录](docs/VALIDATION.md)与[Issue #3](https://github.com/ForceMind/PeerGlide/issues/3)；
自动测试通过不等于已能稳定日常工作。完整候选通过后，按以下顺序验收：

1. 两端使用同一版、相同协议的完整开发包。Windows 解压后保留两个程序在一起；
   macOS 保留完整 app 包。不要混用旧 CLI 和新 GUI，也不要覆盖既有身份或配置。
   安全策略拒绝未签名程序时保留防护，不关闭 Gatekeeper、UAC 或防火墙。
2. 在两端打开“设备与屏幕 → 添加／重新核验”，各自创建不同的本机身份。
   展开“本机公钥与指纹”，只交换 `node.crt.der`，绝不交换 `node.key.der` 或整个身份目录。
3. 选择一台“本机等待对端连接”，另一台“主动连接对端”，填等待方的局域网 IP 与
   端口（默认 UDP 24870）。发现结果仅供填地址，不能代替证书核验。
4. 在各端填写对端公钥证书路径（文件选择器尚未提供），通过独立可信渠道核对
   完整指纹，再登记。登记不会自动允许输入；在“本设备权限”分别选择是否接收
   对端键鼠。双向控制需要两端分别同意，剪贴板和文件仍不开放。
5. 先由等待方建立“仅通信连接”，主动连接方运行“通信探针”。看到探针通过并
   正常退出后，再设置“屏幕布局”：例如 Windows 在左、Mac 在右，则 Windows
   选“右侧”、Mac 选“左侧”。这是相对方向，不是多显示器精细拓扑。
6. 先在等待方、再在主动连接方点击“启用键鼠会话…”，分别明确确认本次使用。
   macOS 需对实际运行的 app/工作进程具备辅助功能和输入监控权限；设置中的GUI
   权限快照不代表实际 worker 已获许可。系统权限由用户明确处理，软件不会代授。
7. 松开物理键和鼠标按钮后跨边，键盘跟随控制目标；先移入对端，再在对端按下
   鼠标拖拽。按住按钮时不跨屏，原生文件跨屏拖放未提供。Control/Command 保持
   各自物理语义，快捷键、布局和输入法由目标系统解释，不是 Unicode/IME 文本转发。
8. `Ctrl+Alt+Right` 并松开组合键可手动切换；`Ctrl+Alt+Esc` 紧急恢复。
   停止或退出后等待工作进程收尾。若显示“输入恢复未确认”，先松开所有物理键/
   按钮并检查两端，保留警告，不反复开启新原生会话；强制停止不能证明恢复成功。

此顺序不替代四边、拖拽/滚轮、修饰键、接收端物理接管、不同DPI、断网/退出、
睡眠唤醒与权限变化的实机矩阵。当前 G1、生产剪贴板/文件、配对码和托盘仍未提供。

## Explicit two-peer enrollment

On each computer create a **different** identity:

```sh
peerglide init --identity identity
```

Exchange only `node.crt.der` through a separately trusted channel. Independently
compare its printed BLAKE3 fingerprint with the other computer. Never share
`node.key.der`. Store the other certificate as `peer.crt.der`. This manual
enrollment precedes the future pairing GUI. There is no blind TOFU mode.

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
- Physical input at the receiver reclaims control.
- Capture watchdog: 1 second. Receiver lease: 1.5 seconds. Exit/error paths
  release tracked remote keys and buttons.

macOS requires Accessibility and Input Monitoring permission for the exact
app/terminal running the prototype, followed by restart. Secure Input remains
outside its control. Windows input cannot cross privilege boundaries or control
UAC/secure desktop. Keep OS security protections enabled.

## Documentation

- [Product](docs/PRODUCT.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Roadmap](docs/ROADMAP.md) and [development plan](docs/DEVELOPMENT_PLAN.md)
- [Research and licensing](docs/REFERENCE_RESEARCH.md)
- [Security](docs/SECURITY.md) and [validation](docs/VALIDATION.md)
- [Local Codex handoff](docs/LOCAL_CODEX_HANDOFF.zh-CN.md) and `python tools/local_preflight.py`

MIT licensed. GPL references are studied, not copied or linked. Dependency
licenses are separate and must accompany distributions.


Protocol migration: current development builds use `peerglide/5`. Update both
computers together; v3/v4 peers are rejected before input capture. Existing identity
and enrolled certificates remain usable. See [ADR-0003](docs/ADR/0003-terminal-cursor-v4.md).

Input capability negotiation and v5 migration are documented in [ADR-0004](docs/ADR/0004-input-capabilities-v5.md).


Trusted reconnect is optional and off by default. Enable **Reconnect trusted peer
automatically** in Settings, or add `--reconnect` to `listen`/`connect`. It retries
the same enrolled identity with capped backoff; changing certificates, native
cleanup errors or authorization failures stop it. Control stays local until you
switch again. See [ADR-0005](docs/ADR/0005-trusted-reconnect.md) for limits.
