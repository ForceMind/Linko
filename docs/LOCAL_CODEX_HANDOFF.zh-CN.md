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


基础修复已分批提交推送：4e8ce93/d7d44c4为Up事务，6b5ace0/49379f1为接管
后要求interior且同事件不反向Acquire。CI41959403280当前Linux/Mac/最低Rust/
审计通过，Windows在打包；最终结果需回读。当前root未提交session WIP按约定
CaptureEpoch/CaptureRoute与begin_routed消费，等待platform phase1接口落地后
共同编译，不能把旧CI当此WIP已通过。platform worker独占lib/windows/mac及必要
capture_thread，root独占core/session/GUI/docs，reviewer只读。Capture域必须
记录捕获时的Local/Remote与代次，匹配当前outgoing才发送；Position/Toggle也
要fence，旧本机事件不可撤销新incoming。Remote/Consumed Down的Up跨disable/
队列满/stop仍归原域；普通stop若500ms未收齐Up，须保持worker/hook等候而不是
main自行退出把原hook杀掉，GUI明确force除外。

后续本版必修还包括接收端先保护再接管：remote修饰/按钮held时不能把第一个
本地事件先放给OS再异步cleanup；拟首次物理对仅收回控制、不回放动作，clear
保护须等旧账清理成功，Consumed pair仍等Up。Esc/watchdog/队列满/源失效须
有明确恢复语义，不能靠清计数或不受控无限阻塞制造成功。Mac tapDisabled不应
继续接受新控制，额外鼠标侧键不能别名成中键或被静默吞掉，begin失败须回滚。
以上尚在实现，不是完成声明。发送背压无界的初步审查结论已撤销：既有session
send本来有250ms超时，未发现绕过路径；自身pointer缺TAG/previous失步假设也未
证实，不为这些假设重写路径。物理矩阵仍未运行，原权限与无Rust决定不变。


PR43/49379f1的完整五项CI41959403280已通过，已转待审阅，范围只含Up事务与
边缘接管，不称整版可用。当前保留全部WIP切到capture-route-ownership，叠加43，
继续同一0.2.1-dev候选，不stash/reset。root session已接公开opaque CaptureEpoch/
CaptureRoute、四种routed Captured、begin_routed、incoming本地域fence与精准
outgoing代次匹配；并用shutdown_poll/shutdown_pending在首次停请求后500ms报告
未知，但保持async worker等原thread/reaper，不因超时自行进程退出。已reaped的
SourceLost/restore错误直接结束为Unknown，不永久死等。新增纯路由与async liveness
回归，尚待完整集成CI。platform phase1正在编辑唯一生命周期及配对表，phase2
Receiving首事件保护仍为已知必修。README补Win/Mac中文双端操作入口，仍标实机
未验收与不可绕过系统防护；不是新能力声明。后续Git提交由root统一选择相关文件。


platform phase1已在检查点冻结，root即将统一commit：CaptureRoute/epoch、固定
pair账本、stop等Up、shutdown_poll/pending、begin回滚、tapDisabled源失效、额外
侧键fail-open均已写入，只有静态证据，待CI。CAPTURE_LIFECYCLE补phase1契约，
明确Receiving首事件P1仍开放。下一步先修真实fmt/编译/回归并独立只读复核，再
推进phase2接收保护与watchdog/失败恢复，不能让保护模式变成无恢复通道的本机
无限拦截。root统一Git；worker检查点后不得覆盖fmt修复。


Phase1源码5cedae946bb36e83300948224c4c698cd1232069完整五项CI41964917548
全过，Native/typed tests三平台、smokes/GUI/Release/最低1.95/审计均通过。
incoming退休fence永久续租的新P1已由Root补前置协调+成功/失败回归，readonly
复核通过；SourceLoop异步drain计时从首次shutdown_poll前开始，不额外宽限send。
Native父四文件继续冻结，worker只在新增receive_driver.rs准备phase2 Driver/Engine/
Backend与fake回归（不接mod，未入phase1）。未来Snapshot用Active/Draining/
Retired/RecoveryRequired，异步命令250ms截止+取消序号fence，heartbeat带epoch/
observed_at；Native loop独立owned ledger恢复，Core退账不得重复OS Up。Mac
Submitted只指SDK请求，不是OS或应用效果。接收保护必须有独立逃生/失败返回通道。
用户已确认Win+Mac且每端一个主屏；不扩多屏。当前文档跟进不改运行源码，最终
文档head CI以PR44为准；随后小分支接phase2，仍同0.2.1-dev目标，不把phase1
绿灯宣称可日常工作。真实C06/宿主权限/无Rust决定保持不变。


Phase1最终文档head383ec384a4c49b21ae9f0d071863d6e46232884d五项CI41967524520
全过，PR44已ready，code与5ced一致。当前native-receive-recovery/PR45：705180b先
仅提交冻结receive_driver.rs和基于HEAD构造的test-only pub mod入口，真实纳入
编译；此步骤只写Git index，完整Parent工作文件没有覆盖。d54036a按CI修65个
fmt hunks，同样保留Parent WIP。旧14测试类型检查在1.95通过，三平台Clippy发现
两处if可合并，writer已在当前WIP修正；不能把尚未运行的新18测试记为通过。

生产WIP目前：Root独占apps/main.rs/session.rs、context三入口与CI Debug build；
writer独占receive_driver.rs、lib/windows/mac/G1 literal最小适配。Parent增加
CaptureRoute::Receiving、plain Option driver/control、Native loop单一OS owner与
waker、coalesced takeover、Consumed/旧Remote modifier屏障、额外键按钮账本、
wheel remainder epoch、Mac按钮事务、健康/观测缺口。Root Core只在Submitted且
still current后记协议ack，退账不重复发OS Up；Draining保持Recovering，不凭
release Submitted报Local。managed stdin先写watch stop再直接request_stop+wake
Native控制器，Arc ptr registration防旧scope清新slot，不依赖Tokio才恢复。
CaptureSession原本!Sync而整体future可Send，因此撤销“加Mutex改done”的预防性
建议，done结构未动；Root全用短借用或&mut跨await。context可用性只读，Win
confirmed_input_desktop，Mac AX/IM+非SecureInput，不在callback查询/修改权限。

Phase2 checkpoint仍未共同编译：writer补18个engine及9个Parent软件回归，真实
原生输入没有运行。Readonly窄审强调enter_receiving Err不可已发布保护、SDK/inflight
不是OS/app效果、release Submitted不是Retired；又发现SourceHealth只loop开头查
缺口会在长SDK之后、下一轮前盲补Up。writer当前窄改Backend按同一观测基准在
每次效果/cleanup前确认时效并补回归，完成后Root统一提交完整接线进CI，保持
v5/单hook/独立授权。所有未提交WIP需保留，不能清理/stash/reset；新18/Parent
以及接收保护尚未验收，不得宣称双机已可日常使用。目标仍Win+Mac各单主屏。


Phase2共同编译检查点现已齐全：writer全文件冻结，20 Engine fake+Parent回归
未实际运行；Root apps async adapter/direct stdin stop、context可用性入口与CI
Debug build已准备。Parent扩CaptureRoute::Receiving；public Driver/Control等由
platform根重导出，Root已改根import，不访问private receive_driver。实际Native
父loop/waker/Consumed/force/健康/SourceObservation、Mac BUTTONS和Win单batch双轴
scroll已接。Root将统一commit push整批（此前705/d540只是test-only入口检查点），
再修真实fmt/类型/Clippy/测试，不能把先前14测试类型检查当新20/生产路径已通过。
确定P1观测缺口已由writer修到每次效果与cleanup前；追加纯helper+慢pointer/慢Up
fake，源已不可确认就不盲补剩余CtrlUp。Readonly Parent其余未见确定新缺陷，
建议补fresh第二次Esc完整路径回归（当前还没跑/不视作关闭）。普通cancel最终guard
之后仍可有in-flight本次SDK，但owner保证后续收尾无迟到新Down，不夸零调用。


共同接线已commit push bbc3f1e；首轮1.95只报一个旧回归fn非async，Root修为tokio
async并应用115个fmt hunks后f680c3b；再按CI修签名格式610dc90。完整Native接线
的Linux类型检查已过，三平台检查仍待继续，不得声称新20/Parent测试已运行。
Writer当前窄修真实HookState强制返回：不再用全局required/outcome准备chord，改
can_force_return(exact epoch, phase==FAILED)；DRAINING第一次cleanup失败后required
虽true，也不能让中途开始的chord提前force。新增3个真实HookState/Control/Engine
假后端回归（ACTIVE/DRAINING中途失败、旧mods/重复Esc、全新FAILED完整chord一次
force、ADMISSIONS_CLOSED健康仍true与旧Up归属），代码已写，未编译。临时旧patch
在/private/tmp不再apply，源码包含新版。Root将统一提交并用CI修实际失败。


本版完整软件候选746b07e1022c2939627f09ff048cca89955e7dd6五项CI41974906260
已全过，含三平台Debug+Release/严格检查/工作区回归/真实无Native smokes、GUI/
最低1.95/审计；Engine20与Parent freshEsc/观测/配对等现为实际自动验证。Readonly
源观测/force入场问题已修并复核，不是独立安全审计。Win/Mac artifact及内层ZIP
摘要核对且无用户身份/配置，未签名开发包。Mac包smoke/reconnect/context在本机
全过；GUI用旧0.2.0隔离profile启动0.2.1，中文新说明/默认未勾选确认/取消/只读
权限/exit0已验，旧identity字节未变，native/allow仍false，无任务进程残留。
Mac AX/IM仍未授予，无真实捕获/注入；Win机器不可操作。匹配包在ignored target
basic-kvm-746b07e/mac和win，已给用户入口并异步问现在能否双机试跑，回答待到。
此为进入C06前的具体候选，不能mark整个双机工作Goal完成或C06通过。最终记录
详VALIDATION/GUI_INTERACTIONS；本次最后文档提交不改运行源码，精确head CI与
PR45待审阅状态下一步回读。main未合并、MIT/v5/单hook不改、G1/数据仍后置。
Native设备/系统权限必须用户手动明确处理，不装Rust、不绕过防护、不发布稳定版。


## 23. 减少双机登记负担（2026-11-05）

用户已可试跑，但现有证书搬运/手输路径与分次索取信息造成阻碍，暂停要求用户搬文件。
从PR45/d8557c5干净工作区新建easier-peer-enrollment；main回读仍5d947637，PR33–45
未合并。现有0.2.1包及隔离试跑GUI保留，不改系统权限/工具链，不启原生输入。
当前增量0.2.2-dev：公开连接信息复制/粘贴、实际网卡IPv4（多网卡明确选）、自动填写
角色/证书/地址、取消示例IP预填。两端仍各需交换一次并独立核对完整指纹，短码/一键
配对尚未实现，不能把信息自带证书/校验值当认证。输入协议、TLS固定证书及独立许可
不变。root独占UI/main/deps/docs，复用localize_panels仅写新编解码模块，不递归派工。
受限文本与版本/证书解析、全文快照失效、自身证书拒绝、只改草稿/只读拒绝是本批检查。
预检cargo/rustc缺失，保持用户“不安装Rust”决定；待CI验证，不以本地diff检查代替运行。


PR46运行候选7d3e035五项CI41987328608全过，新增12条材料/登记/小窗口回归运行。
Mac实际复制/粘贴、自身拒绝、多网卡草稿/独立CLI指纹一致已验；f7ff780实测长文本
溢出后补ScrollArea限高及回归，7d3e035宿主按钮可见。宿主随后锁屏，最后登记未做；
测试GUI留未登记草稿。无真实输入/授权/系统权限改变，实际Windows证书仍未取得，
先不要继续要求用户搬文件。两端各复制一次的便利流程不是短码/一键配对，本目标
未扩成新网络认证设计。最终文档提交与7d3e035运行源码一致，精确head CI继续回读。


## 24. GUI空闲内存事故诊断（2026-11-05，未定位）

用户截图屏跃6.83GB，明确只打开窗口、未操作。暂停双机试跑及在用户Mac继续启动GUI，
不把它归因于用户粘贴或连接，也不建议清系统/加内存。当前ps未见本项目残留；上轮隔离
GUI会话56338回读exit143。不能从退出码推断泄漏根因，旧02:24的0.2.0 ips不是本次证据。
从PR46/5497542干净分支开gui-memory-diagnostics，main仍5d947637、PR46未合并，最终
源码和CI仍有效但只短时GUI，不能代表长驻资源稳定。预检仍因Rust未装停止，不安装。

root检查应用与接线独立CI诊断，review_admission只读依赖，localize_panels仅新增
受保护CI内存采样脚本/对应测试，不递归。静态未证实按帧FullOutput/纹理/font缓存无界
累加，不能先降低100ms重绘假称修复。将用隔离CI分别启动空身份/已有身份GUI，RSS
有绝对cap及预热后增长cap，超限停止仅测试child；不启worker/native/网络/授权。
RSS缺少压缩/GPU部分，macOS CI环境与真实Mac的差异必须记录。当前没有根因或修复
结论，不发布新可用性承诺；继续观察CI/必要原生分配证据，不能以脚本通过关闭事故。


首轮b8d490f普通五项CI42017317157与两平台驻留CI42017317320全过；未修改App代码。
Mac180秒×2峰RSS116320/120512KiB，增长0/3904KiB；Linux峰134636/136360KiB，增长0。
Mac CI为26.6.2/25G83，宿主26.2/25C56，不能把不复现当修复。PR46因事故退回草稿；
PR47本批仅诊断也为草稿，Issue5评论6656842353跟踪。补Darwin vmmap每15秒及预热/
末尾只取physical footprint和reported peak数值，timeout2秒；用同上限停止，缺测记
unavailable/null，RSS-only不报全内存通过。17条纯/小child回归本地通过，真实新口径CI
待跑；不要重新启动用户Mac GUI，不更改权限或系统环境，不作已修好结论。


## 25. 用户恢复双机通信试跑（2026-11-05）

用户要求再次连接Windows；采用已有0.2.2 CLI避开GUI驻留，不再要求搬证书文件。
Windows公开连接信息实际收到并通过原有完整指纹核验，Mac原身份不变。用户在Windows
登记Mac并开始监听后，Mac真实QUIC探针通过mutual TLS/v5/可靠往返，exit0，约0.63s，
峰RSS10688KiB（guard15s/128MiB）；native/allow均false，探针已正常结束。真实材料与
地址只在ignored target，本记录不复制私钥/地址/指纹。网络策略未改变。

下一步原生输入的实际worker权限只读检查：辅助功能、输入监控均denied，Secure Input
inactive；已集中请用户在Mac手动处理两项，等待回复“已开启”或“没有屏跃”。Windows
暂不启用Native，物理C06全部仍未验收。不得把真实网络通过或CLI低RSS等同KVM已可
日常工作/GUI内存修复。GUI事故PR46和诊断PR47仍保留草稿，#10根因未定位。


## 26. 用户已授予屏跃权限，等待短时原生试跑确认（2026-11-05）

用户完成系统设置后，直接CLI的Denied来自TCC把responsible归给自动化宿主，不能
继续让用户重复添加屏跃或扩大宿主权限。正常应用启动的GUI自身已核验AX/输入监控
Granted、Secure Input未检测到。Documents内测试profile会触发额外目录权限提示，未
接受；改用0700私有tmp同身份副本（原资料保留、文件0600），状态指针保存在ignored
c06-pilot-021/active-private-pilot.json。源码/签名不改，native/allow仍false。

有界GUI检查10s预热/256MiB绝对/64MiB增长/3min，采样峰118352KiB，未开Native即
到期停止。CUA观察已关闭App会重开默认profile，已关该意外窗口且未编辑；以后先
核对目标PID，关闭后不再getApp。当前已请用户Windows启用接收偏好和Native监听，
回复“开始试跑”才在Mac开启本次仅Mac→已核验Windows的短测；Mac不接收控制。
实际worker权限自检和C06物理项尚未完成，GUI内存事故也未定位。不得提前宣称通过。


## 27. 真实Native失败后的最小包身份修复（2026-11-05）

用户已明确开始本次Mac→Windows短测。Mac GUI自身Granted，但spawn worker认证后
AX前置检查失败exit1，未捕获原生输入；物理C06未跑。精确TCCsubject：GUI自身是
bundleID允许，helper responsible为GUI却按GUI文件路径拒绝。旧包Info.plist not bound、
无CodeResources、linker生成main identifier。不要再让用户给旧App重复授权或给宿主扩权。

新分支macos-worker-permissions从5f87736，版本0.2.3-dev；root负责GUI实际worker只读
Checker/版本/CI/文档，localize_panels独占package.py/test_package.py，review_admission只读
复核。候选从helper到app ad-hoc签名，绑定plist/资源且strict验证；默认DR不放宽，无私有
entitlement/DeveloperID/公证/凭据购买/全局安装/系统安全修改。7条打包mock回归过；root
本机只对新临时App副本执行真实签名和验证过，旧artifact及身份不变，尚未验证TCC改善。
GUI新增独立worker权限快照，仅固定diagnose-context，受限输出/3s截止/取消，不启输入。

CI将跑原检查并解压包验证签名、篡改plist/helper负例。源码输入v5不变，Windows的v5
包仍可协商；正式候选以完整测试包为准，不混用本机GUI/CLI。C06和GUI内存事故仍未过，
真实新worker权限自检必须在正常应用启动且用户授权的候选上复验；旧ad-hoc授权不保证
跨build自动沿用。当前无活跃Native，Windows可以停止旧会话保持配对，先等待候选。


0.2.3运行候选c9307aa的五项CI42039285409与两平台驻留42039285155均过；5条GUI
worker诊断回归（Win3条）及7条打包回归过，Mac ZIP解压+篡改拒绝过。Mac包artifact
13419264044已下载至ignored worker-permissions-c9307aa/mac并在本机strict/metadata验证。
新包main ID和plist绑定/资源seal正确；这不是已验证TCC修复或物理C06成功。

本机尝试只读复查遇到锁屏，已异步请用户手动解锁，尚未得到回复；不要尝试代解锁。
该新GUI受监测到期停止（峰126400KiB），无Native/worker诊断执行。解锁后先查精确PID，
不存在则使用正常应用入口并显式带active-private-pilot.json所指profile，切勿让CUA默认
重开无参数profile。检查“设置→检查实际工作进程权限”后，才能决定是否需要用户对
新版重新确认一次系统授权；不要扩大宿主权限或重配设备。旧saved native=true只是偏好，
allow=false，普通连接和只读诊断不启用Native。本批最后文档提交不改运行源码；保留
c9307aa完整包用于实机复查，最终head CI及最新证据以PR48回读为准。


## 28. 用户授权完整参考Oink，基础资源迭代（2026-11-05）

用户给出oinkvm/oink并说明作者授权，随后明确确认原创代码可MIT集成/分发，第三方另计。
已固定ignored target/reference/oink为3ca82127，15个crate/UI/原生附加/打包/测试的覆盖索引
与安全差异在REFERENCE_RESEARCH。不运行对方源码或安装工具，不把full-trust/默认数据
开启/签名字段存在当独立授权。采用现有技术栈/许可/中文/v5，当前独立实现没有复制代码。
root负责集成与GUI活动调度，复用localize_panels只读平台/数据/UI和本批窄复核；网络片
由review_admission完成，其后UI片capacity失败已由root/localize接手，不新增或递归代理。

新分支idle-gui-activity从f931f4d，版本0.2.4-dev。去掉无工作GUI的100ms自循环，保留
child/stop/reconnect/discovery/worker诊断/画布/layout-report的tick；postUI补首tick。
关闭策略移到logic，隐藏也要合作stop并等待确认，恢复未知不自动close；普通通信退出
等最终stdout EOF/ProbePassed，避免idle后丢报告。新增6条回归待CI，原生代码未改。

用户后来解锁，c9307aa新包实际worker只读诊断已执行；GUI/worker均Denied，尚需对最终
代码身份确认权限再进Native。设备身份/Windows配对不动，Mac不接收控制；不把签名或
新诊断通过当实际KVM可用。当前GUI已结束，无活跃Native。先完成本批资源候选测试，
再集中给用户最终包与必要系统确认步骤，避免中间包反复授权。GUI6.83GB根因仍未知。
