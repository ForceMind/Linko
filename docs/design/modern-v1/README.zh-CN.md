# 屏跃现代化视觉提案

配色评审：01蓝白、02墨绿/青柠与03石墨/雾白均已被用户否定，不能作为GUI实施依据。[02绿色稿](effects/device-hub-forest-rejected.png)与[03石墨灰稿](effects/device-hub-graphite-rejected.png)仅保留迭代记录；下一稿采用动漫樱粉/雾紫方向，待用户评审，尚未定稿。

按用户要求先效果图，再线稿，视觉方向确定后才按设计开发。当前属于设计提案，不是运行截图，也不代表原生功能验收。应用版本保持0.3.13-dev。

参考[UU远程官网](https://uuyc.163.com/)公开的产品视觉信息，提出屏跃自己的蓝白设备中心：设备卡片优先、单一主操作、布局独立页签、设备详情按需展开。没有复制其Logo、UI素材或账号/会员/远程终端业务。

- [设备主页效果图](effects/device-hub.png)
- [添加电脑效果图](effects/add-computer.png)
- [图标概念（透明PNG）](brand/app-icon-concept.png)
- [可编辑应用图标](brand/app-icon.svg) / [图标预览](brand/previews/app-icon.svg.png) / [菜单栏单色标记](brand/tray-mark.svg)
- [设备主页线稿](wireframes/01-device-hub.svg) / [完整预览](wireframes/previews/01-device-hub.svg.png)
- [屏幕布局线稿](wireframes/02-screen-layout.svg) / [完整预览](wireframes/previews/02-screen-layout.svg.png)
- [添加电脑线稿](wireframes/03-add-computer.svg) / [完整预览](wireframes/previews/03-add-computer.svg.png)
- [视觉与交互规格](SPEC.zh-CN.md) / [状态与操作映射](wireframes/INTERACTION.zh-CN.md)

图标方向为“双屏滑移”，蓝青底色与白色跨屏连接符号。已将PNG概念重建为SVG几何提案，避免直接使用生成图的边缘细节。生产接入前仍需检查16/24/32像素、深浅背景和平台图标包；当前没有替换应用图标。

视觉参考UU的蓝白层次，交互吸收[Oink固定参考版本](https://github.com/oinkvm/oink/tree/3ca82127e61f9d7106a70574a82cc63566d7a572)的设备选择、屏幕排列、就地能力状态与渐进展开。具体证据及屏跃的取舍见规格。当前线稿优先用户的Windows+macOS、各一个主显示器场景，不扩大到多屏或音频开发。

效果图中的设备、平台、权限和短码均为设计示例。运行时必须来自真实后端；不知道的状态显示未知，短码每次生成。不得把示例数字或视觉上的绿色状态当作认证。输入、数据与动作仍独立授权；未接通的G1/数据能力继续标明。

效果图和图标概念使用内置image_gen工具，原始提示词见prompts.json；线稿与几何图标为可编辑SVG。文件尺寸、来源与摘要见assets.json。SVG经过XML/外链检查及macOS AppKit离屏静态渲染检查；PNG预览不是浏览器截图或实际GUI截图。没有修改GUI代码、身份、配置、系统权限或许可证。浏览器未验证。
