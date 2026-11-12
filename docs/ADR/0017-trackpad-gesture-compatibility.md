# ADR-0017 跨机触摸板手势的兼容边界

状态为设计草稿。当前候选仍使用 v5 的键鼠与双轴滚轮。macOS 小数滚轮候选和
Windows 注入来源诊断分别记录，缩放、旋转和滑动没有可用的跨机入口。

## 原生来源

macOS AppKit 的放大、旋转和滑动由活动应用的窗口响应链处理。可先在 Linko 自有
窗口里观察类型和阶段。现有全局鼠标捕获能看到的滚轮事件不能证明后台可以采集这些
手势，也不能证明能够用公开 API 注入到任意应用。

Windows Precision Touchpad 对应用窗口提供精细输入。Microsoft 文档的部分 API
仍属预发行范围，Windows 11 接口也不覆盖现有最低系统。需在运行时核对可用性、
原生输入归属和真正的接收目标，再确定可发布的平台组合。

## 输入归属与兼容

后续原生样例必须沿用现有唯一捕获生命周期。回调只保留有界状态，识别、编解码和发送
在工作器处理。仍保留注入来源、焦点与归属检查，失焦、停止、租约退役、撤权或连接
结束使当前手势失效。触摸板能力与 G1 本机鼠标动作分别授权。

能力按双轴滚动、缩放、旋转、滑动分别确认。在两端具备公开 API 的证据前，沿用现有
v5 输入和 ALPN。高级阶段字段若需单独协商，使用认证连接上的明确能力和版本，旧端
保持既有键鼠和滚轮。采用系统原生手势语义，不把高级手势转换成通用按键或脚本动作。

## 证据

先对比两端 `KVM-DIAG/5` 的 `wh_phys`、`wh_inj`、`scr_tx`、`scr_apply` 和实际
滚动。F12 的驱动注入来源仍属待证实；保持现有注入来源过滤。每种高级手势分别核对
物理触摸板、系统版本、目标应用、焦点变化、撤销及恢复。受限接口及新增全局来源按
项目授权另行决定。

- [Apple AppKit 触摸板事件](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/EventOverview/HandlingTouchEvents/HandlingTouchEvents.html)
- [Microsoft Precision Touchpad 输入](https://learn.microsoft.com/en-us/windows/win32/input-precisiontouchpad/precision-touchpad-portal)
