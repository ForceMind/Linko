# Linko / 屏跃官网

中文官网，站点版本 **0.1.5**。Cloudflare Pages 项目 `linko`，正式访问地址：
<https://linko-63e.pages.dev/>。

官网独立于原生 App，不改变 App 的版本、设备身份、权限或协议。仅发布
`website/dist`，不能从仓库根目录执行上传，也不能上传本机 profile 或 `target/`。
不需要访问者账号、分析脚本或远程字体。仅下载路由使用 Pages Function，其余为静态文件。

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

- `assets/linko-mark.svg`：已确认 A 跃迁标识，来源于 `docs/design/linko-brand-v3/app-icon.svg`。
- `assets/app-preview.png`：代码 `8c6ecbb6517b855f57f4d41ffb41092973cfbc25`
  的原生 GUI CI 原图，1280×800，隔离 NoNative 通信场景；未启用原生键鼠。
  SHA-256：`071f2760a97838a44457d87dbd0fcc03c62c4d71a76afc5e0ffa0f11b38481e9`。
  图内已实现页面不代表真实 Win/macOS 双机验收完成。
- `release.json` 指向公开的 GitHub Pre-release 便携包，免登录下载。
  SHA-256 对应实际便携包 ZIP，不是 CI artifact 外层 ZIP。
- 0.3.17-dev 程序与包来自受测提交 9797d60，直接使用核验后的 CI 原包；来源及摘要见对应 Release。
- 原生包不托管到 Pages，当前仍是开发预览，未发布稳定版。

截图为 0.3.20-dev 候选，与公开下载 0.3.17-dev 分别标注；候选尚未发布，也未完成真实双机验收。

## 国内网络备用下载

官网提供 GitHub 原链与同域中转两个入口：

- `https://linko-63e.pages.dev/download/0.3.17-dev/macos`
- `https://linko-63e.pages.dev/download/0.3.17-dev/windows`

使用 [Pages Advanced mode](https://developers.cloudflare.com/pages/functions/advanced-mode/)，构建将公开 `release.json` 注入 `_worker.js`，`_routes.json` 仅匹配 `/download/*`。仅转发固定版本的两个公开 ZIP，不接受任意 URL、查询参数、上传或访问者凭据。手动检查 GitHub 重定向目标，限制至官方资产域；核对公开包大小和类型、限制重定向次数与 5 分钟传输期限，流式转发避免整包驻留内存，失败不缓存。用户仍需核对公开 SHA-256，响应头声明不能代替完整内容校验。

不引入第三方镜像、不修改原包、不购买服务或启用付费计划。Cloudflare 中转不保证所有国内网络可达或提速；当前不支持断点续传，失败可重新下载或使用 GitHub 原链。该路由使用现有账户 Pages Functions 配额，耗尽时备用入口可能不可用，静态页面仍走原有服务。

构建后的 `npm run check` 包含 8 项路由、凭据隔离、重定向、源失败和元数据回归。部署后运行 `node verify-downloads.mjs https://linko-63e.pages.dev`，实际流式读取两个完整包并校验大小与 SHA-256；这是下载验证，不是国内网络实测或 App 双机验收。`_worker.js` 仅服务端执行，不作为公开静态资源读取。

## 按访问系统定位下载

只在浏览器本地读取低熵平台提示，缺少时兼容读取平台／UA；不上传识别结果、不申请高熵架构信息、不写入存储。Windows／macOS 对应卡片显示“当前系统”；macOS 明确仅 Apple 芯片包，Windows 明确 x64，系统识别不证明 CPU 架构兼容。iOS、iPad 桌面模式、Android、ChromeOS、未知环境不推荐桌面包；Linux 只定位诊断说明。

首页不强制跳过产品介绍；点击“获取预览”或打开 `#preview` 时定位对应卡片。初次等待页面资源加载，重复点击和前进／后退也适用；减少动态效果时不平滑滚动，键盘触发保留焦点可达性，其他平台始终可见。6项平台／定位代码回归与既有8项中转回归通过；浏览器未验证。

计费边界：Pages Functions 按 Workers 套餐计费。Free 计划每天全账户共享100,000次请求，超限会失败；Paid 计划按套餐及超额请求／CPU计费。官方目前不另外收取 Workers 出站流量费。实现未购买或升级计划，未启用R2等存储；未核实账户账单前不能保证增量费用恒为零。规则来源：[Pages Functions pricing](https://developers.cloudflare.com/pages/functions/pricing/)、[Workers pricing](https://developers.cloudflare.com/workers/platform/pricing/)、[Workers limits](https://developers.cloudflare.com/workers/platform/limits/)。
