# 发布规范

沿用 workspace 版本。开发预览使用 `vX.Y.Z-dev` 标签并标记 Pre-release。稳定版需独立批准和原生验收；CI 通过不等于稳定。已发布标签和资源不覆盖，修复使用新版本。

## 检查与发布

1. 固定提交、平台、协议和限制，工作区干净。
2. fmt、严格 Clippy、locked test/build、最低 Rust、审计和相关进程检查通过。
3. 检查完整包、版本、SHA-256、入口、许可证和依赖／字体声明。
4. macOS 验证嵌套签名、密封与篡改拒绝；Windows 检查架构及同包 worker。
5. 实机、DPI、权限、睡眠与恢复结果独立记录，未验收不得写成已支持。
6. 创建绑定精确提交的草稿，上传平台 ZIP 与同名 `.sha256`；说明更新、用法、安全、来源和限制，核验后发布为 Pre-release，不设为稳定最新版。
7. 无认证 HTTP 下载全部资源并核对 SHA-256；官网改为 `/releases/download/<tag>/<file>`，摘要对应便携包 ZIP，再构建、部署和回读。

不上传私人路径、配置、身份或秘密，不自动申请系统权限、开启控制或部署后台服务。

## Windows 发布者签名

下一版公开 Windows 包的发布门槛包括两份 EXE 的可信 Authenticode 签名及 RFC 3161 时间戳。普通 CI 便携包用于开发验证。已发布的 0.3.22-dev 是未签名诊断包。

在有既有受信任代码签名证书的授权 Windows 环境中，使用 Windows SDK 的 SignTool。按受测源码构建后运行：

```powershell
python tools/package.py --windows-signing-thumbprint <证书指纹> --windows-timestamp-url <HTTPS时间戳服务> --signtool <SignTool完整路径>
```

证书从当前用户的 My 证书存储按指纹选取。指纹可公开，私钥由原证书设施管理。打包只签名临时目录中的 `Linko.exe` 和 `peerglide.exe`。两份均通过 `signtool verify /pa /all /tw` 后才生成 ZIP 和新摘要。发布前另在干净的 Windows 上解压复验两份签名、发布者显示、同包 worker 和实际启动。记录签名后的包摘要及受测源码。SmartScreen 信誉提示另行验收。

## macOS 更新时的权限连续性

正式分发版须使用同一 Apple 团队的 Developer ID Application 签名，为 App 和嵌套 worker 保持稳定标识。公证与实际启动另行验证。现有 ad-hoc 预览的代码要求绑定版本，迁移到新的分发身份时可能需要一次重新登记。

在更新前，保留原 App 并准备完整的新包。分别核对 App 和 worker 是否满足上一个分发版的 designated requirement：

```sh
python3 tools/verify_macos_update_identity.py /path/to/previous/Linko.app /path/to/candidate/Linko.app
```

两个不同构建须通过身份核验。再用同一个测试用户对旧分发版授予辅助功能与输入监控，正常退出并替换完整包，确认新版实际 worker 沿用两项权限且无需再次授权。身份核验通过只证明代码身份兼容，实机检查通过后才可承诺权限连续性。禁止更改 TCC 数据库或用宽松代码要求伪造同一身份。

## 首个预览的来源

0.3.16-dev 程序来自 `cb27bce72ece205f138180fd1b6ef0c8cbf0764f`、CI `42382394527`。首个公开包仅替换 ZIP 根目录的 README／SECURITY，程序和 App 内成员保持原样。标签的 Rust 源码须与该基线一致。新 ZIP 有独立摘要，不能混用原始 CI artifact 外层摘要。
