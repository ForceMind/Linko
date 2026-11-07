# Linko 远程工作空间：可执行离线基础实验

这是 PeerGlide/Linko 现有工程内的隔离研发包，不是另起一套应用。它实现并测试未来多设备远程工作空间的纯逻辑，不改变现有 eframe、QUIC、原生输入生命周期、v5 协议或 UI 分支。

## 当前已有代码

- `geometry`：画布缩放/平移、保持比例显示、黑边拒绝、旋转、负坐标显示器和逻辑/像素坐标映射。
- `session`：每个来源/查看端/会话代次绑定；观看与控制分离；权限过期、撤销、断连、重建。观看授权另有代次，防止重新授权后接受旧画面；静态桌面使用短期来源确认挑战，而非用本地重画假造新鲜度。
- `routing`：唯一输入目标、序号防重放、当前布局/来源版本失效；切换目标必须等待旧目标实际输入释放的确认，即使新目标尚无授权。
- `scheduling`：可见性与聚焦优先级、同源多面板复用，以及解码数量、参考像素缓存、像素速率和目标码率预算。
- `media`：固定头部先校验后分配的有界 RGBA 实验帧、来源/观看授权代次绑定、单帧替换缓存、撤权清空及过期丢弃。**不是 H.264，也不是安全传输协议。**
- `handoff`：接收端确认可用后才请求隐藏来源窗口；失败后保留恢复请求，直至原生后端实际确认恢复。中途改变权限或几何会取消旧交接，重新授予权限不会使它复活。
- `examples/workspace_simulation.rs`：把上述组件串联起来的双来源离线场景，执行断言后输出明确的 fixture 结果。

## 执行

需要 Rust 1.95 或更高版本。本包没有第三方依赖、无网络或 OS 权限需求：

```sh
cargo test --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline
cargo clippy --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline -- -D warnings
cargo fmt --manifest-path labs/remote-workspace/Cargo.toml --all -- --check
cargo run --manifest-path labs/remote-workspace/Cargo.toml --example workspace_simulation --locked --offline
```

包使用独立 Cargo workspace/lockfile，避免更改并行研发中的主工作区。`.github/workflows/workspace-lab.yml` 独立检查 Windows/macOS/Linux；原有 CI 保留。后续生产接线需先 ADR，不能因本实验通过就把它当作已上线功能。

## 必须遵守的边界

1. `Binding` / `SurfaceKey` / 本地票据不是认证。真实适配层必须将其绑定到已验证身份和独立用户授权；不可从不可信报文直接赋予权限，不可使用测试常量。
2. `SessionGate` 不查询系统权限。原生权限撤销必须及时喂给 gate，来源也必须独立执行权限与租约校验；空闲时必须运行 watchdog。窗口画面不等于窗口级输入隔离，没有可靠实现时只能观看或明确授权整机控制。
3. `InputRouter` 不注入按键。真实捕获/注入仍由原有运行时唯一拥有；清理失败不能伪造 ACK，不能重放已提交输入。布局变化需提供当前映射，来源几何只能由可信平台注册表提供，变更必须更新版本。
4. 原始帧实验的“覆盖旧帧”仅适用于独立原始/已解码帧，不能对 H.264 参考帧照搬。网络读取要独立超时、限流、取消，并在提交/展示时再次校验授权。
5. 隐藏/恢复只是命令模型，不是系统窗口已移动的证据。隐藏前立即重核验，OS失败保留恢复意图并报告；不要通过虚构 ACK 宣称恢复完成。
6. 调度器输出是目标预算，不是实际 GPU 内存或带宽保证；编解码器、驱动、缓冲、拥塞、重配置重叠和实际码率必须另行测量和限额。
7. `ContentCheck` 是本地挑战模型，不是已接入的网络心跳。只有经认证来源确认采集健康且指定画面未变，才能消费挑战；不能靠定时器、本地重画或一般网络保活调用确认。

## 尚未实现

原生 Windows/macOS 采集、视频编解码、显示与窗口管理后端、生产安全媒体协议/传输、会话/视图协调器、多会话 GUI、移动客户端、NAT 穿透/中继、虚拟显示驱动与真实双机验收均未由本实验完成。硬件缺失不能掩盖上述仍需编写的软件。

不安装工具/驱动，不改用户机器，不部署服务，不购买凭据，不自动上传用户屏幕，不更改许可证。完整技术方案、追加任务、参考版本与验收矩阵见 [LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md](../../docs/LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md)。最终测试结果以 PR #69 精确候选的两套 CI 为准。
