# Linko / 屏跃官网

中文官网 **0.1.6**。现有 Cloudflare Pages 项目 `linko`：<https://linko-63e.pages.dev/>。
只上传 `website/dist` 白名单静态内容，不上传凭据、身份、target 或个人资料。不需要全局安装依赖或浏览器测试。

## 构建与部署

```sh
cd website
npm ci --ignore-scripts --no-audit --no-fund
SITE_ORIGIN=https://linko-63e.pages.dev npm run build
npm run check
npx --no-install wrangler pages deploy dist --project-name linko --branch main
node verify-deployment.mjs https://linko-63e.pages.dev https://<deployment>.linko-63e.pages.dev
node verify-downloads.mjs
```

构建拒绝符号链接和未知输入，保留既有构建到 ignored `.build-history/`，清单绑定源提交及资源 SHA-256。检查中文、链接、隐私边界、代理精确前缀，并运行6项系统识别与定位回归。HTTP回读和代码测试不替代浏览器验收；当前浏览器未验证。

## 下载与费用边界

用户指定代理为 `https://gh-proxy.org/`，直接在完整 GitHub Release URL 前加此前缀。原链与 SHA-256 保留，页面明确第三方代理；本站不请求或保存安装包，不使用自建下载 Function、R2 或其他存储。原先0.1.4的自建Function和路由已移除，旧 `/download/…` 不再提供下载。代理可用性、速度和后续政策由第三方决定，不保证国内可达；下载后必须核对原包摘要。`verify-downloads.mjs`完整读取两平台代理包并校验大小、摘要。

本站恢复纯静态 Pages；下载直接访问第三方，因而不消耗我们账户的下载 Functions 请求额度。未购买服务或升级账户套餐，也不承诺第三方永久免费。Cloudflare静态与Functions计费区别见[官方说明](https://developers.cloudflare.com/pages/functions/pricing/)。

## 系统识别与定位

只在浏览器本地读取低熵平台提示，缺少时兼容读取平台／UA；不上传、不申请高熵架构信息、不写存储。Windows／macOS对应卡片显示“当前系统”；Mac仍明确仅Apple芯片包，Windows为x64，识别系统不证明CPU兼容。移动／iPad桌面模式／未知环境不推荐桌面包；Linux仅定位诊断说明。

首页不强制跳过介绍，点击“获取预览”或打开 `#preview` 才滚到对应卡片。支持初始加载、重复点击、hash导航、键盘及减少动态效果；所有平台入口保留。

## 公开素材与版本

- `assets/linko-mark.svg`：已确认A跃迁，源 `docs/design/linko-brand-v3/app-icon.svg`。
- `assets/app-preview.png`：8c6ecbb6517b855f57f4d41ffb41092973cfbc25的原生CI隔离NoNative原图，SHA-256 `071f2760a97838a44457d87dbd0fcc03c62c4d71a76afc5e0ffa0f11b38481e9`。
- 候选界面为0.3.20-dev；公开下载仍是0.3.17-dev，来源9797d60。下载元数据在 `src/release.json`，摘要对应原始便携ZIP。
- 真实双机跨屏尚未通过，新V2排版尚未完整落地。功能进度见[状态文档](../docs/STATUS.zh-CN.md)。
