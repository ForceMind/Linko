# Reference research

Inspected 2026-10-22. This is a scoped source/design review, not a complete audit of every dependency. No source from these implementations is copied or linked into PeerGlide.

| Reference | Snapshot | License evidence | Decision |
|---|---|---|---|
| [Lan Mouse](https://github.com/feschber/lan-mouse) | `f1b96bdcc0294d285bc9050b7e146800a3ef8796` | LICENSE GPLv3; Cargo.toml GPL-3.0-or-later | Study only; do not link capture/emulation crates |
| [Deskflow](https://github.com/deskflow/deskflow) | `c1b19244a37bd238038ea2bf0ab56d9efc2de5d7` | LICENSE GPLv2; LICENSES contains GPL-2.0-only, LGPL-2.1-only, OpenSSL exception and MIT texts | Study only; per-file licensing differs |
| [Input Leap](https://github.com/input-leap/input-leap) | `34a34fb20b93113a6b26052cb5a54f9be2327775` | LICENSE GPLv2 with OpenSSL exception; protocol header refers to GPL | Study only; repository is archived at inspection |
| [ShareMouse](https://www.sharemouse.com/) | Public website at inspection | Commercial; no implementation source reviewed | UX ideas only; no nonpublic implementation assumptions |

## Lan Mouse

Read Cargo.toml, LICENSE, input-capture/src/windows/event_thread.rs,
input-capture/src/macos.rs, lan-mouse-proto/src/lib.rs, src/capture.rs and
src/emulation.rs. Workspace separates input capture, emulation, event vocabulary,
IPC and frontends. Windows uses message-loop low-level hooks; macOS uses an event
tap and display bounds. Its protocol has enter/leave acknowledgement and liveness
events. Capture release paths include explicit release, send failure, remote
leave and destruction. Certificate fingerprints are surfaced for authorization.
The manifest uses Tokio and DTLS-related dependencies; it is not Quinn code.

Borrow the general ideas of adapter boundaries, event origin handling, explicit
transition acknowledgements and timeout release. PeerGlide uses an independently
authored JSON protocol with bounded frames, normalized absolute QUIC datagrams,
mutual TLS enrollment and receiver-authoritative short leases.

## Deskflow

Read LICENSE, docs/dev/protocol_reference.md and src/lib/server/Server.cpp.
The protocol documentation distinguishes network client/server from input
primary/secondary; it would be inaccurate to claim Deskflow can never separate
those roles. It describes TCP, TLS, connected/active states, display information,
clipboard and keepalive messages. The server has switch delays, two-tap arming,
corner handling, input event handlers and disconnect cleanup.

Borrow topology as explicit data, edge hysteresis and safe release requirements.
Do not reuse wire identifiers, C++ logic or GUI assets. PeerGlide's stronger
peer-oriented goal is symmetric input grants without permanently assigning an
input primary, not a claim that network-role separation is novel.

## Input Leap

Read LICENSE, src/lib/inputleap/protocol_types.h and src/lib/server/Server.cpp.
The header describes enter/leave, key press/release, clipboard sequence context,
resolution acknowledgement and keepalive expectations. The server implements
screen switching, delay/two-tap gates and client-disconnect handling. Treat it as
mature historical evidence, not an actively maintained dependency recommendation.

## ShareMouse

The public site describes bidirectional Windows/macOS control, edge navigation,
clipboard sharing and cross-computer file drag-and-drop. These are UX targets,
not evidence that PeerGlide already provides them. Pairing, monitor arrangement
and clear trust status should be easy to understand. Native drag must receive its
own implementation and acceptance tests; ordinary file sends cannot substitute.

## Dependency policy

PeerGlide source is MIT. Direct Rust dependencies are independently sourced from
Cargo; record their exact resolved versions and SPDX metadata from Cargo.lock /
cargo metadata. Rustls/Quinn/Tokio/Serde/rcgen, BLAKE3, clap and windows-sys do not
inherit licenses from these reference apps. Transitive dependencies, bundled
native code and their notices require separate distribution review. Dependency
metadata is a screening aid, not a substitute for inspecting shipped notices.


## mDNS implementation dependency

`mdns-sd` 0.21.7 (https://github.com/keepsimple1/mdns-sd), licensed Apache-2.0 OR MIT
per locked published Cargo metadata, is linked with default features disabled.
API behavior reviewed at https://docs.rs/mdns-sd/0.21.7/mdns_sd/ and local published
source; no reference GPL implementation was copied. Discovery hints never establish
trust; collector/notice screening includes the dependency. See DISCOVERY.md for
actual bounded hint policy and unresolved daemon-cache/network acceptance limits.


## Oink：全模块参考与首批落地（2026-11-05）

固定参考源码：[oinkvm/oink@3ca82127e61f9d7106a70574a82cc63566d7a572](https://github.com/oinkvm/oink/tree/3ca82127e61f9d7106a70574a82cc63566d7a572)，
workspace版本0.3.2，Rust1.99/edition2024，Tauri前端。PeerGlide保留现有Rust/eframe、
GUI最低1.95、MIT及输入v5。这是全模块入口/关键调用链/异常测试的源码设计参考，
不是对约48万行源码逐行完成安全审计，也没有运行Oink程序、测试或安装其服务/插件。

### 授权与来源

公开[LICENSE](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/LICENSE)、
[附加条款](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/LICENSE-ADDITIONAL-TERMS.md)
和[NOTICE](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/NOTICE)
仍声明GPL-3.0-only及附加条件。用户于2026-11-05确认已获作者对Oink原创代码的
MIT集成和分发许可；本记录区分用户确认的单独授权与仓库公开许可。它不改Oink公开
许可证，也不覆盖第三方材料。将来直接移植具体原创文件时须记录来源/修改/作者归属；
第三方库、Apple音频插件基础代码、MPL/MS-RL及图标等逐项保留原许可。
本批只吸收机制并独立修改PeerGlide已有代码，没有移入Oink源码、注释、品牌资源或依赖。

### 覆盖索引与当前取舍

| 模块/入口 | 已核对的核心机制 | PeerGlide当前取舍 |
|---|---|---|
| foundation | Boot/会话/路径身份、完整快照、原始pending age、唤醒模型 | 保留现有capture epoch/租约，旧输入不得重标为新会话；积压原始年龄列必要回归候选 |
| input | Mac tap/主队列/权限，Win hook/typed服务接口与停止 | 实际输入owner自检优先；保留现有唯一捕获、来源标记、Pending/reaper/Unconfirmed |
| network | TLS签名、SPAKE2、发现、组提交journal与业务admission | 发现仍只是提示；短码机制后续独立设计，不取代v5的显式pin/功能授权 |
| protocol | CBOR帧头先限额、能力协商、错误流poison、未知字段策略 | Oink wire/low v1与PG v5不兼容，不搬协议或ALPN |
| routing | Down冻结目标、Up归原目标、断线改Dropped、接收引用计数 | 对照已有pair/epoch与不确定注入账本；不退回成功后才记Down/先删账再Up |
| agent | 网络/输入/注入owner分离，双向输入流与旧释放barrier完成才attach | 认证、Native准备、控制归属、恢复完成保持分开，不用TLS成功冒充可注入 |
| clipboard | 方向epoch、offer来源/版本、持久token、私有缓存与drop授权 | 只作为后续D路线参考，保留文本/文件独立授权和默认关闭 |
| display | OS bounds与共享plane分离、物理尺寸来源、native baseline/readback | 本版仍两端单主屏，不执行系统布局写入 |
| locator | 光标拖尾/瞄准/投影覆盖层的有界模型与生命周期 | 不是网络发现；覆盖层后置，不新增竞争捕获 |
| winsvc | PID/session/token/受保护映像校验，桌面代次与清理事务 | 不安装SYSTEM/UIAccess/登录前服务，不扩大当前普通用户权限 |
| audio | 实时capture/playback、claim代次、音量控制、静音恢复journal | 音频是独立能力，当前不实现录音/转发或安装HAL插件 |
| autostart | 系统状态为准，显式选择才登记；用户与机器层分开 | 当前不开机自启，不自动登记任何系统项 |
| update | 签名feed/过期与重放拒绝、产品架构验证、安装事务与恢复 | 不借用其信任根/域名/凭据，不启用更新服务或发布稳定版 |
| telemetry | 默认关闭、固定数值schema、持久同意/预算、退出后排队 | 当前不增加遥测或第三方请求，原有私密内容禁记禁传不变 |
| crash | 本机capture与有限字段提取分离、上传边界 | 不增加raw dump收集/自动上传，尤其不把内存故障当权限扩大理由 |
| Tauri/UI | 后台权限投影、配对revision、陈旧结果失效、设备卡事实与意图区分 | 保留中文eframe，不照搬默认共享；实际worker权限已新增独立入口 |
| FileProvider/音频插件 | appex/AppGroup/域启用、loopback capability、原生CRUD/MOVE；HAL构建与权限 | 属额外安装/权限/原生验收，当前只读参考，不实施 |
| tests/build-support/packaging | 入口smoke、内到外签名、来源/许可/升级与故障测试 | 借鉴验证边界，不把源码中存在测试当本轮执行通过 |

### 优先吸收的机制

1. **权限看真实owner。** Oink外层App的主执行文件是backend，UI是嵌套独立bundle，
   UI用LaunchServices进入自己的责任域；权限状态来自后台输入主体。
   [组包](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/packaging/macos/build.py#L114-L133)、
   [启动](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-agent/src/backend/mod.rs#L228-L305)。
   这不是“GUI和helper共用identifier”的实现，也不能直接证明PG的授权传递已修复。
2. **无需求时停止界面工作。** 可见页面决定遥测需求，隐藏时仅保留最新完整快照，恢复只
   重放一次；delta和操作回执不能当可丢快照。
   [live_views](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/apps/oink/src-tauri/src/live_views.rs#L31-L115)、
   [window_activity](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/apps/oink/src-tauri/src/window_activity.rs#L25-L76)。
   本批PG独立实现“只在活动工作时保留100ms应用tick”，不移植WebView代码。
3. **重连是新身份代次，旧释放先完成。** 输入attach需双向stream、开场状态和旧detach释放
   屏障；in-flight保留原始age，heartbeat不能刷新旧工作年龄。
   [input stream](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-agent/src/streams/input.rs#L1-L24)、
   [pending_age](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-foundation/src/pending_age.rs#L1-L128)。
4. **异步界面结果必须可作废。** 地址查询generation、prepared lease、配对revision和请求
   receipt均与当前意图匹配；已提交的取消应给真实Undo结果，不能本地假装撤销。
   [pair model](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/apps/oink/src/pairing/model.ts#L33-L64)、
   [control receipts](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/apps/oink/src/devices/controlActions.ts#L93-L154)。
5. **内容的存储、取消、撤销、应用接收分开。** ClipId源/序号、offer generation与写入计数
   分别处理来源和回环；私有文件Stored只说明本地可读，不证明目标应用已接收。
   [sync](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-clipboard/src/sync.rs#L47-L155)、
   [完成边界](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-clipboard/src/content/linked/completed.rs#L1-L25)。

### 不直接迁入的产品与安全取舍

- Oink的SPAKE2/HKDF/HMAC有真实代码和transcript/角色/attempt绑定，但没有在本轮做
  密码学审计；10秒是协议推进期限，人工等待短码没有同一失效期限。
  [PAKE](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-network/src/group_pake.rs#L53-L124)。
- 组内采用full-trust：admission先限定旧成员，roster并不逐一验证嵌入签名。不能把这些
  签名字段当成独立issuer授权证明，也不把配对成功当输入/剪贴板/文件自动授权。
  [roster策略与测试](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-network/src/roster.rs#L69-L95)。
- 默认输入Shared及screen/clipboard共享、剪贴板send/receive默认true、忽略旧分格式关闭
  字段的迁移，不符合PG默认关闭/保留用户关闭选择的要求。
  [policy](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-clipboard/src/policy.rs#L18-L39)。
- Mac tap有刻意保留的Shared，stop直接join；部分注入路径的退账取舍不同。保留PG的
  不确定Down责任、失败Up账本、SourceLost不盲Up及明确恢复未知状态。
  [tap生命周期](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-input/src/macos/tap.rs#L287-L322)。
- 输入/注入队列有界，不代表所有消息总线有界；普通commands/events与discovery刷新
  存在无界通道。不能据模块名宣称资源已全部受限。
- 持久content token可跨namespace取消继续使用；必须区分本次取消与未来读取撤销。
  所读缓存盘点/保留量不等于跨批次持久硬配额预约账本，不替代PG现有接收预算。
  [取消语义](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-clipboard/src/content/grants/tests.rs#L239-L262)。
- FileProvider的用户启用、原生MOVE/CRUD和音频/登录前服务是额外权限与数据语义，不从
  普通KVM连接隐式开启；升级信任根、报告端点、启动时赞助图片请求也不照搬。
  [隐私声明](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/PRIVACY.md)。

### 测试证据入口与未运行项

已静态核对：PAKE错误码/取消不授信、journal提交/重启、v1帧头限额/poison、未Ready
拒绝业务、旧Boot与held Up归属、释放barrier、原始in-flight age；UI引导保存失败、迟到
结果、控制超时、几何变化取消拖动、native布局baseline/readback；剪贴板来源/版本、撤权、
断流/末块/EOF、持久token、私有缓存pin及FileProvider升级/替换文件保护。
示例入口：[路由生命周期](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-agent/src/threads/input/driver/tests/lifecycle.rs)、
[配对节点](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/crates/oink-agent/tests/pairing_nodes.rs)、
[引导](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/apps/oink/src/onboarding/Onboarding.test.tsx)、
[接收文件](https://github.com/oinkvm/oink/blob/3ca82127e61f9d7106a70574a82cc63566d7a572/apps/file-provider/ReceivedTests.swift)。
这些只证明源码中存在相应回归，不证明该SHA的CI/实机通过。参考项目源码不是PG的
验收数据；PG仍须运行自己的检查与真实Windows↔Mac物理矩阵。
