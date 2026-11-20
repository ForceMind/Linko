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

## 构建版本号

每次构建的版本由所构建提交的提交时间生成，固定换算为北京时间（UTC+08:00），同一提交在任何平台得到相同版本，由 `tools/build_version.py` 计算，CI 在首次 cargo 构建前写入 `LINKO_VERSION`。

- 显示版本与安装包名：`YYYY.M.D-HHMM`（月日不补零，时分补零），例如 `Linko-2026.11.10-1749-darwin-arm64.zip`。
- macOS `CFBundleShortVersionString` 为 `YYYY.M.D`；`CFBundleVersion` 为 `YYYYMMDD.H*100+M`，不带前导零，例如 `20261110.1749`、09:05 为 `20261110.905`。
- `tools/package.py` 优先使用 `LINKO_VERSION`（格式不符即报错），未设置时由 git 计算，git 不可用时回退到 `Cargo.toml` 的 workspace version（该值不随提交修改）。
- 发布标签仍按上文 `vX.Y.Z-dev` 规则，与构建版本号相互独立。

## 包内来源

ZIP 根目录的 `BUILD_SOURCE.json` 记录版本、平台、架构和实际
Git 提交及源码树。PR 工作流同时记录受审提交，GitHub 合成提交
可与受审提交有不同编号。生成验收包时核对各平台的源码树与
提交来源。来源字段和 SHA-256 需从可信 CI 或发布记录核对，
本地 ad-hoc 校验和 ZIP 来源文件不提供发布者身份。

## 便携版本选择与回滚

`tools/portable_update.py` 使用用户指定且已存在的独立版本目录。
`stage` 需要 ZIP、从可信 CI 或发布记录取得的 SHA-256 及预期
来源 JSON。成功时输出本次暂存编号和收据摘要。`activate` 用这两个
摘要复查已暂存内容后选择版本。`current` 复查并显示入口，
`rollback` 复查前一版后恢复选择。工具输出入口，实际启动由
用户操作。更新目录之外的应用配置、已配对身份和授权由应用继续管理。

```text
python tools/portable_update.py --versions PATH stage --archive ZIP --archive-sha256 SHA --expected-source SOURCE_JSON
python tools/portable_update.py --versions PATH activate --label LABEL --archive-sha256 SHA --receipt-sha256 RECEIPT_SHA
python tools/portable_update.py --versions PATH current
python tools/portable_update.py --versions PATH rollback
```

版本目录保留旧包内容。切换记录以同目录替换，回读时验证收据和
文件内容。系统级安装、后台服务、代码签名、公证及真实更新后的
系统权限继续分别授权和验收。

同仓库 PR 的 GitHub Actions 在临时目录暂存并选择当轮成品，运行同包
worker 的版本入口。找到同分支或基线分支中来源匹配、尚未过期的上一轮
通过产物时，作业还会验证损坏版本被拒绝及恢复旧版入口。缺少上一版时
记录跳过回滚，暂存与选择仍须通过。

## 首个预览的来源

0.3.16-dev 程序来自 `cb27bce72ece205f138180fd1b6ef0c8cbf0764f`、CI `42382394527`。首个公开包仅替换 ZIP 根目录的 README／SECURITY，程序和 App 内成员保持原样。标签的 Rust 源码须与该基线一致。新 ZIP 有独立摘要，不能混用原始 CI artifact 外层摘要。
