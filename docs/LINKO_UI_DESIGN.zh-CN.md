# Linko / 屏跃 — Desktop UI design proposal (2026-11-07)

**Status:** approved visual direction for Codex exploration; NOT an implemented feature list. Product display name: **Linko / 屏跃**. Preserve existing repository, crate and protocol identifiers PeerGlide until a separately reviewed migration. No domain purchase or trademark clearance is claimed.

## Reference
- Logi Options+: device-first, restrained, functional navigation.
- Linear: hierarchy, typography, compact content, subtle borders.
- Synergy: spatial display topology as the central UX.
- LocalSend: simple discovery/file send flows.
Use ideas, not copied artwork or code.

## Visual system — porcelain white / graphite / muted blue
| Token | Value |
| --- | --- |
| canvas | #F7F8FA |
| surface | #FFFFFF |
| primary text | #20252D |
| secondary text | #64748B |
| brand / focus | #4263EB |
| brand hover | #3651CC |
| brand soft | #E8EEFF |
| border | #E2E7EF |
| success | #10B981 |
| warning | #F59E0B |
| danger | #EF4444 |

~85% neutrals / 10% brand / <=5% semantic accents. Avoid mint/teal background gradients, decorative dashboard charts, ungrounded metrics and multi-color cards. Ensure keyboard focus, reduced motion, contrast WCAG AA, high DPI and light/dark/system settings.

## Information architecture
Sidebar: Devices, Screen Layout, File Transfer, Clipboard, Shortcuts, Settings. Keep existing Gesture Lab, rule editor, trust enrollment and diagnostics accessible; mark unimplemented product entries as unavailable.
- Main page is Screen Layout: prominent drag-to-arrange topology, selected computer border, host/OS/state labels; zoom, auto-arrange, reset, keyboard adjustment.
- Right context panel: sharing settings, connection permissions, diagnostics. Separate consent for clipboard/files and any new window bridge capability.
- Detail drawer: selected peer, connect/disconnect/revoke. Never fake real device or transfer data.
- Active sidebar row uses a slight blue tint, one accent border; panels 12–16 px radius; controls 8–12 px; one-pixel borders; native system typography.

The previously generated render is **concept art**, not a screenshot from the repository. It depicts hypothetical devices, OS versions, connection state, transfer progress and latency. Codex must bind the UI to actual capabilities/state, including loading/empty/error/revoked paths and secure permission prompts.

## Codex execution
Read AGENTS.md, README, DEVELOPMENT_PLAN, VALIDATION and LOCAL_CODEX_HANDOFF before changes. Preserve eframe and current Rust runtime. Start by inventorying existing GUI screens/widgets and state. Implement semantic tokens and topology pure model (snapping, scale, edge ordering, hit testing, overlap, keyboard), deterministic tests and state bindings incrementally. Do not silently enable unimplemented features. Keep physical Windows/macOS acceptance separate from CI, and use small PRs. See WINDOW_BRIDGE_RESEARCH.zh-CN.md.
