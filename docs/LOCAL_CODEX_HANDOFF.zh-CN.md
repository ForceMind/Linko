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


## 12. 本机接管：隔离 GUI 验证入口（2026-11-04，PR #34）

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


最终代码候选 c933ba2 已通过完整五项 run 41790024756；同一候选 macOS ARM64
包在本机 macOS 26.2 (25C56) 通过真实 probe/停止/重连/换证拒绝/退避停止。
GUI 显式指向仓库内独立目录，实际窗口已渲染离线主页、无身份/无对端、native
关闭；关闭窗口进程退出0。目录仅实例锁，未创建身份/证书/设置。未安装 Rust，
未做原生输入或系统权限变更；C06、G1原生接线和后续数据软件依旧未完成。
一次桌面控制调用异常耗时约51分钟，不作为应用延迟或耐久证据。详见 VALIDATION。
PR #33 最终 2d2bed5 完整矩阵通过，已可审阅；PR #34 叠加其上，最终文档
跟进提交仍以 exact-head CI 为准。两者未合并，main 未改变；不可把已受测候选
自动描述成合并主线，后续合并须核对源码树。继续不要自动安装工具链。


## 13. 界面语言：默认简体中文（用户明确要求，2026-11-04）

当前 GUI 的主页、设备、显示器、传输、设置、手势实验室/规则、发现、状态与
恢复提示已改为简体中文。后续界面继续使用中文，不交付英文占位原型；技术
标识、协议/配置键、动作ID、导出JSON、原始诊断和用户自定义名称保留原值。
内置 Noto Sans SC（OFL-1.1，来源固定且摘要已核对），不依赖系统字体安装。

中文候选 d3891c6 通过完整五项 run 41798545549；Linux 中文主页/实验室截图
已实际查看，字形无方框，主动导出与评估通过。相同候选的 macOS ARM64 包已在
本机 macOS 26.2 (25C56) 实际打开，标题“屏跃 · PeerGlide”，中文主页正常。
测试窗口留给用户查看，使用 target/local-validation/gui-zh-d3891c6/profile，
仅有实例锁，仍无身份/对端/native 输入。没有安装 Rust 或系统字体。
PR #34 的最终 README/记录提交只改文档，精确最终 CI 及 ready 状态见 PR。
PR #33 已完整通过且待审阅；PR #34 叠加其上，两者未合并，main 仍保持原线。
真实 C06 与 G1 原生接线、数据网络/详细持久历史等剩余软件未完成，不得因
中文界面或 CI 通过而关闭相关产品验收。#10 历史根因仍未定位。


## 14. 本机前台/焦点/权限提供器（2026-11-04，PR #35）

native-context 分支叠加 #34。实现真实 Win/mac 只读前台应用和焦点采样、同一
原生资源前后核对、显式 diagnose-context CLI 及设置中的中文权限检查按钮。
完整契约见 NATIVE_CONTEXT.md；不会装输入钩子、执行动作、自动申请权限，
诊断成功也不授予输入/手势能力。默认命令不输出应用标识；明确请求时才输出，
Windows 路径可能含个人目录，不可自动上传诊断报告。窗口标题从不读取。

该采样提供器不能替代连续焦点通知/代次与不可重用的进程内票据；这些和 G03
原生仲裁、Backend/owned injection、本地 worker 独立同意仍须继续开发。
用户不安装 Rust 的决定保持；本批最终 CI 和本机只读诊断尚待，不能伪称已验收。


代码/测试候选31ae8be已通过完整五项 run 41806308409，Windows/Mac原生只读
诊断、Linux明确Unsupported、原有probe/重连/GUI/最低Rust/审计均通过。
修复了CLI测试模块位置、CFRelease声明一致性和smoke不可用状态对应关系，
未忽略警告、未跳测试；#10旧故障根因不变。独立静态审查复核无新增问题。

8f3847a的Mac完整job包（运行源码与31ae8be一致）已下载校验并实际运行：
本机前台应用信息可用，缺少系统访问条件的焦点路径如实拒绝，不伪称焦点成功。
应用标识只在测试内存核对，默认输出无ID；个人权限详情报告仅在target本地，
不上传。中文GUI的权限按钮也已实际点击，未启用控制/native或申请权限，
目录仍仅实例锁。测试窗口保留。无Rust/系统字体安装，无新输入钩子。

下一步仍按原目标继续：连续焦点/目标事件失效、生产随机token与单调期限、
G03共享捕获短路径、本机Backend/owned injection账本、独立默认关闭worker；
缺OS授权和第二台Windows机器的正向实机/C06不能由采样/CI代替。
界面保持中文。另观察到原生深色选项与可见主题疑似不一致，单独待定位，不
扩大此PR修复范围。#35最终跟进为README/记录及过期的规则页脚说明；原生提供器、CLI与权限按钮
逻辑不变，exact-head CI见PR。


## 15. 原生通知失效与有界上下文票据（focus-invalidation，开发中）

本轮从已完成且未合并的 #35/1a648f4 新开 focus-invalidation。主线仍5d947637，
当前实现 context_watch.rs、context_events_windows.rs、context_events_macos.rs：
只读WinEvent/会话/电源与Workspace/AX通知，回调只设置原子失效位；不是第二套
键鼠捕获。原捕获生命周期、输入v5、独立授权及全局G1关闭状态保持不变。

原生回调可用固定大小 TriggerMark 记录原代次/单调起点，Worker 不得延迟后
跟随新target。OS随机worker nonce/target token在回调外生成；128绑定、原始
5秒以内deadline、消费一次、配置/事件/队列丢失/退出代次失效。Native observer
不支持时拒绝创建可用上下文，不以采样降级宣称持续保护。API 不提供执行授权。

代码和针对性回归正在写入，未编译/运行/提交；用户不安装Rust、不改系统权限
决定保持。后续必须独立审查raw callback生命周期/CF与Win句柄释放、跑完整CI
及显式只读observe诊断。不能把CF/MSG pump当成通知全量屏障或物理验收。G03
共享输入短路径、动作Backend/owned injection、默认关闭本地worker仍须继续。


PR #36 已建立，focus-invalidation 叠加 #35；均未合并。代码候选70bba44通过
完整五项 run 41818463792，含独立CLI特性、三平台回归、显式observer smoke。
修复ready ABA采用单一版本化原子值；128资源槽包含消费后仍持有的Lease。
完整shutdown fake ops回归覆盖部分安装/逐项失败/指针先detach/poison；清理
失败单独分类且不能让smoke通过。最低Rust保留1.95，当前stable弃用API改有界CAS。

Mac包已校验并实际跑只读观察：当前访问条件拒绝，前台信息可用，没有启动
输入或签发上下文，delivery_verified=false，不能宣称通知正向验收。报告只在
本地target，不公开OS个人权限或ID；没有安装Rust或改权限。最后加入显式取消
API及资源回归，observer适配/诊断运行逻辑不变，最终提交仍需exact-head CI。
Git ref传输服务曾失败，已用有expected SHA的非强制API快进恢复并回读，不强推。

后续仍需G03唯一捕获短路径的实际接线、Backend/owned injection、独立本机worker
同意和原生交付验收。现有通知API是异步的，pump不是全量屏障，不能用本批逻辑
回归代替C06或宣称G1已经全局可用；UI继续简体中文。数据网络/详细持久历史等
原路线任务继续保留，#10旧根因仍未知。


## 16. 本机窗口动作Backend（local-window-actions，开发中）

基于已通过且未合并的#36/c6bc593。实现platform/context/actions的真实本机
Backend+既有Coordinator流程，独立默认关闭动作同意（原始trigger前后固定代次），
使用真实上下文消费/晚期权限/期限复查。仅Win最小化/最大化/关闭窗口，Mac公开
AX最小化/关闭；不猜AXFullScreen、不用Zoom冒充最大化、CloseTab/Quit不alias。
键盘动作/owned injection账本仍未实现。所有OS成功仅Submitted，未知结果不重放。

代码和注入式回归正在实现，完整CI及独立静态审查尚待；没有二进制启用/派发
G1，没有实际操作用户窗口，不改OS权限/装Rust。Native action测试只能在明确
授权的隔离测试目标上做，缺权限的正向路径与C06必须如实保留未验收。所有界面
继续中文；G03唯一捕获接线、本机worker独立同意界面和后续数据协作仍需继续。
契约见LOCAL_WINDOW_ACTIONS.md，不把源码Native Backend描述成全局手势可用。


PR #37/local-window-actions 已建立，叠加未合并#36。窗口Backend运行候选86b4e71
通过完整五项run41826266699：三平台独立CLI/严格Clippy/测试/Release，原probe/
重连/只读observer/GUI，最低Rust1.95和审计。关闭控件禁用检查经独立静态审查
发现并修复、复核。全部窗口效果测试是injected fake ops，不操作真实窗口。
原生绑定已编译链接，实际Min/Max/Close效果、modal/提权/延迟仍未验收。

Native Backend真实路径复用Coordinator和上下文Lease，但没有二进制启用/派发。
动作同意独立、默认关闭，原trigger固定revision；替换/撤销/之后确认不能复活
旧动作。Mac仅公开AX最小化/关闭，Win最小化/最大化/关闭；其余明确Unsupported。
无键鼠合成，owned injection账本仍需继续；不将全屏/缩放/最大化或Tab/Quit混淆。
本机不装Rust、不改系统权限、不操纵用户窗口的边界保持。最后只改记录/说明，
最终exact-head CI及待审阅状态以PR37回读为准，main仍未合并。

后续按原目标接G03共享捕获短路径、键盘动作owned ledger、本地worker独立
同意与中文界面，再做明确隔离目标/授权环境下原生动作验收。不要用本批源码/
绿色CI/fake effect次数冒充全局手势、物理输入或真实Native效果已经可用。

## 17. 唯一捕获内的Windows G03短路径（native-gesture-broker）

基于未合并#37/84745a7，main核对仍5d947637。新增platform/gesture_capture、
预分配SPSC队列并接入现有Windows capture的可选分支，原KVM传None；无新增
全局钩子、无产品二进制入口。回调不识别/分配/查询目标/IO，用固定键位图和
五键状态、帧绑定准入与原子一次领取。独立同意、随机身份、焦点/权限/配置
代次继续约束旧trigger。发布/领取竞争由单一带事件代次的latch和交错回归保护。

普通放行down/up不被中途接管；抑制down后取消/停机仍抑制对应物理up，停机
超时保留原线程/reaper预约。已知源丢失单独锁存，不能说右键已经恢复。
跨进程OS文件锁协调相同用户临时目录的合作进程，保留旧Atomic及真实TLS join
边界，不删/截断用户文件；不防恶意本机替换或不同TMPDIR。详见CAPTURE_LIFECYCLE。

Mac G1尚未接：CGEvent PID/state不能证明物理来源，不用PID0/HID1猜测，不改
原Mac KVM tap。Windows入口仅库API，手势worker/识别/规则dispatch、owned
injection账本、普通短右键一次恢复、独立中文同意界面/覆盖层仍未实现；因此
不能把本批编译/逻辑测试称为全局手势可用。界面中文保持，没有安装Rust或
更改宿主权限，没有启动宿主捕获/操作用户窗口。正向原生效果/物理C06未运行。

本批验证要求完整三平台、最低1.95与审计五项CI；当前进展和exact-head结果以
本分支PR为准。合成回归覆盖pair/cancel/full/stop/watchdog、准入并发、零分配、
SPSC真实双线程非Copy清理，以及跨进程锁/TLS析构，不替代原生事件验收。
后续继续本机worker和owned recovery、Mac可靠来源，再推进原计划数据网络等；
#10历史根因仍未确认，主线未合并，不把CI当独立安全审计或稳定版交付。

PR #38现已建立，叠加未合并#37。代码候选feb3f7a通过完整五项run41836066218，
包括三平台严格检查/回归/Release、原probe/重连/只读上下文、Linux GUI、Mac
并行GUI压力、最低Rust1.95与审计；Linux193、Mac206工作区测试通过。早期
fmt和边界常量Clippy失败已修正，未跳过检查。准入发布/领取ABA、解锁失败
未传播、先算remaining再取clock会延长expires，均经针对独立静态审查修正并
回归、复核；不是完整安全审计。最后跟进仅更新记录，运行源码和测试保持。
最终head五项CI与待审阅状态回读以PR38为准。main保持5d947637，所有这些PR
尚未合并；未进行宿主捕获/动作/权限变更/装Rust。G1产品入口仍关闭，原生
输入/普通短右键恢复、真实权限/多机C06和#10旧根因状态不变，继续后续实现。

## 18. 短右键恢复与owned input（owned-local-input，开发中）

本轮基于已通过最终五项CI/待审阅未合并#38的2cd4be6，main仍5d947637。
目标把真实捕获End生成且不开放构造的Rust私有字段票据、原目标/期限/DPI/位置
复查、Win同batch独立tag的Down/Up与既有hook回执接成恢复闭环。SDK不保证
partial count是数组prefix，必须凭逐事件回执识别；count2无回执等不确定
结果保留pending，不重放点击。部分失败中只能清理确认属于本次的Up，旧
目标/同意失效后不得新增Down。已放行/抑制physical pair仍归原域。

root负责gesture_capture/recovery协议、LocalActions真实Lease衔接、原生只读
preflight和记录；review_admission仅做只读契约/代码审查；
localize_panels只拥有windows.rs既有hook接线/Native INPUT构造。不会新增钩子、
在回调做识别/IO/分配或运行宿主注入。Native callback只固定字段/原子决策，
实际SendInput发生在worker/capture loop且先释放TLS RefCell借用。Mac物理
来源仍未知，本批不猜测或启用。短click恢复完善后再接识别/规则worker；
全局产品入口仍关闭，中文界面保持，不装Rust/不改权限，验证仍须完整CI。
本节初始记录时尚未实现/编译/执行；后续实绩见下，不能把fake事件当物理验收。

PR #39/owned-local-input已建立，叠加未合并#38。代码候选ceecaa9通过完整五项
run41849905620，三平台fmt/独立CLI/严格Clippy/回归/Release/原probe/重连/
只读上下文、Linux GUI、Mac并行GUI、最低1.95和审计全部通过；工作区测试
Linux208、Mac221、Windows243。修复真实Windows导入编译与fmt失败，无忽略。

端到端的库调用已接：真实Native Prepared lease+sealed End→复查整段短点击/
原坐标/窗口DPI/route/原同意与期限→同batch独立随机tag INPUT→既有hook的
Allowed/Blocked回执→有限已知缺失Up清理。不是app完成/全局G1交付。四态slot
复用、外部注入坐标/运动、已排队Up后的foreign-right冲突经针对性独立静态
审查发现、修复与复核；另补重试在途重置和重复Down几何边界回归。完整安全
审计仍未运行，#10旧故障根因不变。旧的两个superseded CI已取消释放Mac排队，
最终候选完整五项照常执行；最后跟进仅记录，受测运行/测试代码保持不变。

原生实际点击/驱动/延迟/睡眠/撤权/C06未运行，Mac恢复与键盘ownership仍
Unsupported；source silent-removal不能靠heartbeat证明连续性。产品二进制
不调用恢复API，GUI仍中文且全局G1关闭。main核对仍5d947637，未合并。
后续接recognizer/rules worker与独立中文同意/诊断，继续Native source证明和
原生验收，再推进数据原路线。不要以本批mock回执或绿色CI当实机验收。

## 20. UI线框与交互重设计（用户新任务，2026-11-05）

用户清除了旧Goal，转为UI线框/交互重设计，并要求每个可审阅步骤和版本均
commit+push。当前chinese-ui-wireframes从owned-local-input/e7d0778派生，
保留已推送的PR40/local-gesture-worker/db7a680及其CI，不继续扩大Native任务。
对标按仓库MouseInc/Aitiy为主、Deskflow/ShareMouse为跨屏辅助；已刷新官方资料，
独立设计、不复制资产。用户明确允许仅本地原型的浏览器交互检查，不访问账号
或真实设备。设计入口docs/design/README.zh-CN.md；先0.1信息架构，再线框原型
与完整流程，逐步提交。实际系统能力/实现状态不能被原型模拟状态冒充。


UI设计已完成0.1信息架构(fcb991c)、0.2可点击线框/SVG(fad471d)并分别push。
0.3修复浏览器实际发现的恢复重启/画布丢失，以及复核发现的暂停/撤权/只读/
弹窗急停，22项Node状态测试通过。用户授权的本地浏览器流程已点验，保留
http://127.0.0.1:8051/标签页作为设计交付；服务器仅公开docs/design，无账号/
系统权限/真实剪贴板文件或原生输入操作。SVG与PNG可离线查看。新增GUI尚未
迁入eframe；不要把目标原型模拟成已可用能力。细节见docs/design/README与
VALIDATION。旧Native增量#40仍留在独立分支，目标已清除，当前只做UI设计。

## 21. 线框迁入实际GUI（actual-gui-interactions，开发中）

用户明确要求按0.3线框落地eframe，实际接线、中文、未实现标注，每步/版本
commit push。本分支从#41/8d644e6开，保留旧Native worker #40独立分支。
0.2.0-dev实现五页导航/真实证书向导/通信与Native显式模式/设备权限和布局/
规则画布/只读恢复/常驻停止。新增input IPC（无内容）和terminal最终软件
清理状态，Auth不等于控制，旧Local缓存或EOF不等于清理证明；原Input v5不改。
root拥有GUI main/desktop_ui与platform restore_checked，localize_panels只改
protocol worker及desktop main/session；gui_mouse_editor只改rules/lab，readonly
reviewer只审不写，均不递归。第一候选源码已写入未编译；本机无Rust决定不变。
说明GUI_INTERACTIONS.zh-CN.md。不得用演示原型填充实际GUI或打开未接通功能。


本批实际源码c0a1882通过完整五项CI41883855509，包含三平台严格检查/测试/
Release/真实进程smokes、Linux实际GUI绘制与显式导出、Mac多轮并行回归、最低
1.95与审计。fmt、测试Debug断言、egui测试API、严格Clippy和旧smoke混合解析
均按实际日志修复，原连接顺序断言保留，新增input Disabled与最终terminal断言。
针对只读审查修正重复停止/超时覆盖已到达terminal的竞态，旧Local缓存、读取
失败与未终态EOF都不能充当恢复成功。每批修复已commit push，无强推。

SHA校验的同候选Mac产物已在本机以独立目录实际验收：中文五页/禁用规则保存/
中文名称和路径粘贴/真实身份指纹登记/独立接收许可保持关闭/仅回环通信/停止/
再次probe/布局持久化/深浅色/只读权限检查/关闭窗口；GUI、worker和测试peer均
exit0，无残留。没有--native/--allow-control、全局G1或数据共享。Mac AX与输入
监控未授予，没有改权限。宿主沙箱内初次启动134无日志，允许WindowServer后
成功，不是#10根因定位。CUA观察已关闭App会重开默认窗口，已关闭未编辑；用
真实隔离进程退出码确认收尾。细节GUI_INTERACTIONS、VALIDATION。私钥/配置/
宿主截图不上传，测试产物留ignored target。最后跟进仅文档，源码/测试/tools/
workflow与c0a1882一致；精确最终head五项CI与PR42就绪状态以远端回读为准。
#42叠加未合并#41，main未合并；后续授权仍只沿本次GUI迁移，不恢复旧G1/数据
完整Goal。物理C06、Native动作、权限变化、中文IME、托盘/配对码/应用选择器/
生产数据流仍未验收或未提供，不能把CI当完整独立安全审计。


## 22. 本版优先基础 Win/Mac KVM（2026-11-05）

用户明确下一版先用于 Windows + macOS 两台电脑工作，剩余G1/数据暂缓。
从最终五项CI通过的#42/51c9eb9开basic-kvm-readiness，main刷新仍5d947637，
旧PR均保留未合并；本机仍不安装Rust，预检因cargo/rustc不可用停在fmt。
本批开发版本0.2.1-dev，保留v5/唯一捕获/独立许可与恢复机制，不宣称物理C06通过。
root负责core/session与GUI接线，review_admission只读审session/core，
localize_panels审基础platform并协作capture路由契约，不递归、不扩G1/数据。

已定位并修复正常Up的早删账：pointer或原生Up注入失败时，不得先丢失已持有
的key/button；新input_with在Down尝试前登记潜在责任、Up成功后才移除，未持有
Up在定位/注入前拒绝，重复Down保留autorepeat。实际session走同一路径，新增
key/button故障/不重放回归；当前待CI编译与验证，不能记已通过。
待本批修复：接收端physical takeover不得同事件反向Acquire；capture时route/
epoch必须防止队列中的旧本地事件或旧lease事件按处理时状态转发到新lease。
后续基础软件检查与两平台包围绕这两项继续，具体CI及真实设备结果记VALIDATION。
