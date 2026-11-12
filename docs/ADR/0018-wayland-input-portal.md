# ADR-0018 Wayland 键鼠会话

状态为设计草稿。Wayland 键鼠接收已有候选实现，需在真实合成器验证。
主动发送与物理输入自动接管待开发。Xorg 候选仅在本机 X11 会话开启。

## 发送侧

使用 XDG Desktop Portal 的 `InputCapture` 和 EIS。先确认本机已授权该次
输入会话，由合成器激活指针屏障。收到 `Activated` 后，Linko 还需完成既有
v5 输入租约的对端授权与本机提交检查。合成器的激活 ID 和 Linko 的连接、
租约及布局代次分别管理。回调只写有界状态，编码和发送由原工作器处理。

屏幕变化时以最新 `zone_set` 重建暴露边缘。收到撤销、`Deactivated`、
`Disabled`、关闭或失去 EIS 时立即停止转发，释放本机捕获。迟到的释放或激活
仅针对对应 ID，恢复先完成再允许下一次输入。默认不复用恢复令牌。

## 接收侧

使用 `RemoteDesktop` 仅申请所选键盘或指针，由操作系统确认并返回实际获准
的设备。设备已同意仍需 Linko 验证固定证书及本次输入租约。首选
`ConnectToEIS`，用合成器返回的区域处理绝对位置，保留当前注入账本和
停止后释放。屏幕图像、剪贴板、触摸屏及脚本权限各自独立，本候选只申请键鼠。

接收候选通过系统 liboeffis 请求 RemoteDesktop 指针和显式开启的键盘设备，使用 libei EIS
往返确认接收和清理。按钮和滚轮分别受 CLI 同意和合成器设备能力约束。保持一条
固定证书连接及一个 v5 租约。没有绝对指针区域、区域歧义、设备变化或系统撤权会结束
接收。正常结束时释放已提交的按钮和按键、停止滚轮和设备模拟，往返确认后发恢复回执。

若当前桌面缺少所需接口、区域数据或可证明的物理接管语义，保留该部分能力关闭。
同一桌面只运行一个输入所有者；Wayland 路径不通过 XWayland 重新打开捕获，
也不要求 root 或直接设备权限。软件依赖按锁文件和审计门槛单独核对。

## 验证顺序

先检查门户版本、许可拒绝、取消、父窗口及会话结束。测试不同合成器的 EIS
设备、区域变更、断线、会话关闭和已提交键／按钮释放。使用相同 Windows/macOS
候选测试 v5 配对和回落；实际 Linux 主动发送、接收及多屏接管以物理测试记录。

- [XDG InputCapture](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html)
- [XDG RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
- [XDG 窗口标识](https://flatpak.github.io/xdg-desktop-portal/docs/window-identifiers.html)
