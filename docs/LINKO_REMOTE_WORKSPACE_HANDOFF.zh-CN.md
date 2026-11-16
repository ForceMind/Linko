# Linko（屏跃）远程工作空间：工作对话研发交接入口

**当前入口已更新到 RW02 代码切片。不要重新实现已经提交的协调器，也不要将它误认成完整远控产品。** 用户授权在开发分支研究、实现、测试和提交；本轮没有自动合并 main 或其他分支。

## 1. 仓库、依赖与历史基线

- 正式仓库：`ForceMind/Linko`，https://github.com/ForceMind/Linko 。仓库 ID `1429285083`；原 `ForceMind/PeerGlide` 是同一工程的旧名。
- 主分支：`main`。接手时重新读取，不把以下历史 SHA 当作最新主线。
- 原远程基础：[PR #69](https://github.com/ForceMind/Linko/pull/69)，分支 `docs/linko-ui-window-bridge-20261107`，本轮依赖点 `a72355445e0596b300dd7f78351cfa43882c4a36`。
- 研究选型：[PR #274](https://github.com/ForceMind/Linko/pull/274)，文档提交 `72e58568ae9319e46ee0e636d445dd2ac2fcfdb5`。该 PR 独立存在，不能假定其文件已在本分支。
- 本次实际实现：[PR #286](https://github.com/ForceMind/Linko/pull/286)，分支 `feat/remote-window-rw02-coordinator`，base 为 #69 的分支，不是 main。
- 本次已通过实验包三平台 CI 的代码基线：`a4f23e0178aade220f23186136ceadc0850b5a10`。后续还提交了文档，最终 HEAD 和其 CI 必须实时核对。
- 并行集成 [PR #180](https://github.com/ForceMind/Linko/pull/180) 不由本 PR 自动导入、改写或合并。

读取当前 main、#69/#274/#286/#180 的精确 HEAD、审查、相关 Issue 和 CI；保护本地未提交内容，不 reset、不强推、不覆盖并行工作。若 #69 或 #286 已合并，确认合并树后再选择基线；未合并时使用显式依赖分支，不从缺少 lab 的 main 另造一份。

历史记录保留：上一交接曾核验 main `5d9476376b218567330cce0b5c74c9fb2e8c70a7`，旧代码基线 `9b5b3b59acc1e399e08588652423bf1678c04e77`；[原交接全文](https://github.com/ForceMind/Linko/blob/a72355445e0596b300dd7f78351cfa43882c4a36/docs/LINKO_REMOTE_WORKSPACE_HANDOFF.zh-CN.md) 可回读。其“先实现 RW02”是历史任务，不是要求再创建第二个协调器。

## 2. 阅读顺序

先遵守 AGENTS.md，读取 README、DEVELOPMENT_PLAN、VALIDATION、LOCAL_CODEX_HANDOFF。然后阅读：

| 入口 | 用途 |
| --- | --- |
| [LINKO_REMOTE_WINDOW_RW02.zh-CN.md](LINKO_REMOTE_WINDOW_RW02.zh-CN.md) | 本次详细实现、API契约、失效矩阵、原生接线步骤与验收缺口 |
| [实验包 README](../labs/remote-workspace/README.zh-CN.md) | 实际代码能力、运行命令和安全边界 |
| [coordinator.rs](../labs/remote-workspace/src/coordinator.rs) | 统一会话/视图生命周期和恢复意图 |
| [coordinator_faults.rs](../labs/remote-workspace/tests/coordinator_faults.rs) | 可执行跨模块故障测试 |
| [LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md](LINKO_REMOTE_WORKSPACE_IMPLEMENTATION.zh-CN.md) | 保留的 RW01–RW09 技术基线；旧状态快照用本文补充 |
| [LINKO_WORKSPACE_VISION.zh-CN.md](LINKO_WORKSPACE_VISION.zh-CN.md) | 产品愿景、手机、多设备工作空间与资源边界 |
| [WINDOW_BRIDGE_RESEARCH.zh-CN.md](WINDOW_BRIDGE_RESEARCH.zh-CN.md) | 单窗口代理/虚拟屏路线及原 WB 研究 |
| [LINKO_UI_DESIGN.zh-CN.md](LINKO_UI_DESIGN.zh-CN.md) | 原视觉方向，核对并行UI而非覆盖其实现 |

#274 固定提交中的三份研究/架构/实施文档由新详细方案链接。原生可行性、媒体协议、codec和手机/WAN仍需分切片实现；不能用这套文档代替相应工程代码。

## 3. 本次已实现的代码

保持零第三方依赖、独立 Cargo workspace/lockfile、Rust 1.95、MIT 和禁止 unsafe。既有 geometry/session/routing/scheduling/media/handoff 保留。

新增 `coordinator.rs`，负责：

- 最多16个有界注册条目，固定本地viewer身份、不可复用ViewId、当前来源几何/查看布局、终态关闭和清理后注销。
- 独立观看/控制授权；重授、撤权、几何改变时统一废除旧画面/票据/路由；WindowOnly始终拒绝。
- 一帧缓存与一次性展示收据：收到/解码/取帧均不等于显示；展示期限从原始接收时刻算，重复/旧代次ACK拒绝。
- 唯一InputRouter、当前坐标预检、目标切换释放屏障、空闲watchdog、时钟异常关门；不新增原生hook。
- 默认关闭的来源停放，独立许可、事务token、隐藏与恢复效果确认、失焦和故障后的恢复义务保留。
- 单View终态故障以及精确Binding下所有窗口关闭；不打断无关连接的输入。只有释放和窗口恢复都已确认才能删除危险条目。

新增36项跨模块故障测试，与原有53项单元测试合计89项。测试覆盖授权/展示/输入重放、时间边界、几何/布局、六类故障、连接级关闭、恢复迟到ACK、容量与100次授权循环。100次模型循环不是100次真实进程启停或物理跨屏。

此库不打开网络端口、不捕获屏幕、不注入原生按键、不移动系统窗口。ViewId/Binding/fixture不是密码认证。Raw RGBA不是H.264或生产安全媒体协议。

## 4. 验证记录与未验证项

| 候选 | 工作流证据 | 结果 |
| --- | --- | --- |
| 历史 `9b5b3b5…` | [Rust CI 42357961468](https://github.com/ForceMind/Linko/actions/runs/42357961468)、[Lab 42357961758](https://github.com/ForceMind/Linko/actions/runs/42357961758) | 原交接曾核验通过，仅对应旧候选 |
| 初版 `caa17e9…` | [Lab 43817061806](https://github.com/ForceMind/Linko/actions/runs/43817061806) | 三平台测试/Clippy/模拟/build通过，fmt失败，不能算全绿 |
| 代码修正 `a4f23e0…` | [Lab 43817627210](https://github.com/ForceMind/Linko/actions/runs/43817627210) | 三平台完整实验包检查成功 |
| 同一代码候选 | [Rust CI 43817627214](https://github.com/ForceMind/Linko/actions/runs/43817627214) | 本文写入前仍在运行，须独立核验 |
| 本文之后的最终HEAD | #286验证评论及该HEAD Actions | 不把上述旧run套到新SHA |

编译/测试在GitHub Actions执行；当前编写容器没有本地Rust编译与可用克隆网络证据，不声称本地通过。未关闭任何检查，不用重试抹掉失败历史。

**仍需实际开发的软件**：Win/Mac选源和采集、原生presenter、编解码与WebRTC窄桥、身份绑定媒体信令、原生输入桥、来源侧跨viewer全局仲裁、生产资源调度接线、同源多View安全共享、持久恢复账本、GUI、手机、公网和可选虚拟屏。硬件不在此环境不能掩盖这些软件尚未完成。

物理Windows/macOS双向、Bluetooth/USB、混合DPI、系统权限/锁屏、IME、窗口停放恢复、实际显示和性能均NOT_RUN。C06/Issue #3仍是独立门槛；模型和CI不能替代。

## 5. 下一项实际开发

先审查RW02的边界并运行既有测试，不重建已存在模块。当前Coordinator采用一注册条目一个View和单查看端路由；来源侧全局租约、同源多View共享及真实scheduler接线仍属于后续集成，不标为已完成。

下一可独立执行切片是RW03：使用系统公开API，完成显式选单窗口、本机只读预览和关闭/尺寸/权限事件回传，来源保持可见。Windows与macOS分别实现并原生编译，不因为一端成功而宣称双向支持。

随后RW04先固定libwebrtc依赖及构建，再做合成媒体、平台codec/呈现桥、帧元数据贯通，最后接入独立认证信令和真实双机只读观看。没有可靠帧关联和呈现确认前，不启用远程控制。

RW05复用原有唯一输入owner、v5、按键清理账本；RW09先显式发送/返回和可证明恢复，再接自然拖动。没有持久恢复与崩溃验证时默认不允许来源停放。多面板、手机、公网按原RW06–RW08增量推进，不变更原编号，也不把全部未来目标绑成一个巨大PR。

## 6. 接线不可破坏的约束

观看不是控制，配对不是所有权限。当前只读或明确整机控制；窗口画面不能伪装窗口级输入隔离。单Coordinator的单目标路由不等于多个远端查看者之间的全局仲裁。

适配层串行调用Coordinator，空闲运行poll；native回调只入有界队列，不执行集合遍历、识别、磁盘或网络I/O。snapshot只读，不代替watchdog。原生窗口/权限/输入owner对真实效果负责，ACK不能由超时或远端布尔值伪造。

每个迟到回调保留原ViewId/媒体代次/事务token。禁止按窗口标题、PID或最新连接重定向。恢复前重新核验原生对象和当前有效屏幕；不能把新应用误当作旧窗口移动。

独立进程/纹理仍需明确资源所有权、停止和回收。库清空内部缓冲不等于外部GPU纹理消失；撤权时适配器必须遮盖并停止展示。来源停放尚无持久账本，进程崩溃场景不能宣称可靠恢复。

保持MIT和notices，不复制GPL工程代码；新增依赖/协议需ADR和许可清单。不得自动部署、购买凭据、安装工具/驱动、修改用户机器权限、绕过系统保护或上传屏幕/按键/剪贴板/私钥。

## 7. 复现与回滚

先确认现有工具链与工作区。以下在仓库根执行；新coordinator测试在第一条cargo命令中：

```sh
git status --short
cargo test --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline
cargo clippy --manifest-path labs/remote-workspace/Cargo.toml --all-targets --locked --offline -- -D warnings
cargo fmt --manifest-path labs/remote-workspace/Cargo.toml --all -- --check
cargo run --manifest-path labs/remote-workspace/Cargo.toml --example workspace_simulation --locked --offline
cargo build --manifest-path labs/remote-workspace/Cargo.toml --release --all-targets --locked --offline
```

实验包之外，仍按AGENTS/原preflight执行相关主工作区检查和两套CI。缺工具记录缺失，不擅自安装。未来增加依赖要更新lockfile、构建、notice及命令。

当前增量回滚仅撤销新模块/测试/lib导出/文档，不涉及身份、配置、协议或驱动迁移。未来原生接线回滚前，必须先清理本会话拥有的输入、窗口、采集和worker，不能用删除UI条目丢弃未确认恢复。

## 8. 可直接交给工作对话的指令

> 正式接续ForceMind/Linko远程窗口研发，实际修改、测试和提交，不重建项目。先读取AGENTS、实时main/PR/Issue/CI和本交接；重点读取docs/LINKO_REMOTE_WINDOW_RW02.zh-CN.md。#286分支feat/remote-window-rw02-coordinator依赖#69，已有coordinator.rs和36项跨模块测试；#274是独立研究文档。重新核对精确HEAD与合并状态，不遗漏或重复lab，不把历史SHA当当前状态。复现89项实验测试和原工程CI后，审查RW02边界，继续RW03本机单窗口只读采集预览、RW04原生媒体桥/信令/双机观看、RW05原输入owner桥和RW09显式交接恢复。Coordinator不等于完整原生实现，保留默认不停车/只读优先和所有释放/恢复确认。保护并行#180/UI及本地修改，保持MIT、eframe、v5和唯一原生输入生命周期。只提交小分支PR，不自动合并main或部署/安装/变更用户机器。每个阶段给出精确SHA、真实CI、未实现软件、未测原生项和回滚方式。
