# Linko / 屏跃官网

中文静态官网，站点版本 **0.1.1**。Cloudflare Pages 项目 `linko`，正式访问地址：
<https://linko-63e.pages.dev/>。

官网独立于原生 App，不改变 App 的版本、设备身份、权限或协议。仅发布
`website/dist`，不能从仓库根目录执行上传，也不能上传本机 profile 或 `target/`。
无需运行服务端、访问者账号、分析脚本、远程字体或第三方 CDN。

## 构建与检查

```sh
cd website
npm ci --ignore-scripts --no-audit --no-fund
SITE_ORIGIN=https://linko-63e.pages.dev npm run build
npm run check
node --check src/main.js
```

Node.js 24 LTS 用于网站 CI。Wrangler 固定在项目依赖中，不需要全局安装。
构建仅接收 `src/` 白名单文件，拒绝符号链接、未知文件与超出 Pages
25 MiB 限制的单个文件；输出带源提交与逐文件 SHA-256 的
`asset-manifest.json`。已有本工具输出保留在 ignored `.build-history/`；
未知来源的既有 `dist/` 不会被覆盖。

## 部署

使用账户现有授权，在构建检查通过后上传：

```sh
npx --no-install wrangler pages deploy dist --project-name linko --branch main
```

首次创建已完成，后续不需要 `--force`。不要提交 Cloudflare 凭据或将其写入网页。
此项目为 Direct Upload；仓库 CI 仅检查，不持有部署密钥或自动发布。
部署后应独立 HTTP 回读生产域名和部署域名，核对资源摘要、404 与安全响应头。
可运行 `node verify-deployment.mjs https://linko-63e.pages.dev https://<deployment>.linko-63e.pages.dev`，
本地证据写入 ignored `deployment.local.json`。该命令不启动浏览器。
浏览器视觉和交互测试须另行取得用户授权，不能用构建或 HTTP 结果代替。

## 公开素材与下载来源

- `assets/linko-mark.svg`：本项目原创双屏标识，来源于 `docs/design/linko-v1/brand/app-icon.svg`。
- `assets/app-preview.png`：代码 `cb27bce72ece205f138180fd1b6ef0c8cbf0764f`
  的原生 GUI CI 原图，1280×800，隔离 NoNative 通信场景；未启用原生键鼠。
  SHA-256：`ab7630f0eb3621672c8803607e56b3625a0c30ddab77582fcdd8ea7c33f89ca5`。
  图内已实现页面不代表真实 Win/macOS 双机验收完成。
- `release.json` 指向公开的 GitHub Pre-release 便携包，免登录下载。
  SHA-256 对应实际便携包 ZIP，不是 CI artifact 外层 ZIP。
- 0.3.16-dev 程序来自 cb27bce；公开包只替换包外 README／SECURITY，程序和签名不变。
- 原生包不托管到 Pages，当前仍是开发预览，未发布稳定版。
