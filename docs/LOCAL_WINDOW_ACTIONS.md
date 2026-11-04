# G1 local window action Backend

This is a native library execution path on top of the existing action Coordinator
and native context bindings. No shipped GUI/CLI starts or dispatches G1 actions
yet; global gestures stay disabled. Input v5, native capture, input/data grants and
MIT remain unchanged. There is no remote RPC, script/command parameter or key injection.

## Implemented semantics

| Platform | Native adapter | Explicitly unsupported |
|---|---|---|
| Windows | MinimizeWindow / MaximizeWindow via ShowWindowAsync; CloseWindow via WM_CLOSE | CloseTab, QuitApplication, ToggleFullscreen, keyboard actions, ShowPeerGlide |
| macOS | MinimizeWindow via public AXMinimized=true; CloseWindow via frozen window's AXCloseButton/AXPress | MaximizeWindow, ToggleFullscreen, CloseTab, QuitApplication, keyboard actions, ShowPeerGlide |
| Linux | Empty capabilities | All native actions |

A supported adapter does not guarantee a particular app/window supports the
operation. Styles/settable/button/target checks can reject before submission. Disabled or
unknown SC_CLOSE/AXEnabled controls reject; a request never bypasses a greyed
close control.
CloseWindow is not an alias for CloseTab or QuitApplication. macOS Zoom is not
Maximize, and an undocumented AXFullScreen string is not used. Actual native
positive behavior and compatibility remain separate validation gates.

## Independent consent at the original trigger

LocalActionConsent is a fresh OS-random process-local identity plus one checked
atomic consent revision. It starts disabled, is not deserialized/persisted, and
cannot be obtained from KVM input grants, a rule enabled flag or OS access status.
Only an explicit trusted local allow call can enable it; destructive confirmation
must already exist before the gesture starts. Replacing consent, even with equal
flags, invalidates older stamps. Revocation is terminal for that instance. Counter
exhaustion fails closed. A fresh worker requires fresh consent.

ActionTriggerSource fixes consent BEFORE obtaining the original context mark,
then confirms consent has not changed. Native callbacks only use fixed-size
atomic/clock data; no random generation, OS target queries, recognition or scripts
run there. Emergency revoke/invalidate is bounded. Enabling or confirming later
never revives a gesture queued under a different consent revision.

## Native submission and resource/result contract

LocalActions freezes a real bound context, rechecks consent, and creates a
PreparedLocalAction. The native Backend can only be constructed internally from
that object. Coordinator preflight preserves schema/local-only/capability/risk/
deadline/dedup rules. Native submit consumes the context once, polls its same
native owner and rechecks the target, observer state, permission, configuration,
consent and the ORIGINAL action/context deadlines immediately before the OS effect.
No query/queue step resets a deadline. Unused/rejected pending contexts cancel on
adapter drop; owned leases retain their shared resource slot until dropped.

Every possible OS effect is preceded by the late guard after native preflight.
Windows never changes foreground, attaches input threads or terminates processes.
macOS verifies the close button's AX type, PID and owning AXWindow; CF globals are
borrowed, Copy results are owned, and parameters are prepared before the guard.
No synthetic key/button press occurs, so there is no invented owned-key recovery
claim. Keyboard actions still require the future owned-injection ledger.

OS success means Submitted only, not proof that an app minimized/closed. AX writes/
Press returning CannotComplete may already have taken effect or entered a modal
operation, so they are UnknownOutcome and never auto-retried. One Backend attempt
is terminal; Coordinator's existing bounded result history refuses conflicts and
returns retained outcomes without replay. Request IDs identify deduplication only,
not authentication or cross-process exactly-once execution.

Native notifications are asynchronous. Late recheck is not an atomic check-and-
submit lock; C06, actual notification delivery, permission revoke, app behavior and
resource/latency acceptance remain required. No action is enabled by diagnostics,
and no host-native effect is part of ordinary CI smoke.

## Current validation scope / next integration

Pure injected-driver/window-operation tests check default/changed/revoked consent,
trigger races, stale/remote/unsupported/destructive denial, late expiry, one native
attempt, error/unknown results and Coordinator duplicate no-replay. Native compile/
link is checked on Windows/macOS, including standalone CLI feature boundaries.
Tests do not use fake HWNDs/PIDs as production authority or physical acceptance.

Still required: G03 shared capture path, balanced ordinary right-click recovery,
owned keyboard action injection, local worker and explicit Chinese enable/pause/
confirmation UI, physical acceptance and independent security review. No user
window should be manipulated for validation without a specifically authorized
isolated test target; no OS permission should be changed automatically.

Primary sources:

- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showwindowasync
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postmessagew
- Apple SDK AXAttributeConstants.h / AXActionConstants.h / AXUIElement.h and CFNumber.h
- https://developer.apple.com/documentation/applicationservices/1462091-axuielementperformaction


Runtime candidate 86b4e71 passed complete five-job run41826266699, including
standalone CLI dependency features, both native platforms, logical fake effects
and unchanged process/GUI baseline. No actual user/native window effect was run.
Final exact-head and remaining acceptance are in VALIDATION.md and PR #37.
