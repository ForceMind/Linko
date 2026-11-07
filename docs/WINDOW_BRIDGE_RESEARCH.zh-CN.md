# Linko Window Bridge — 跨电脑应用窗口桥接研究与实施建议

2026-11-07 | **研究/规划**，未实现、未真机验证。与跨屏鼠标和文件拖拽不是同一功能。

## 产品目标与边界
A 电脑应用仍在 A 上运行；B 以系统本地代理窗口显示其图像并在明确授权后转发鼠标、键盘、焦点、缩放等交互。用户可把 A 的窗口从屏幕边缘自然“拖”到 B，再拖回来。**不迁移进程、内存、系统资源或应用安装**。优先做显式“发送窗口到设备”，然后再评估自动边缘拖窗。断链必须自动恢复 A 本地窗口可见性且不影响原应用运行。

## GitHub projects to examine
1. **Transom** https://github.com/aydinmrnv/transom — exceptionally relevant macOS host -> native Windows proxy windows. Its README describes virtual display tiling, ScreenCaptureKit one-stream hardware encode, window rectangle side channel, 1:1 crop, Windows native windows with resize backpropagation. Early access, **currently unauthenticated and unencrypted**: research its architecture, do NOT reuse wire security or ship it as-is. Determine license and real compatibility before code reuse.
2. **Xpra** https://github.com/Xpra-org/xpra — mature seamless remote application model, per-window lifecycle, focus/clipboard/window-manager integration. Seamless servers require supported Linux/Unix environment, not Windows/macOS. Study window protocol, not a drop-in host.
3. **WinApps** https://github.com/winapps-org/winapps — FreeRDP/RemoteApp-based Windows app windows on Linux, shortcut/taskbar integration; requires RDP/VM/server capabilities and is not a universal Mac/Win bridge. Review FreeRDP licensing/API before integration.
4. Official platform APIs: Apple ScreenCaptureKit + Accessibility permission; Windows Graphics Capture + Win32 window APIs. Capture/proxy/input capability must be probed rather than assumed.

## Suggested modular design
- **Window registry:** enumerate eligible user-owned visible windows, ephemeral generation-scoped IDs, window bounds, DPI, ownership, lifecycle; guard against recycling IDs; filter sensitive/system/secure windows.
- **Capture + encode:** OS-specific capture source for selected windows, compression negotiation, timestamping, occlusion/DRM constraints, frame pacing. Evaluate per-window capture vs virtual-display tiling; Transom's external virtual monitor dependency may not be acceptable.
- **Transport:** reuse the project's mutually authenticated QUIC, use separately permissioned video/control channels with congestion and backpressure; separate capability/grant from KVM input, files and clipboard. Never treat input grant as window-sharing authorization.
- **Destination proxy:** native frameless/decorated window as appropriate, resize/move/minimize/close, DPI-aware coordinate mapping, focus/IME, cursor, escape/revoke indicator, local window-manager interoperability. Resize end-to-end ACK; source remains authoritative for application/window lifecycle.
- **Input:** reuse the existing single capture/injection lifecycle; no competing global hook, no log of keyboard/window contents. Validate trusted peer, window token, focus, lease and monotonic order. Secure desktop/UAC, Secure Input and permission denial are hard boundaries.
- **Recoverability:** never permanently hide/move the source window until control+video ready; on disconnect, revoke inputs and restore the original locally visible bounds. Handle compositor failure, app exits, screen changes, lock/unlock and sleep.

## Rollout milestones (additive; do not renumber M0–M5 / G1–G2)
WB0 research & executable feasibility matrix: Mac->Win, Win->Mac, Win->Win, Mac->Mac; each capture API, window enumeration, accessibility, codecs, target proxy, license and fail-safe. Do not claim any combination verified from repository docs alone.
WB1 explicit user-selected window mirror, one direction; dedicated consent, stop button, source always visible; synthetic and CI tests.
WB2 destination proxy window with correct focus, resize, pointer, keyboard/IME, DPI and close semantics; separate window-control authorization.
WB3 one-click handoff, synchronized position/size, source hiding only after ACK, automatic restore.
WB4 border-drag handoff: only after native feasibility; avoid hijacking ordinary window drag, support cancellation, dead zones, multi-monitor and mixed DPI. The existing edge switch is for mouse, not application-window dragging.
WB5 performance/security/physical acceptance: repeated 8-hour sessions, low-jitter LAN measurements, 60fps target where supported, latency budgets measured not invented, error/permission/lock/restart cases, no security bypasses.

## Codex guidance
Research the named repos, pin exact source versions and licenses, list reusable architectural patterns versus prohibited or incompatible implementation, then produce ADR and proof-of-concept behind a disabled-by-default capability flag. No network screenshot or remote window input without user consent. No remote action RPC piggybacking. Keep actual source/client native behavior unverified until measured on real Windows/macOS devices. Do not start Window Bridge before critical C06 physical core acceptance unless work is isolated, safe, testable and not marketed as shipping.
