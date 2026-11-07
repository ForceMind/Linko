# Linko（屏跃）远程工作空间：工作对话研发交接入口

交接日期：2026-11-07。

**用途：直接接续已有研究与代码，不再从零规划，不将离线基础实验误认成完整远控产品。** 用户已授权在分支上继续研究、实现、测试和提交；后续由用户决定合并。

## 1. 仓库与基线

- 当前正式仓库：`ForceMind/Linko`，https://github.com/ForceMind/Linko 。仓库 ID 为 `1429285083`；原 `ForceMind/PeerGlide` 已改名。旧文档中的旧路径是历史记录，不代表另一个工程。
- 主分支：`main`。本次只读核验的主线提交：`5d9476376b218567330cce0b5c74c9fb2e8c70a7`。
- 接续分支：`docs/linko-ui-window-bridge-20261107`。虽保留历史 docs 前缀，已包含实际 Rust 实验代码。
- PR：[#69](https://github.com/ForceMind/Linko/pull/69)，本次核验时开放、未合并。
- 已验证代码基线：`9b5b3b59acc1e399e08588652423bf1678c04e77`。
- 本交接文件是该基线之后的文档增量，不修改运行代码。不要把上面的代码基线当作未来最新 HEAD，也不要将旧 CI 结果自动套用到新提交。

新工作对话第一步必须重新读取当前 main、接续分支 HEAD、PR #69 的评论/审查、其他并行 PR、相关 Issues 与精确候选 CI。保护工作区未提交内容；不得 reset、强推或覆盖他人工作。PR #69 未合并时，从其最新 head 接续或创建明确依赖它的小分支，不能仅从 main 开始后遗漏这些文档和实验包。若已合并，核对合并内容后从最新 main 接续。

## 2. 已具备的方案与阅读顺序

先遵守 `AGENTS.md`，并读取 `README.md`、`docs/DEVELOPMENT_PLAN.md`、`docs/VALIDATION.md`、`docs/LOCAL_CODEX_HANDOFF.zh-CN.md`。再按以下入口接续：

| 文档 | 作用 |
| --- | --- |
| [LINKO_WORKSPACE_VISION.zh-CN.md](LINKO_WORKSPACE_VISION.zh-CN.md) | 产品长期目标、手机控制端、多设备工作空间、权限与资源边界 |
| [LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md](LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md) | 核心技术实施基线：模块分工、Win/Mac 适配、媒体/输入通道、RW01–RW09、验收与回滚 |
| [WINDOW_BRIDGE_RESEARCH.zh-CN.md](WINDOW_BRIDGE_RESEARCH.zh-CN.md) | 虚拟扩展屏与单窗口代理两条路线、第三方项目与 WB 研究任务 |
| [LINKO_UI_DESIGN.zh-CN.md](LINKO_UI_DESIGN.zh-CN.md) | Linko 视觉与交互方向、屏幕拓扑和远程工作空间的区分；结合最新并行 UI PR 核对，不直接覆盖其实现 |
| [labs/remote-workspace/README.zh-CN.md](../labs/remote-workspace/README.zh-CN.md) | 实验包实际能力、复现命令、接口安全前提与缺口 |

上述材料已足以开始下一研发切片；它们不是冻结的生产规格。原生可行性、生产媒体协议、硬件编解码及手机/WAN 的细节仍需通过原型、ADR、代码审查与验收逐项确定。遇到结论变化，更新方案及其证据，不沿用未经核实的第三方宣传。

## 3. 已实现与未实现

`labs/remote-workspace` 是现有工程中的隔离 Rust 包，独立 workspace/lockfile，无第三方依赖，禁止 unsafe。已有六组基础逻辑：

- `geometry.rs`：画布与来源坐标、缩放、等比显示、黑边、旋转、负原点和逻辑/像素空间。
- `session.rs`：独立观看/控制授权、对象/会话/授权代次、过期与撤销、真实展示确认以及静态内容确认挑战。
- `routing.rs`：单目标输入预检、防重放、当前映射核验、切换释放屏障与空闲失效检查。
- `scheduling.rs`：同源多视图复用、可见性/聚焦调度、资源预算、降级和暂停。
- `media.rs`：有界 RGBA 参考帧、校验后分配、单帧缓存、旧授权/过期数据拒绝。
- `handoff.rs`：来源窗口交接/恢复事务模型，需要真实后端确认，不伪造完成。

另有 `examples/workspace_simulation.rs`，串联双来源 fixture 场景；当前基线含 53 项单元测试。没有打开网络端口、捕获桌面、注入系统输入、创建虚拟屏幕，也没有移动端客户端。模拟标识不能用于生产认证，RGBA 参考格式不能冒充已加密的视频协议。

**仍需开发的软件**：实际会话/视图协调器、Win/Mac 原生采集与呈现、编解码、安全媒体传输、真实输入接线、多会话 GUI、移动客户端、公网连接，以及可选虚拟屏和自然拖窗。缺少真机不等于这些代码已经完成。

## 4. 本次最终核验的 CI 证据

针对代码基线 `9b5b3b59acc1e399e08588652423bf1678c04e77`，本次通过 GitHub 连接重新查询两套 Actions，均已完成并返回 success：

| 工作流 | Run | 本次核验结果 |
| --- | --- | --- |
| 原项目 Rust CI | [42357961468](https://github.com/ForceMind/Linko/actions/runs/42357961468) | completed / success；更新了上次交付时仍在运行的状态 |
| Linko Workspace Lab | [42357961758](https://github.com/ForceMind/Linko/actions/runs/42357961758) | completed / success；Windows/macOS/Linux 实验包测试、严格 Clippy、格式、模拟、release |

这是既有候选的 CI 证据，不是代码安全审查完成或物理双机验收证明。PR 中既有自动审查摘要指向早期文档提交 `b6f6174`，不能据此声称新增 Rust 代码已经得到同等审查；合并前审查实际最终差异及最新评论。

本交接文件提交后形成新的 HEAD，应检查该 HEAD 的 CI。不要因为本次仅改 Markdown 就将上述 run 标为新 HEAD 的运行结果。失败历史保留，不通过关闭检查、重试掩盖不稳定性或降级权限来制造全绿。

## 5. 第一项实际开发：RW02 会话与视图协调

沿已有实现推进 RW02，不另造一套权限、输入或传输基础。首个交付目标是把现有纯逻辑组织成可测试的完整生命周期，再进入真实采集观看闭环。

RW02 至少覆盖：

1. 会话、Surface、View 的有界注册/删除与身份、代次、映射版本约束；来源和查看端不能凭 UI 名称混淆；多查看端仲裁与授权隔离。
2. 连接、授权、呈现、聚焦、失焦、暂停、撤权、断开、来源消失及重新创建的事件路径；watchdog 在没有新用户输入时也生效。
3. 把 `SessionGate`、`InputRouter`、`LatestFrame`、调度器和交接模型串起来。切换或撤权时先停路由，确认旧目标输入释放；来源被移除后不能丢弃待释放/恢复责任或伪造 ACK。
4. 资源生命周期采用可验证归属；旧资源回收失败时明确保留待清理状态，不能先占用新资源再假定总预算成立。布局变化使旧输入映射失效。
5. 全链路故障注入测试：重复/乱序/迟到事件、旧会话包、撤权后再授权、视图销毁、来源退出、丢失释放/恢复 ACK、时钟异常、预算耗尽、停止中的重连。
6. 扩展可执行模拟并说明其 fixture 边界。先红后绿覆盖具体问题；更新技术方案、实现状态和验收记录，不把模拟作为真机证据。

RW02 完成后，按依赖继续 RW03/RW04：用户显式选源、来源保持可见、另一端实际观看，然后单独授权控制。先做“Mac→Windows 与 Windows→Mac 各一个来源”的真实闭环，不要求手机、虚拟屏和无限画布一次完成后才交付。

## 6. 后续执行原则

- 使用现有 Rust、eframe、可信身份/QUIC 和唯一原生输入生命周期；不重建项目，不重编号原 M/G 里程碑。媒体扩展需独立能力协商和 ADR，不把视频夹进现有 v5 输入协议。
- 观看不是控制，配对不是所有权限。窗口画面不是窗口级安全输入隔离；没有可靠限定就只读或明确申请整机控制。
- 无限画布是客户端布局，不是无限分辨率；默认输入只进入一个明确目标，不做隐式群控。手机作为控制端不意味着手机也可任意被控。
- 原生控制接线遵守 C06 与现有安全门槛。设备/SDK不可用时，记录具体未运行项并继续独立软件任务；不要伪造验收，也不要将所有任务都归为硬件阻塞。
- 代码参考先核对固定版本与许可；保持 MIT 和已有 notices。不自动安装驱动/工具、不修改用户机器权限、不部署服务、不购买凭据、不绕过系统保护。
- 保护用户屏幕、按键、剪贴板和私钥；不默认记录或上传这些内容。截图和性能数据必须是实际测量并获准采集。
- 使用可审查的小提交/小 PR，说明范围、依赖、测试、未验证项与回滚。用户尚未授权现在合并，不自动合并 main，也不覆盖并行 UI 工作。

## 7. 接手后的复现命令

先确认工作区、现有工具链和本地授权。以下命令针对仓库根目录执行，不包含安装或系统权限操作：

```sh
git status --short
cargo test --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline
cargo clippy --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline -- -D warnings
cargo fmt --manifest-path labs/remote-workspace/Cargo.toml --all -- --check
cargo run --manifest-path labs/remote-workspace/Cargo.toml --example workspace_simulation --locked --offline
cargo build --manifest-path labs/remote-workspace/Cargo.toml --release --all-targets --locked --offline
```

当前实验包要求 Rust 1.95 或更高。缺少工具先报告，不擅自安装。还须按 AGENTS/现有预检运行相关原工程检查，不能只运行 lab 后宣称完整应用通过。后续增加依赖或调整隔离方式时，同步更新 lockfile、CI、notice 和复现命令。

## 8. 可直接交给工作对话的指令

> 正式接续 ForceMind/Linko（原 PeerGlide）的远程工作空间研发。这不是重建项目或重新写计划。先读取 AGENTS.md、当前 main/PR/Issues/CI，以及 docs/LINKO_REMOTE_WORKSPACE_HANDOFF.zh-CN.md。PR #69 的接续分支是 docs/linko-ui-window-bridge-20261107；历史已验证代码基线为 9b5b3b59acc1e399e08588652423bf1678c04e77，必须重新核对最新 HEAD。若 PR 未合并，从该分支接续或创建明确依赖它的小分支，不要仅从 main 开始而遗漏代码。复现已有实验测试，先实现 RW02 会话/视图协调和故障注入，再按技术方案推进 RW03/RW04 实际选源、采集、编码、安全传输与显示。之后沿依赖推进单目标远控、多会话画布、移动端、公网和可选拖窗。实际修改代码、测试、修复、提交，不只给计划；不要每完成一个文件就等待确认。保护现有主程序与并行 UI，保持 MIT、现有安全门槛和原生输入单一生命周期。区分已实现、测试通过、未实现及真机未验证，不以模拟代替验收。只在开发分支推进，不自动合并 main，不越权操作用户电脑、安装驱动或部署服务。每一可交付阶段记录精确 SHA、验证证据、剩余任务和回滚方法。
