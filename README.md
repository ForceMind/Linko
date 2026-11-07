# Linko · 屏跃

**Multiple Computers. One Seamless Experience.**

Rust software KVM for independent computers. Each peer can capture local input
and receive authorized remote input. Bluetooth and USB devices stay paired
with their original computer; operating system input is forwarded over LAN.

## Status

Experimental **0.3.16-dev**, not a stable release. CLI, authenticated QUIC transport,
bounded protocol and recovery state machine are runnable. Windows/macOS native
adapters are implemented; real Windows ↔ macOS dual-machine acceptance is
**pending**. Linux currently supports transport/protocol diagnostics only.
See [validation](docs/VALIDATION.md).

Primary-display switching, basic HID keys, three mouse buttons and scrolling
form the initial native prototype. Multi-monitor topology, IME composition,
clipboard, files and stable signed installers remain unimplemented. The new Chinese
short-code pairing flow and dual-peer save receipts are experimental and undergoing
qualification; manual independent fingerprint enrollment remains available. Pending
receipts block ordinary sessions and never enable input automatically. See validation
for tested scope and unfinished real-device acceptance. Optional tray/menu-bar integration is an experimental desktop slice; see validation. The macOS application menu and normal termination now use cooperative recovery; physical native-input acceptance remains pending. The desktop interface is Simplified Chinese, with an embedded OFL-licensed Noto Sans SC font; no system font installation is needed. A native eframe control panel provides
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

The existing repository was renamed to **[ForceMind/Linko](https://github.com/ForceMind/Linko)** on 2026-11-07; history, branches and PRs are retained. New portable packages use `Linko-<version>-<platform>`: open `Linko.app` on macOS, `Linko.exe` on Windows, or `./linko` on Linux. Keep the `peerglide` worker beside the GUI (inside the app on macOS). Source build commands and binary targets below retain their compatibility names.

Existing data directories, device identities, protocol v5 and the macOS bundle identifier remain compatible. New packages are separate from older PeerGlide packages; existing apps and system permissions are not moved or rewritten by packaging.

```sh
cargo build --release --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Use stable Rust (GUI requires at least 1.95). Run `cargo build --workspace --release --locked` to build both
executables. Launch `peerglide-gui` with `peerglide` beside it. The GUI displays **Linko / 屏跃**. The GitHub repository is ForceMind/Linko; crate, worker, identity and protocol identifiers retain PeerGlide compatibility.
It defaults to 屏幕布局, with six Chinese navigation entries: 设备、屏幕布局、文件传输、剪贴板、快捷键、设置. 快捷键 retains access to 协作总览 and 本机鼠标 / gesture rules. 设备 provides
identity/enrollment, independent receiving permission, harmless communication/probes
and primary-display direction. Starting a keyboard/mouse session requires a recent
read-only worker permission check and fresh explicit confirmation; saved preferences never enable native input on a normal
connection. 屏幕布局 offers two-primary-display direction and view-only zoom; it does not implement multi-monitor geometry or remote video. 本机鼠标 edits offline rules and tests window-local gestures; its global
G1 switch remains unavailable. See [actual GUI interactions](docs/GUI_INTERACTIONS.zh-CN.md).
No config file editing is required.

CI artifacts contain an unsigned Windows portable ZIP, an ad-hoc integrity-signed macOS app bundle or
Linux portable ZIP plus checksums and notices. Linux GUI runtime libraries
include X11/XKB, EGL/OpenGL (or the corresponding Wayland stack). They are experimental builds,
not stable installers. macOS Developer ID signing/notarization and Windows code signing are
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
“检查实际工作进程权限” additionally launches only the sibling CLI read-only diagnostic,
with bounded output, cancellation and a deadline. GUI and worker snapshots are separate;
neither enables input or replaces the worker's check at native startup.
These checks never request OS permissions or authorize control/actions. See
[native context](docs/NATIVE_CONTEXT.md) for privacy, supported OSes and limitations.

## Windows + macOS 双机操作（开发版）

本版优先基础键鼠协同，范围为两台电脑的主显示器。真实双机验收仍见
[验证记录](docs/VALIDATION.md)与[Issue #3](https://github.com/ForceMind/Linko/issues/3)；
自动测试通过不等于已能稳定日常工作。完整候选通过后，按以下顺序验收：

1. 两端使用同一版、相同协议的完整开发包。Windows 解压后保留两个程序在一起；
   macOS 保留完整 app 包。不要混用旧 CLI 和新 GUI，也不要覆盖既有身份或配置。
   安全策略拒绝未签名程序时保留防护，不关闭 Gatekeeper、UAC 或防火墙。
2. 在总览点击“添加另一台电脑”，各自准备不同的本机身份。一端选择“在这台显示配对码”，
   另一端输入它显示的实际 IPv4 地址与八位码，再点击“核验另一台电脑”。可选局域网查找：
   显示码的一端还要点击让其他电脑找到本次配对，另一端点击查找并选择地址。
   发现不发送码或授信，选择后重新输入这台电脑当前的码；没有结果仍可手动填写地址。
3. 两端分别点击“记住这台设备”，等待本机已保存且收到对端保存回执。没有完成回执时
   不能视为两端完成，待回执登记会阻止连接；请重新核验或明确撤销，不会恢复旧授权。
   短码不会广播或写入日志/配置，完成记住设备也不会开启键鼠。已有登记保持可用。
4. 高级回退仍可交换公开连接卡，或沿用证书路径；它们只含公开材料，不含私钥。
   该路径须通过独立可信渠道对照对方完整 BLAKE3 指纹，一致后才勾选确认并登记。
   读取信息不授信，修改后需重新读取和核对；连接卡不是短码认证。
   在“协作权限”分别选择是否接收对端键鼠，双向控制需要两端分别同意。
   剪贴板和文件仍不开放。高级区域保留原有证书路径登记及未认证发现提示。
5. 可在“高级连接与身份”先由等待方建立“只连接，不开启键鼠”，主动连接方运行
   “检查两台电脑的连接”。通过并正常退出后，再摆放屏幕：例如 Windows 在左、Mac 在右，则 Windows
   选“右侧”、Mac 选“左侧”。这是相对方向，不是多显示器精细拓扑。
6. 两端先点击“检查本机键鼠权限”。通过后，分别点击“开启键鼠协同…”并明确确认。
   使用向导配对后，不必赶在短连接超时内同时操作；必要时会等待另一台约一分钟，
   可随时停止。这不启用断线自动重连。检查不申请系统权限，过期需重新检查；
   实际输入程序开启时仍会重新验权。
   macOS 需对实际运行的 app/输入程序具备辅助功能和输入监控权限；“设置 → 检查键鼠权限”
   查询实际输入程序，上次快照不授予执行许可。系统权限由用户明确处理，软件不会代授。
7. 松开物理键和鼠标按钮后跨边，键盘跟随控制目标；先移入对端，再在对端按下
   鼠标拖拽。按住按钮时不跨屏，原生文件跨屏拖放未提供。Control/Command 保持
   各自物理语义，快捷键、布局和输入法由目标系统解释，不是 Unicode/IME 文本转发。
   切换没成功时，查看总览的请求、拒绝、超时或对端能力提示。拒绝没有披露具体原因，
   请在另一台检查接收控制、键鼠开启及恢复状态；移回屏幕内再试，不会自动重试。
8. `Ctrl+Alt+Right` 并松开组合键可手动切换；`Ctrl+Alt+Esc` 紧急恢复。
   接收端首次物理动作只用于收回控制，不执行或回放触发动作；切回本机后先松开
   全部物理键/按钮再操作。停止或退出后等待工作进程收尾。若显示“输入恢复未确认”，
   松开所有键/按钮并检查两端，保留警告，不反复开启新原生会话。清理失败且原生
   捕获仍可用时，重新按下并松开完整 `Ctrl+Alt+Esc` 可请求强制返回；未知警告不清除，
   不代表系统修饰键已释放。明确强制结束工作进程同样不能证明恢复成功。

此顺序不替代四边、拖拽/滚轮、修饰键、接收端物理接管、不同DPI、断网/退出、
睡眠唤醒与权限变化的实机矩阵。当前 G1、生产剪贴板/文件仍未提供；短码配对与
双端回执是开发预览，真实双机仍待验证。托盘/菜单栏的可选预览见下文。

0.2.3-dev 修正 macOS 打包身份，并新增“设置 → 检查实际工作进程权限”。0.2.2 实机
曾出现 GUI 两项权限已授予、worker 仍因辅助功能未获许可而退出；不能反复要求用户给
旧应用加同一权限。macOS 包现在从内到外采用 ad-hoc 完整性签名并验证 plist/嵌套代码，
没有 Developer ID 身份、没有公证；不同构建的系统授权不保证自动沿用。不要关闭防护。
0.2.4在用户移除旧0.1.0授权条目、重新添加正确应用后，实际worker两项权限只读检查
已通过；这不证明未来构建会自动沿用授权或真实键鼠已可用。GUI 6.83GB根因仍未定位。
已通过真实双机无输入通信；物理键鼠 C06 仍未通过。此增量不改输入 v5/设备身份或授权。

0.2.4-dev 在没有会话、停止/重连、发现、工作进程诊断或画布超时时，不再强制每100ms
重绘。活动工作仍保持状态轮询，隐藏窗口仍处理停止与最终输出；窗口内画布遮挡后取消。
这是空闲资源改进，不代表此前GUI6.83GB根因已定位。Oink全模块参考、用户确认的原创
MIT授权范围和安全差异见[参考研究](docs/REFERENCE_RESEARCH.md)。

0.3.0-dev把设备卡、相对屏幕摆放和键鼠主操作放进同一工作区，采用新的浅深色卡片与
简洁中文状态。总览也能直接管理已登记电脑，证书/网络角色/连接测试和GUI自身快照
收进高级区域。开启仍需本次明确确认，旧控制状态和普通通信不会变成执行许可。
连接卡仍需独立核对完整指纹，认证短码、托盘、G1、数据与音频未因界面改版而接通。
版本及真实候选验证见VALIDATION，整合顺序见DEVELOPMENT_PLAN。

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
- The first physical action at the receiver only requests local takeover; its key/button pair is consumed, with no replay. Release physical keys/buttons before continuing locally.
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


0.3.2-dev增加可选择开启的Windows托盘/macOS菜单栏。设置中先启用入口，再单独选择
关闭窗口后是否保留后台协同；默认仍停止、清理后退出。菜单只提供显示窗口、停止、
退出，不从菜单启用键鼠或扩大权限。入口失败时不隐藏，恢复未知会显示窗口保留警告。
桌面偏好使用单独版本化文件，不更改旧输入设置或身份；不开机启动。精确候选及原生
验收状态见VALIDATION，未验证的菜单行为不能当物理键鼠C06通过。
