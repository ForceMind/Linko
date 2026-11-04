# 屏跃：本机 Codex 接手开发

这是既有 Rust 工程的延续，不是重建项目。仓库 ForceMind/PeerGlide，main，MIT。
先重新读取 GitHub 当前 main/PR/Issues/CI，不把本文件中的历史 SHA 当作最新状态。
PR #27 的云端基础候选 6ebeda55b22a3edba159c8f88f34f09784134fa8 已通过三平台
run 41587053713，合并为 d8b6c54239da741e64a706fa087fa0f9a0fa7509，源码树相同。
后续执行器/评估器/预检交付的最终证据见对应 PR 和 VALIDATION.md。

## 1. 先运行安全预检

在仓库根目录，使用已有 stable Rust（GUI 最低 1.95）和 Python 3：

```sh
git status --short
python tools/local_preflight.py
```

只收集环境版本而不构建时用 `--inventory-only`。报告和日志位于新的
`target/local-validation/preflight-…/`，脚本不安装工具、不修改系统权限、
不捕获原生输入、不上传报告或清理既有文件。它运行格式化、严格 Clippy、
测试、字体 notice 回归/许可筛查、Debug 构建和无原生注入的真实进程通信/重连测试。
如已安装 cargo-audit，可加 `--audit` 要求严格依赖审计；缺少该工具则失败，不自动安装。失败即停止后续检查。
`preflight_passed` 仅指代码/进程预检，报告中的原生桌面、蓝牙/USB/DPI/权限
验收仍是 `not_run`。工具缺失时先说明所需安装，不擅自改动机器环境。

## 2. 已有代码，优先复用

| 边界 | 代码 / 已验证范围 | 不代表什么 |
| --- | --- | --- |
| KVM | platform 捕获/注入源码、desktop session、core 租约/恢复、protocol v5；三平台 CI | 未证明真实 Win/Mac 跨屏可用 |
| 连接 | QUIC/TLS 双证书固定身份、能力协商、默认关闭的可信重连、结构化 worker 状态 | 有默认关闭的未认证 IPv4 发现提示；无短码配对/多节点产品闭环 |
| 手势 | gestures 8 种轨迹、拒识/分差、有界收集、合成回归 | 未完成真人数据和原生右键兼容 |
| G03 | core::gesture_broker 的按键归属/取消/目标/时间/边缘模型，1024 次模式转换回归 | 不是可直接放进原生钩子的低延迟实现 |
| 动作 | actions schema/风险/能力/上下文预检、local execution 协调器 | 没有 OS Backend，没有远程动作 RPC |
| GUI | 离线画布、主动标注导出、应用规则编辑与无执行预览 | 不是全局手势/应用覆盖层 |
| 配置 | 64 KiB、版本封装、旧配置备份迁移、原子替换、过期快照拒绝、OS 锁、显式恢复 | 不是私钥库/身份多文件事务或对抗恶意本机进程 |

`CLOUD_FOUNDATION.md`、`ACTIONS.md`、`GESTURES.md`、`GESTURE_LAB.md` 和
ADR-0002 至 0005 描述具体契约。不要为模拟竞品替换 eframe/QUIC 或另建全局钩子。

## 3. 第一项本机工作：C06 真实双机输入

先完成既有 KVM，随后才启用 G1 全局拦截。两端同时更新 v5；监听/连接角色不
代表永久主从。每台生成独立身份，只交换公钥证书并在独立渠道核对指纹，
不得共享 node.key.der。先跑无原生输入的 probe，再按明确授权启用 native/control。

记录真实 OS/build、硬件、显示器分辨率/缩放、蓝牙或 USB、权限、提交及结果。
macOS 对实际运行 app/terminal 授予所需 Accessibility/Input Monitoring；由用户
明确授权操作，不要求关闭系统整体安全防护。Windows 不绕过 UAC/安全桌面，
macOS 不绕过 Secure Input。缺少第二台电脑或权限时，逐项记未运行，不能用
SendInput、egui 或网络模拟替代物理键鼠验收。

覆盖四边进出、键盘跟随、修饰键/扩展键/IME、滚轮、不同 DPI、普通/全屏窗口、
权限拒绝及撤销、紧急恢复、接收端物理接管、断网/进程退出/睡眠唤醒。
沿 PRODUCT_EVOLUTION 第7节目标记录 500 次切换、8小时基础会话，以及关键故障
各100轮。不要采集真实键内容或把用户桌面截图自动推到公开仓库。
闭环前 #3 保持开放。#10 需独立定位历史失败，近期 CI 成功不能证明其根因。

## 4. G03 接线：单一捕获/仲裁路径

1. 保留 platform 原有生命周期、注入标记、逃生键、watchdog、线程回收与 reservation。
2. 原生回调只做来源/优先级/按键账本和有界队列决定。G03 reference model 有分配和
   识别，不能把 `handle` 整体直接塞进钩子；以测试契约实现同步短路径和后台识别。
3. emergency/撤权/物理接管优先；RemoteInjected/ActionInjected/RecoveryInjected
   不识别、不重新转发。短右键恢复完整一次点击对，目标已变则取消；长失败轨迹不
   回放为拖动。已放行 down 的 up 继续放行；抑制 down 的最终 up 保留归属。
4. 准备真实本机上下文：设备、窗口/目标不透明 token、焦点、配置、触发代次，使用
   单调时钟并限制寿命。生产 token 必须每个工作进程重新生成，不能复制测试常量。
   PID/时间戳不是认证。重启/切换/失焦/撤权使旧目标失效，不跟随新焦点。
5. Pending/Tracking 阻止边缘切换；结束/取消后先进入屏幕内部才重新武装。验证
   普通右键、CAD/游戏拖动排除、组合键、队列满、事件丢失、退出和丢权限。
6. 消费票据和 actions 预检使用同一个本机单调时钟基准，保留原始截止时间；不能
   在排队或执行时重置有效期。OS 句柄留在本机，只通过 token 查询。

## 5. G01/G04 原生动作和单机体验

实现 actions::execution::Backend 的 Windows/macOS 适配，真实能力默认空，
确认支持才声明；在提交前再次核验 OS 焦点/目标/权限。区分 Ctrl/Command、
CloseTab/CloseWindow/QuitApplication、MaximizeWindow/ToggleFullscreen。
先完成低风险动作；破坏性动作需要明确确认。动作注入要独立记账，失败释放自己
拥有的合成键，不能释放用户物理按键或重新识别为手势。

协调器只返回 Submitted / 明确失败 / UnknownOutcome 等，不把 OS 提交描述成应用
执行完成。相同 ID/请求返回既有结果，不自动重放；冲突 ID 拒绝。保留上限128和
5秒期限。跨进程重启或远端会话不能依靠这个本地缓存承诺“恰好一次”。

接通不依赖身份/配对/网络的本地 worker，提供独立、默认关闭的 G1 同意。
规则文件/输入授权/预览开关都不能代替此同意。原生 app ID 来源做平台规范化，
精确匹配，排除优先；不要猜测窗口标题。GUI 仍需原生应用选择、权限诊断、
不抢焦点覆盖层、托盘/菜单栏及启动同意。不要把目前 preview 当作已执行动作。

## 6. 采集、评估与数据隐私

打开 GUI Gesture lab；保存只在用户主动点击时写入 canvas-local JSON，不自动上传。
预期标签必须由标注者独立给出，不能改成识别器预测以美化结果。评估命令：

```sh
cargo run -p peerglide-gestures --example evaluate --locked -- SAMPLE_FOLDER
```

工具输出混淆矩阵、正确/错误/拒识、负样本误匹配和比率，坏文件使评估失败；
不打印样本内容/路径。窗口内 egui 帧事件不是原生 125/500/1000Hz 证据，文件数量
本身也不能证明5名独立测试者。沿路线图建立经同意的800正样本/2000负样本，
>=95% / <=0.5% 目标不降低，破坏性动作的0误执行需另做真实执行验收。

## 7. 尚未实现的后续软件：不是“已完成、只缺真机测试”

| 后续范围 | 真实状态 / 依赖 |
| --- | --- |
| mDNS、短码配对、多设备闭环 | 已有默认关闭的 IPv4 mDNS 地址提示和显式广播；跨主机/网卡/权限未验收，短码与多节点闭环仍未实现，详见 DISCOVERY.md |
| 剪贴板文本/图片和文件传输 | M4 产品未实现；已有 peerglide-transfer 离线存储基础（分块/完整性/重扫续传/非覆盖提交），但无网络、独立授权或 GUI 接线；剪贴板、流隔离、配额和历史仍需研发 |
| 远端语义动作 | G2 尚未实现；本地 Coordinator 不是远端协议，需独立权限、目标 token、顺序屏障和会话去重 |
| 多显示器、Linux输入、签名安装 | 仍是后续任务；Linux目前为 GUI/通信诊断，签名/公证需凭据与发布授权 |
| OS 私钥库/ACL、身份文件事务 | 未完成；不要把 GUI 配置存储当成密钥存储或原子身份迁移 |

上表包含可以在云端继续编码的软件，不应伪称都被硬件阻塞；它们按原路线依赖
暂缓。本次交接提供当前 M1–M3/G1 可独立验证的基础与本机接线入口，不宣称整个
产品或全部未来非硬件功能已完成。完成 C06 后，继续维护原有 Issues #4–#7、#11、#12。

## 8. 可直接交给本机 Codex 的指令

> 继续 ForceMind/PeerGlide，先读取 AGENTS.md、本交接文档、最新 main/PR/CI 和
> docs/VALIDATION.md。保留现有实现及 MIT。先运行 tools/local_preflight.py，修复
> 实际失败，再完成 Windows/macOS C06 双机验收；记录未运行项，不伪造通过。
> 在同一个原生捕获生命周期内实现 G03 同步归属/有界队列，提供真实焦点上下文，
> 接通 G01 Backend 和默认关闭的单机 G1 worker/GUI；复用现有 rules、execution、
> store、gesture_broker 和 corpus evaluator。不要复制测试常量为生产 token，
> 不把后台模型/识别/分配放进钩子，不新增竞争钩子。权限或工具安装需说明并获
> 授权，不关闭系统安全。按现有依赖继续第7节未开发的软件任务，分 PR，运行
> fmt/Clippy/测试/构建与对应平台CI，保护真实键内容/剪贴板/私钥。不要把提交、
> 模拟测试、截图或无错误构建当成实机验收，不部署服务或更改仓库可见性。

## 9. 字体依赖与审计约束

GUI 已迁移到 eframe/egui 0.37，最低 Rust 1.95；核心协议仍为 v5。
CI 的 cargo-audit 0.22.2 拒绝漏洞和 advisory warning；不要加通用忽略项。
字体授权文件纳入包内 notices，Python 回归必须随构建运行。
参见 ADR-0006；RustSec 的“停止维护”提示不是已发现可利用漏洞的证明。

离线文件存储契约及剩余安全边界见 TRANSFER_STORAGE.md；不要把 PeerBinding 或测试 fixture 当成认证/授权，不把该 crate 描述成跨机发送或原生拖拽。

## 10. 2026-11-04 数据协作增量（PR #32）

新增可独立验证的 peerglide-transfer::{data_policy,clipboard,manager,codec}。
文本引擎有 UTF-8/大小/敏感分类/回环/版本冲突/重放限制；文件管理已接原有
Receiver，支持独立接收授权、显式接单/续传、并发/累计接收预算、取消保留和提交
前再次验权；数据编解码器在分配前检查长度。集成测试真实走解码到磁盘提交。

这些代码尚未接生产网络或系统剪贴板，不能称作跨设备复制/发送已可用。
Manager 预算仅覆盖该对象生命周期；重连/重启前需盘点已有残留数据，不能靠
新建对象重置配额。见 CLIPBOARD_ENGINE.md、TRANSFER_MANAGER.md、DATA_FRAMING.md。

下一批可继续编码的工作仍包括：独立认证数据流与响应/调度、持久化磁盘配额和
历史、授权界面、短码配对、多设备闭环。这些不是“被真机阻塞”的工作。
本机专属工作仍以 C06、唯一原生捕获路径、焦点/app-ID、系统动作和权限为先。
当前会话只有用户 Mac 终端和 GitHub 连接，没有独立云开发执行器；本增量通过
GitHub 分支与 CI 验证，不在用户 Mac 安装工具或启用输入/剪贴板。
最终候选 CI 与审查限制记录在 PR/VALIDATION，未跑项仍保持未验收。


## 11. 本机接管：持久接收预算增量（2026-11-04，PR #33）

本地克隆 main 5d947637，核验无开放 PR；main run 41784077840 与 #32
最终候选 run 41783406794 均成功。`python` 不存在，使用已有 python3 运行
预检，报告显示 rustc/cargo 不可用，fmt 无法启动，后续检查 not_run。
没有安装工具、授予输入权限或读取系统剪贴板；真实 GUI/worker/C06 尚未运行。
用户明确暂不安装 Rust，继续代码和 GitHub CI；本机运行验证客观受阻。

当前分支 persist-transfer-admission：接收根目录独占锁、跨会话/重启的持久
预留、旧暂存盘点/导入和配额复用，见 TRANSFER_MANAGER.md。修复旧测试中重建
Manager 只计当前续传、不计其余残留的问题；保留未知提交结果和独立授权。
最终候选尚需 fmt/严格 Clippy/全 workspace 测试、构建及三平台 CI，不能以
代码已写入代替通过。后续仍有 G03/native context/Backend、独立数据网络协商、
详细持久历史/清理、原生剪贴板和界面；不启用全局手势，不修改协议 v5。


代码候选 bc304939 已通过完整五项 run 41788814220；11组新增回归、Linux
workspace 140项（transfer 40项）通过。独立静态审查发现并修复了不完整暂存、
相对路径/cwd 与目录条目容量边界，复核未发现新增问题；不等于安全审计。
PR #33 的最终文档候选以其 exact-head CI 为准，尚未合并。后续 GUI 隔离入口
PR #34 独立交付。当前 main 的已校验 ARM64 开发包也已在本机通过真实
probe/managed-stop/reconnect/pin-change/backoff-stop；无 native/control/权限改动。


## 12. 本机接管：隔离 GUI 验证入口（2026-11-04，开发中）

已重新核验 main 5d947637、无开放 PR，以及 main run 41784077840 成功。
本机无 Rust/Cargo，python3 预检在启动 fmt 时失败；用户明确暂不安装工具链，
继续代码与 CI，不应再次自动安装。真实双机和 C06 保持未运行。

本增量增加 GUI `--data-dir DIRECTORY`，供仓库内隔离启动；缺省仍用现有用户
配置，不搬迁或覆盖身份/信任/授权。Linux GUI smoke 使用显式目录。待最终
候选三平台 CI 和可用的 macOS 开发包启动/无原生输入 worker 验证，不把代码
或 CI 代替实际启动。文件持久预算修复另见 PR #33，GUI 分支现叠加存储分支，两个 PR 分别审阅；
它们都未合并，后续须分别核验最终受测树。全局 G1/native context/Backend、
数据网络与持久详细历史仍未完成，#10 原历史故障根因仍未知。


本机已绕开工具链缺失完成**运行验证**（并非绕过 OS 防护）：从 main CI
41784077840 下载并校验 ARM64 包，仅在仓库 target 内解包。现有 smoke.py 和
reconnect_smoke.py 在本机回环真实进程通过：认证 probe、受管停止、服务重启
重连、证书替换拒绝、退避中停止。未开启 native/control，未接触用户 GUI 配置。
这是 main 预编译 worker 的证据，不是本地编译或新 GUI 入口的验收。
