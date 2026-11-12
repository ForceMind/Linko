# ADR-0015 跨机拖放的复制投递

日期 2026-11-12。状态为设计草稿。桌面文件页拖入本机文件后可明确发送。
Finder、Explorer 与其他应用之间的跨机原生投递仍待实现和验收。

## 本机意图和会话

源端由用户发起实际拖动，已认证的目标电脑需要独立同意此次拖放。
文本、图片和文件沿用对应内容类型的方向许可，另需本机确认投递。
连接或键鼠控制成功不能授予这项许可。既有 v5 报文及 ALPN 保留，
文件和图片使用独立数据连接，接收方只采用已通过边界与内容校验的数据。

每次投递绑定精确证书、当次连接、传输编号、活动的本机拖放代次和
受控的输入归属。失焦、按钮状态失配、取消、连接结束、布局变化或
撤销均关闭旧票据。只接受同一票据的完成回执，异常或结果未知时
提示检查目标应用，禁止自动再投。原生输入只使用现有捕获生命周期。
拖动期间的非复制行为、失效票据或未完成验证不向目标应用提供数据。

## 文件暂存与原生交付

文件先进入用户明确选择的接收范围，沿用有界清单、已验证分块、
持久配额和不覆盖发布。原生交付从已验证的本机副本产生，目标位置由
接收端系统和用户的放置动作决定。远端没有指定目标路径、打开文件、
移动或删除源文件的权限。目标拒绝时保留已暂存内容和已知状态。

Windows 的第一条候选路径采用现有 GUI 线程的 OLE 拖动会话，只允许
复制效果。目标能处理本地文件时提供已准备的本机文件数据，虚拟文件
需要同时支持文件描述和内容流。跨进程数据对象的读取放在受控工作器，
主输入回调只做有界状态记录。系统拒绝、焦点变化或用户放开按钮时
终止旧会话并走已有释放路径。

macOS 的第一条候选路径使用 AppKit 拖动会话和 NSFilePromiseProvider。
提供者只登记已准备的内容和名称，写入由其操作队列交给本机存储工作器，
收到目标提供的 URL 后再验证本机票据和当前目标。普通文件 URL 和文件承诺
各有目标应用兼容性。与共享容器、File Provider 扩展或签名有关的附加
方案需要独立记录并获授权后才能启用。

## 验证

GitHub Actions 先覆盖本地拖动票据、拒绝、断线、代次变化、复制效果、
完整性和权限无效化。Windows 和 macOS 分别编译和测试各自适配器。
最终同源候选由用户在 Finder、Explorer 及实际目标应用中验收跨屏
文字、PNG、文件、取消、慢传输、失焦、源文件变化和本机输入恢复。

## 参考

- [Microsoft Shell 剪贴板与拖放格式](https://learn.microsoft.com/en-us/windows/win32/shell/clipboard)
- [Microsoft OLE DoDragDrop](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-dodragdrop)
- [Apple NSFilePromiseProvider](https://developer.apple.com/documentation/appkit/nsfilepromiseprovider)
- [Apple NSFilePromiseProviderDelegate](https://developer.apple.com/documentation/appkit/nsfilepromiseproviderdelegate)
