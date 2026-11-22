# Linko 远程工作空间：可执行离线基础实验

这是 Linko（原 PeerGlide）现有工程内的隔离研发包，不是另起一套应用。它实现并测试未来多设备远程工作空间的纯逻辑，不改变现有 eframe、QUIC、原生输入生命周期、v5 协议或 UI 分支。

**当前增量为 RW03 本机单窗口预览，沿用 PR #286 的 RW02 协调器与跨模块故障测试。** 详细技术方案、API契约、失效矩阵、Windows/macOS接线步骤及剩余门槛见 [LINKO_REMOTE_WINDOW_RW02.zh-CN.md](../../docs/LINKO_REMOTE_WINDOW_RW02.zh-CN.md)，接手见 [远程研发交接](../../docs/LINKO_REMOTE_WORKSPACE_HANDOFF.zh-CN.md)。

## 当前已有代码

- `geometry`：画布缩放/平移、保持比例显示、黑边拒绝、旋转、负坐标显示器和逻辑/像素坐标映射。
- `session`：每个来源/查看端/会话代次绑定；观看与控制分离；权限过期、撤销、断连、重建。观看授权另有代次，防止重新授权后接受旧画面；静态桌面使用短期来源确认挑战，而非用本地重画假造新鲜度。
- `routing`：唯一输入目标、序号防重放、当前布局/来源版本失效；切换目标必须等待旧目标实际输入释放的确认，即使新目标尚无授权。
- `scheduling`：可见性与聚焦优先级、同源多面板复用，以及解码数量、参考像素缓存、像素速率和目标码率预算；生产接线仍待完成。
- `media`：固定头部先校验后分配的有界 RGBA 实验帧、来源/观看授权代次绑定、单帧替换缓存、撤权清空及过期丢弃。**不是 H.264，也不是安全传输协议。**
- `handoff`：接收端确认可用后才请求隐藏来源窗口；失败后保留恢复请求，直至原生后端实际确认恢复。中途改变权限或几何会取消旧交接，重新授予权限不会使它复活。
- **`coordinator`**：把上述策略/媒体/输入/交接模块纳入统一生命周期；最多16条有界注册、不可复用本地ViewId、独立观看/控制、一次性展示收据、几何/布局失效、空闲watchdog、精确Binding的多窗口终态关闭、默认禁止停放、释放/恢复确认后才能注销。没有第二个InputRouter或原生hook。
- `tests/coordinator_faults.rs`：36项新增跨模块集成测试，包括六类故障、迟到/重复ACK、重授权、失焦、连接级故障、队列期限、容量和100次模型授权循环。
- `examples/workspace_simulation.rs`：原有双来源离线场景，执行断言后输出明确的fixture结果；不是原生远控演示。

## 执行

需要 Rust 1.95 或更高版本。本包没有第三方依赖、无网络或 OS 权限需求。在仓库根目录执行：

```sh
cargo test --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline
cargo clippy --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline -- -D warnings
cargo fmt --manifest-path labs/remote-workspace/Cargo.toml --all -- --check
cargo run --manifest-path labs/remote-workspace/Cargo.toml --example workspace_simulation --locked --offline
cargo build --manifest-path labs/remote-workspace/Cargo.toml --release --all-targets --locked --offline
```

第一条命令运行原有53项单元测试和新增36项集成测试。100次模型循环不代表真实100次进程启停、GPU资源回收或跨屏验收。

包使用独立 Cargo workspace/lockfile，避免更改并行研发中的主工作区。`.github/workflows/workspace-lab.yml` 独立检查 Windows/macOS/Linux；原有 Rust CI 保留。后续生产接线需先ADR，不能因实验通过就当作已上线功能。精确候选验证见 #286；代码基线 `a4f23e0178aade220f23186136ceadc0850b5a10` 的实验包 [run 43817627210](https://github.com/ForceMind/Linko/actions/runs/43817627210) 已核对三平台成功，后续HEAD必须检查自己的CI。

## RW03 本机单窗口预览

独立预览入口在本实验包的 `preview/`，由 GitHub Actions 的 `linko-rw03-windows-latest` 与 `linko-rw03-macos-latest` 提供。Windows 使用支持 Windows.Graphics.Capture 的桌面环境（系统接口要求 Windows 10 1903 或更新版本）。macOS 要求 14 或更新版本。

1. 打开「屏跃 · 本机窗口预览」，从可见的本机窗口中选一个。macOS 第一次需在系统设置允许屏幕录制并重新打开预览。
2. 在预览或原应用调整尺寸。预览保持画面比例，来源窗口继续正常使用。选择的窗口关闭、Windows 最小化或捕获出错时清掉画面。macOS 无有效帧时在两秒内清掉画面。
3. 点「停止」或关闭预览窗口。每次开始都重新选源。画面留在进程内存，仅用于本机显示。

Windows 用公开的单窗口 WGC 接口和后台 D3D11 回读，捕获池固定为 4096×2160，预览只读回实际窗口矩形。macOS 将所选 SCWindow 固定在单窗口过滤器内，1920×1080 内使用帧的内容矩形，不采集音频。两个后端都不写入截图、不发送媒体，也不授予或注入键鼠。实验产物没有正式签名或公证。系统拒绝时遵循正常的系统权限处理流程。

Windows Actions 在托管桌面用两个同名测试窗口检查尺寸更新、最小化后清屏、复原后等待重新选择，以及关闭选中窗口时不切到另一个。测试识别窗口中心的颜色来确认选择，并检查预览窗口在收帧和停止时重绘、空闲时无定时器消息。macOS Actions 编译、链接 AppKit 和 ScreenCaptureKit，验证帧有效期、几何裁剪和停止后的拒绝更新。已有屏幕录制权限时也实采一个临时窗口，验证缩放和关闭。实际机型上的系统授权、最小化、全屏和混合 DPI 尚待单机验收。

Windows 启停回归重复绑定实际窗口 100 次，逐轮验证显示和停止清屏，记录停止后的工作集、句柄与 GDI 对象数，并核对预热后的句柄与 GDI 对象是否增长。

macOS 本机预览复用渲染上下文和串行采样队列，只在有采集时轮询。原生冒烟覆盖停止后立即清屏、显式重开同一窗口、来源关闭及空闲轮询退出。

## 必须遵守的边界

1. `Binding`、`SurfaceKey`、ViewId和本地票据不是认证。真实适配层必须将其绑定到已验证身份和独立用户授权；不可从不可信报文直接赋予权限，不可使用测试常量。重连须由可信运行时产生新连接绑定，不得复活旧gate。
2. `SessionGate` 不查询系统权限。原生权限撤销必须及时喂给模型，来源也必须独立执行权限与租约校验；空闲必须运行watchdog。窗口画面不等于窗口级输入隔离，Coordinator当前一律拒绝WindowOnly，只读和明确整机控制分开。
3. `InputRouter` 不注入按键。真实捕获/注入由原有运行时唯一拥有；清理失败不能伪造ACK，不能重放已提交输入。Coordinator只保证一个本地查看者的单目标路由，多个远端查看者之间的来源侧全局仲裁尚需原生owner接线。
4. 原始帧的“覆盖旧帧”仅适用于独立原始/已解码帧，不能照搬到H.264参考帧。网络适配器须独立超时、限流、取消，并在提交/展示时再次校验授权。撤权清空内部队列不等于外部GPU纹理已清除。
5. PresentationTicket只允许确认实际展示；收包、解码完成或提交GPU命令不算。期限从原接收时刻计算，不从晚取出时重置。旧票据/旧ViewId不能重定向到新窗口或新连接。
6. `ContentCheck` 是本地挑战模型，不是已接入的网络心跳。只有经认证来源确认采集健康且指定画面未变，才能消费挑战；不能靠定时器、本地重画或一般保活确认。
7. 隐藏/恢复只是命令模型。默认停放关闭；来源后端执行前重新核验当前事务、真实窗口、权限和几何，完成后才ACK。未确认恢复不得被timeout、close、retire或新事务抹掉。本次没有持久恢复账本，不能声称进程崩溃后可靠恢复。
8. 调度器输出不是实际GPU内存或带宽保证。捕获池、编解码器、外部纹理、在途缓冲和重配置重叠必须另外测量并限额；16条注册上限不意味着支持16路实况。
9. Coordinator应在串行控制循环运行，原生输入回调不能执行它的集合遍历、分配和状态协调。snapshot只是读取状态，不替代poll。

## 尚未实现

视频编解码、远程画面显示与窗口管理后端、生产安全媒体协议/传输、原生输入桥、来源侧多查看者全局仲裁、同源多View安全共享、实际scheduler接线、持久恢复账本、多会话GUI、移动客户端、NAT穿透/中继、虚拟显示驱动与真实双机验收仍未由本实验完成。缺少硬件不能掩盖这些软件尚需编写。

不安装工具/驱动，不改用户机器，不部署服务，不购买凭据，不自动上传用户屏幕，不更改许可证。保留的长期方案见 [LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md](../../docs/LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md)，其历史状态快照由本README及RW02详细方案补充。#286依赖#69，研究选型在独立#274；不要在main尚未合并时假定这些内容已经进入正式产品。
