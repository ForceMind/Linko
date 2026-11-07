# 屏跃现代化视觉提案

本目录为历史配色探索。用户现已确认采用[Linko瓷白/石墨/克制蓝拓扑方案](../linko-v1/README.zh-CN.md)，04“樱花汽水”被新方向取代；01蓝白、02墨绿/青柠与03石墨/雾白均已被用户否定。01–04不再作为GUI实施依据。

按用户要求先效果图，再线稿，视觉方向确定后才按设计开发。当前属于设计提案，不是运行截图，也不代表原生功能验收。应用版本保持0.3.13-dev。

设备卡片优先、单一主操作、布局独立页签、设备详情按需展开。04当前包含首页效果图和图标配色稿；三张线稿是独立于配色的结构稿。添加电脑彩色效果图仍是01历史稿，尚未按04重绘。没有复制参考项目Logo、UI素材或账号/会员/远程终端业务。

- [04樱花汽水效果图](effects/device-hub-sakura.png)
- [04可编辑图标](brand/app-icon-sakura.svg) / [图标预览](brand/previews/app-icon-sakura.svg.png) / [菜单栏单色标记](brand/tray-mark.svg)
- [设备主页线稿](wireframes/01-device-hub.svg) / [完整预览](wireframes/previews/01-device-hub.svg.png)
- [屏幕布局线稿](wireframes/02-screen-layout.svg) / [完整预览](wireframes/previews/02-screen-layout.svg.png)
- [添加电脑线稿](wireframes/03-add-computer.svg) / [完整预览](wireframes/previews/03-add-computer.svg.png)
- [视觉与交互规格](SPEC.zh-CN.md) / [状态与操作映射](wireframes/INTERACTION.zh-CN.md)

图标方向为“双屏滑移”，04使用雾紫底座、暖白屏幕轮廓与樱粉连接曲线。生产接入前仍需检查16/24/32像素、深浅背景和平台图标包；当前只检查了512像素静态预览，没有替换应用图标。

交互吸收[Oink固定参考版本](https://github.com/oinkvm/oink/tree/3ca82127e61f9d7106a70574a82cc63566d7a572)的设备选择、屏幕排列、就地能力状态与渐进展开。具体证据及屏跃的取舍见规格。当前线稿优先用户的Windows+macOS、各一个主显示器场景，不扩大到多屏或音频开发。

效果图中的设备、平台、权限和短码均为设计示例。运行时必须来自真实后端；不知道的状态显示未知，短码每次生成。不得把示例数字或视觉上的绿色状态当作认证。输入、数据与动作仍独立授权；未接通的G1/数据能力继续标明。

效果图使用内置image_gen工具，04原始提示词见[prompts-sakura.json](prompts-sakura.json)；线稿与几何图标为可编辑SVG。文件尺寸、来源与摘要见assets.json。SVG经过XML/外链检查，线稿通过AppKit、图标通过Quick Look离屏静态渲染检查；PNG预览不是浏览器截图或实际GUI截图。04规范色值的主要文本组合对比度为4.99–12.25:1，不代表AI图每个像素均符合规范。没有修改GUI代码、身份、配置、系统权限或许可证。浏览器未验证。

历史提案保留：[01设备主页](effects/device-hub.png)、[01添加电脑](effects/add-computer.png)、[01图标](brand/app-icon-concept.png)、[02绿色稿](effects/device-hub-forest-rejected.png)、[03石墨灰稿](effects/device-hub-graphite-rejected.png)。对应原始提示词保存在prompts.json及两份prompts-*-rejected.json。
