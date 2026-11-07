# Linko / 屏跃：拓扑首页设计

用户已确认按[PR69 / bfd739c](https://github.com/ForceMind/PeerGlide/blob/bfd739c9a06f11965a9720ff67b6a6fd4b12a625/docs/LINKO_UI_DESIGN.zh-CN.md)重新设计。当前采用瓷白、石墨与少量品牌蓝；modern-v1的01–04稿保留为历史，不再作为实施依据。

- [拓扑首页效果图](effects/screen-layout.png)：AI视觉提案，设备和状态为示例，不是运行截图。
- [应用图标SVG](brand/app-icon.svg) / [512px预览](brand/previews/app-icon.svg.png)：原创双屏连接标记，不迁移仓库、包名或协议标识。
- [拓扑首页线稿](wireframes/01-screen-layout.svg) / [完整预览](wireframes/previews/01-screen-layout.svg.png)。
- [设备与详情线稿](wireframes/02-devices.svg) / [完整预览](wireframes/previews/02-devices.svg.png)。
- [添加电脑线稿](wireframes/03-add-computer.svg) / [完整预览](wireframes/previews/03-add-computer.svg.png)。
- [交互与安全状态映射](wireframes/INTERACTION.zh-CN.md)。

## 当前迭代

默认进入“屏幕布局”，中央为两台主屏的相对位置，右侧为所选设备状态、权限和一个主操作。导航采用“设备、屏幕布局、文件传输、剪贴板、快捷键、设置”，手势实验室、规则编辑和原总览从快捷键页继续可达。停止与恢复保留固定入口。

视图缩放60–100%，适配恢复100%，不写配置或改变授权。方向按钮与拖动使用同一个安全门与存储路径；会话、配对、只读、恢复未知或登记待回执时不改布局。配置冲突拒写后恢复原方向并显示错误。

当前只做本机与一台对端、各一个主显示器。独立自动排列、平移、多屏像素布局、远程视频工作空间、移动控制器与窗口桥不在本批交付范围。文件、系统剪贴板和全局手势仍明确标为尚未接通；不能用设计图代替实现或物理验收。

## 色彩与组件

| 用途 | 规范色值 |
|---|---|
| 画布 / 卡片 | `#F7F8FA` / `#FFFFFF` |
| 主文字 / 辅助文字 | `#20252D` / `#5F6F85` |
| 主操作 / 悬停 / 选中浅底 | `#4263EB` / `#3651CC` / `#E8EEFF` |
| 装饰边界 | `#E2E7EF`，控件焦点另有可见强调边线 |

辅助文字从源提案的`#64748B`略加深，满足浅灰底普通小字对比要求。语义状态同时用文字和符号，不用颜色代表授权。保留已有中文字体许可与深色主题；不安装系统字体。卡片圆角12–16px、控件8–12px、8px间距节奏；技术字段在按需详情中呈现。

效果图使用内置image_gen，[原始提示词](prompts.json)与[资产尺寸/SHA256](assets.json)随稿保存。线稿与图标为可编辑SVG，已检查XML/外链，并分别通过AppKit/Quick Look离屏静态渲染查看。浏览器未验证。这些静态检查不代表实际GUI、系统权限或双机输入验收；实现与最终候选证据见项目验证记录。
