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

原生发送端可以用 `--remote-entry-edge top` 指定进入另一台的上沿，同时用 `--edge right`
保留本机从右沿出去。回程从另一台的进入边触发，本机光标回到本机的出边。两端仍分别
控制接收许可。跨过出边后的前进位移沿另一台的入边进入屏内，沿边位移保持相同坐标
顺序。若同时请求副屏预留，接收端的布局方向要与该进入边一致。


## 多台可信电脑的监听

```text
peerglide listen-peers --identity PATH --bind 0.0.0.0:24870 --peer-cert A.der --peer-cert B.der
```

每个证书必须分别线下核对。对方使用已有的 `connect --probe` 检查
同一个端口。默认所有连接只运行 v5 通信诊断。Windows 和 macOS 可以
额外指定 `--native-peer-cert A.der --edge right`，让该证书的首次连接
进入现有原生会话。只有再加 `--allow-control` 才允许它接管本机。
其余已登记连接继续提供通信诊断。切换原生目标需先 Ctrl+C 停止，再
重新指定证书启动。使用 `--serial-native` 可重复列出明确允许原生会话的
`--native-peer-cert`。在上一会话确认本机恢复后，同一监听可以接入下一台。
串行模式的接收控制分别用 `--allow-control-peer-cert` 指定。已选为原生候选但未获
接收许可的电脑，只能由本机主动控制。其他连接仍可通信诊断。真实多机输入留到集中验收。

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

同一个本机输出可混合最多四台已经分别核验的系统音频来源。
接收端明确列出来源证书。各来源沿用 `share-system-audio`，并核对接收端证书。
播放端可选择 `audio-output-devices` 显示的本机设备及播放音量。

```text
peerglide play-audio-mix --identity PATH --peer-cert A.der --peer-cert B.der --device DEVICE --volume 65
```

## Linux Xorg 输入接收候选

```text
peerglide listen-x11 --peer-cert SOURCE.der --allow-pointer-control
```

仅在本机 Xorg 会话使用。接收端先核对来源证书，发送端用已有 v5 `connect --native`。
显式启动后最多接收一次指针租约。另加 `--allow-discrete-input` 可允许此会话接收
本机 XKB 支持的键位和三键鼠标按钮。本机活动、断线、租约过期或 Ctrl+C 会结束会话
并尝试释放工作器提交的按下事件。接收端确认本机清理成功后，以现有 v5 恢复回执结束连接。
标准 X11 按钮映射可用时，`--allow-scroll` 独立
允许双轴滚轮，每帧每轴最多 12 步。
实际 Xorg 键鼠、滚轮方向、接管和注入来源由集中验收确认。

在单一 Xorg 根屏幕上，可以显式启动一次相对指针发送：

```text
peerglide send-x11 RECEIVER_IP:24870 --peer-cert RECEIVER.der --edge right --allow-pointer-capture
```

从屏幕内部用相对指针移到指定外沿并继续向外推。接收端须核验本机证书并授予
v5 指针租约。另加 `--allow-button-capture` 可发送三个鼠标按钮，另加
`--allow-scroll-capture` 可发送双轴滚轮。两项分别开启，接收端仍须授予对应能力。
发送端需要标准 Xorg 按钮映射和匹配的本机相对指针事件。

加 `--allow-keyboard-capture` 可发送双方支持的 XKB 物理键位。键盘捕获时按
Ctrl+Alt+Esc 结束远程会话。未开启键盘捕获时，本机按键仍会结束发送。

回到接收端的进入边、断线、Ctrl+C 或遇到未获允许的输入也会结束发送。
没有已截获的按下时，Xorg 将当前输入交还本机。若仍按住已截获的键或按钮，
先结束网络发送。原所有者等到这些输入和恢复期间新按下的输入全部抬起，再归还本机。
Wayland 使用单独的接收入口，仍沿用现有 v5 连接和固定证书。

### Wayland 键鼠接收候选

```text
peerglide listen-wayland --peer-cert SOURCE.der --allow-pointer-control
```

在 Wayland 桌面运行后，先在系统 RemoteDesktop 对话框同意所请求的设备。可分别加
`--allow-buttons`、`--allow-scroll` 或 `--allow-keyboard`。合成器提供多个绝对指针区域时，使用
`--region X,Y` 指定其报告的区域原点。

此入口通过 `liboeffis.so.1` 和 `libei.so.1` 使用系统已提供的 EIS 接口，
要求 libei 支持 1.4 的往返确认。收到有效 v5 租约才向合成器提交输入。
键盘沿用 HID 物理键位。电源键和有重复 evdev 编码的非美式井号位置未声明为可接收键。
使用 Ctrl+C 或系统共享控制结束会话。Wayland 主动发送和本机物理输入自动接管待开发。

## 麦克风音频候选

播放端在 Windows 或 macOS 明确指定麦克风来源，默认使用 UDP 24875。
发送端明确指定已核验的播放端。Windows 使用默认通信麦克风，macOS 使用默认录音设备并由系统确认权限。
设备会在本次会话认证成功后打开，Ctrl+C、断线和许可到期结束会话。

```text
peerglide play-microphone --identity PATH --peer-cert SOURCE.der
peerglide share-microphone RECEIVER_IP:24875 --identity PATH --peer-cert RECEIVER.der
```

## 指定本机键鼠接收屏幕

Windows 和 macOS 的原生接收会话可用 `--receive-display=X,Y` 选定一块本机屏幕。
设备详情中的显示器列表提供当前位置。使用屏幕左上角坐标，镜像或重复坐标会拒绝。
选项需要显式 `--native --allow-control`，可与现有 v5 电脑通信。源端的边缘方向仍按
两端已有的配置。自动重连仍使用本次启动时的本机目标快照。更换目标需要停止并重新启动。
工作器定期核对本机布局。检测到变化或无法重新核对已有布局时，会结束原生会话并执行
键鼠释放和本机恢复。重启会话前重新确认接收屏幕，避免沿用热插拔前的目标。

```text
peerglide listen --identity PATH --peer-cert SOURCE.der --native --allow-control --receive-display=-1920,0
```

真实 DPI、旋转、拖动和热插拔由集中验收确认。

### 双端确认指定接收屏幕

接收端可同时设置 `--secondary-layout-bind`，要求每次接管先由当前输入连接确认选定的
本机屏幕。发送端用 `--request-secondary-layout` 指向该布局监听。两端方向须相反。
旧端仍可使用原有单主屏或仅本机指定屏幕模式。启用双端确认时，未确认会保留本机控制。

```text
peerglide listen --peer-cert SOURCE.der --native --allow-control --edge left --receive-display=-1920,0 --secondary-layout-bind=0.0.0.0:24878
peerglide connect HOST:24870 --peer-cert HOST.der --native --edge right --request-secondary-layout=HOST:24878
```

这条路径一次确认接收端明确配置的屏幕。多目标自动路由仍待开发。

主屏模式显式启用 `--reconnect` 时，检测到新的有效布局后可在本机释放完成后重连原电脑。
开始新的键鼠捕获前重新核对证书与本机布局，光标先回到屏内才能再次从边缘切换。布局
读不到、恢复未确认或指定了任意一端的接收屏幕时，需要先检查本机状态。真实热插拔和
两端恢复仍待验收。

## 探测分段屏幕路由

Windows 和 macOS 可以用当前显示器布局核对一个指定外沿点。先独立核对各电脑的
证书指纹，再把证书路径和目标地址写入本机布局文件。相对证书路径从布局文件所在目录
解析，`peer` 是 `peers` 中从零开始的位置。坐标用本机显示器列表中的桌面坐标。

```json
{
  "version": 1,
  "peers": [
    {"certificate": "office.der", "address": "192.168.1.21:24870"},
    {"certificate": "studio.der", "address": "192.168.1.22:24870"}
  ],
  "routes": [
    {"local_origin": [-1920, 0], "local_edge": "Right", "local_span": [0, 0.5],
     "peer": 0, "remote_edge": "Left", "remote_span": [0, 1]},
    {"local_origin": [-1920, 0], "local_edge": "Right", "local_span": [0.5, 1],
     "peer": 1, "remote_edge": "Top", "remote_span": [0, 1]}
  ]
}
```

```text
peerglide probe-route --identity PATH --layout routes.json --point=-1,450 --edge right
```

目标使用 `listen` 或 `listen-peers` 的默认通信模式。探测只连接这个分段指定的
电脑，并核对精确证书、v5 协商和可靠流回执。共用边、屏内点和失效布局无路由，
目标离线时直接报错。

### 启用分段键鼠发送

Windows 和 macOS 可用同一布局文件启动分段发送。各接收电脑须分别核对来源证书并
允许来源控制。下例在目标端保持串行原生监听，以便发送端回到本机后重新选择路线。

```text
peerglide listen-peers --peer-cert source.der --native-peer-cert source.der --serial-native --allow-control-peer-cert source.der
peerglide connect-routes --identity PATH --layout routes.json --native
```

发送端回到屏内后才能再次跨边。目的地由实际光标到达的唯一分段决定。共用边和多个
分段相交的角点保留本机控制。连接失败时也留在本机。发送端拒绝从这些连接接收键鼠。
返回本机时，旧连接必须确认清理完成才继续选择。恢复未确认时会停止分段发送。
`Ctrl+Alt+Right` 可从当前目标返回本机，`Ctrl+Alt+Esc` 和 `Ctrl+C` 结束发送。

在某台目标的 `peers` 记录加入 `"secondary_layout": "192.168.1.22:24877"`，可要求
每次接管先确认该目标明确选定的副屏。该电脑须用 `listen` 启用 `--native --allow-control`，
指定 `--receive-display=X,Y --secondary-layout-bind=0.0.0.0:24877`，并把自己的 `--edge`
设为发送端配置的 `remote_edge`。可用 `--reconnect` 接受后续分段连接。预留失败时，
光标留在本机。省略该字段时，继续使用目标接收会话的默认行为。副屏由接收端选择。
桌面可在布局工作区选择本机，展开详情中的“多电脑分段布局”。读取当前显示器后，
可选择出边、目标、入边和两端分段范围。导出文件保存在数据目录的 `route-exports`，
带有已核对的证书指纹。用 `connect-routes --layout` 指向导出文件。现有导出不会被
同名操作覆盖。物理多机切换仍待验收。

也可在同一位置选择“开启分段键鼠”，检查本机权限后确认。桌面工作进程在捕获前重验
草稿和身份。使用桌面的停止入口结束，上一台未确认清理时保留恢复警告。

空闲时可选择“投影分段布局”，在独立窗口查看本机屏幕排列。选中分段后，把圆点拖到
目标本机屏幕的可用外沿。保存前会再次读取显示器并校验草稿与分段冲突。取消会丢弃
本次预览。已有的地址、证书和远端入边在分段表单中编辑。

## 查询已核验电脑的屏幕表

Windows/macOS 发送端可明确临时共享一次屏幕位置、比例和方向。两台电脑需分别
核对证书，查询使用独立的 v5 认证探针与布局连接。

```text
peerglide listen-displays --identity PATH --peer-cert REQUESTER.der --share-display-layout
peerglide probe-displays HOST:24876 HOST:24877 --identity PATH --peer-cert HOST.der
```

屏幕编号只用于本次查询。此命令不授予键鼠控制权限。
