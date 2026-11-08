# Linko 原生应用图标

图形直接来自已确认的 `docs/design/linko-v1/brand/app-icon.svg`，没有重新设计或引入第三方素材。
`icon-source.json` 记录源文件与产物的摘要。

- `app-icon.png`：AppKit 从 SVG 渲染的 1024px RGBA 主图。
- `app-icon-256.png`：eframe 0.37 `ViewportBuilder::with_icon` 的运行窗口图标；不代替包图标。
- `Linko.icns`：macOS 包的 `Contents/Resources/Linko.icns`，由 `CFBundleIconFile` 引用；完整签名前复制并纳入严格资源 seal。
- `Linko.ico` / `linko.rc`：Windows MSVC 构建经现有 Windows SDK `rc.exe` 编译后链接进 GUI EXE，打包改名 `Linko.exe` 时资源仍保留。CLI worker 不变。

在已有 AppKit/Swift 与 sips 的 macOS 环境执行 `python3 tools/generate_app_icons.py`。
生成器拒绝覆盖已有产物；仅在明确更新图标的独立目录中重建，审核后替换。
PNG-backed ICNS/ICO 使用 Python 标准库封装。初次 `iconutil -c icns` 在隔离环境拒绝 iconset，因此不依赖该转换服务。
生成后的格式、尺寸、来源摘要与 Mac 包资源由 `tools/test_package.py` 校验。

没有新增 Cargo 依赖或改锁文件。Windows SDK 工具不在 PATH 时，构建脚本搜索现有 Windows Kits 10；缺失则明确失败，不悄悄发布默认图标。
非 MSVC Windows 构建仅接入运行窗口图标，PE 资源嵌入需另行验证。macOS Finder/Dock 与 Windows Explorer/任务栏的实际显示仍须用新 CI 包确认；不操作图标缓存、TCC、身份或当前冻结 App。
