# 贡献指南

欢迎参与 Linko。阅读 README、架构、验证记录和相关 ADR，保持小范围增量，避免无关重构。

## 开发与检查

GUI 最低 Rust 1.95，使用 rustfmt、Clippy 和锁定依赖。macOS 使用 Xcode Command Line Tools；Windows 使用 MSVC 与 Windows SDK。Linux 目前仅支持通信和 GUI 诊断，运行需要 X11/XKB、EGL/OpenGL 或相应 Wayland 依赖，具体参考 CI。

```sh
python tools/local_preflight.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
```

输入、恢复和持久化缺陷需要回归测试。原生变化还需相关进程、停止／重连及平台验证。实机、DPI、权限、睡眠验收与自动测试分别记录；缺少工具时写明未运行，不关闭系统防护。官网使用 Node.js 24 LTS，命令见 [website](website/README.md)。视觉与交互验收单独注明。

## 工程边界

- 保持唯一输入捕获生命周期；原生回调不做识别、重分配、磁盘或网络 I/O。
- 保留逃生键、watchdog、输入归属、注入标记与恢复；按下／抬起必须配对。
- 焦点、目标、配置或权限变化使旧动作／票据失效；测试令牌不能充当生产认证。
- 输入许可不包含数据或动作许可；协议 v5 变更需 ADR、兼容策略和两端迁移。
- 不覆盖身份、用户文件、无效或未来配置，不记录真实输入或秘密。

## 提交与许可

PR 说明问题、最终行为、关联 Issue、验证结果和未验收项。用户可见变化更新 CHANGELOG，协议变化更新 ADR。漏洞按 [安全政策](SECURITY.md) 私密报告。

贡献须具备明确可分发的权利，与 MIT 分发兼容；保留必要的版权、依赖和字体声明。不确定许可时先说明，不直接合入。
