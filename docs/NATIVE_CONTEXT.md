# G1 native foreground/focus observations

`peerglide-platform::context` reads the real foreground application and focused
window/control on Windows/macOS, without installing capture, injecting input,
requesting OS permission, changing focus or opening a network endpoint. It is a
provider for the future shared G1 worker, not an action Backend or execution ticket.

## Local rule identifiers

- macOS: `mac-bundle:<bundleIdentifier>`, original ASCII spelling/case. No fallback
  to a window title, localized display name, PID or executable basename.
- Windows: `win-exe:<QueryFullProcessImageNameW(flags=0) full Win32 path>`, preserving
  original case and lossless Unicode. No arbitrary lowercasing, basename fallback,
  silent truncation or conflation of case-sensitive paths.
- The exact existing rule limit applies: nonempty, <=256 UTF-8 bytes, no control
  characters or surrounding whitespace. Longer paths explicitly reject; there is
  no implicit rule-file migration or replacement of existing user identifiers.
- Linux returns Unsupported. The provider never invents a synthetic native ID.

Native handles remain private in a non-serializable, non-Debug snapshot. Window
names, document contents, keystrokes, clipboard contents and private keys are not
queried. Process IDs/creation times are resource-instance consistency material,
not authentication. No action capability or user grant derives from these reads.

## Native consistency and failure

Windows samples foreground HWND, GUI-thread focus child and owner PID/thread;
reads the executable via a limited-query process handle; retains private process
instance data; then rechecks the foreground/focus. It also checks the calling
thread's desktop against the currently queryable input desktop without switching
or attaching threads. Null focus is unavailable, never replaced by the top-level
window. Process handles and opened desktop handles have RAII cleanup.

macOS uses NSWorkspace/NSRunningApplication, with process-instance equality and
case-sensitive bundle identity. AX access is attempted only if already trusted
and Secure Input is not active. System-wide AX focused application, focused window
and focused UI element must have the expected owner; repeated reads must agree.
AX IPC has an explicit timeout in the current process. No TCC prompt/options call,
security toggle, event tap, keyboard query or window-title read occurs.

`matches_current` repeats native observations and rejects unavailable/changed/
revoked context. It is NOT atomic with a later action submission. It cannot detect
focus leaving and returning between samples, and NSRunningApplication properties
follow AppKit run-loop rules. Continuous native notifications, generation changes,
cryptographically random process-local tokens and late dispatch validation are
still required before G1 execution. Do not wire a sampled match directly to an action.

Errors distinguish unsupported/no foreground/no focus/changed during read/denied/
missing or oversized identifier/query failure. They never silently follow another
target. Input capabilities remain compile-time support only; OS query success
never means user consent or proof injection would succeed. Windows readable input
desktop does not diagnose UAC or privilege compatibility. macOS Secure Input is
reported and respected, never disabled.

## Explicit diagnostics and privacy

```
peerglide diagnose-context
peerglide diagnose-context --include-application-id
```

No identity, certificate, pairing, listener or native flag is needed. The default
JSON contains only schema/platform/status/OS permission observations and explicit
`action_execution_supported=false` / `native_capture_started=false`. Application
IDs are omitted unless requested: a Windows installation path can contain personal
information. No PID/HWND/AX reference/target token or title is serialized. The
command does not write configuration or reports. The smoke captures IDs only in
memory and prints schema/status success, never the identifier or payload.

Settings offers a Chinese **检查当前系统权限** button. It reads only on click and
shows the GUI process's OS snapshot; the worker executable may have different OS
trust. The cached display is not an authorization source and must be refreshed
following permission changes. No checkbox/grant is enabled by diagnostic success.

## Evidence and remaining work

Pure identifier/focus/instance/error regressions plus CLI argument isolation and
three-platform `tools/context_smoke.py` run in CI. Unavailable runner desktops
remain unavailable; the smoke does not manufacture a passing foreground target.
Local preflight includes the same read-only smoke after build, while real desktop,
physical input and GUI acceptance fields stay separate. Candidate 31ae8be passed all five jobs in CI run 41806308409, including native
read-only process smoke on Windows/macOS and explicit Linux Unsupported. Local
Mac process and GUI-button evidence, unverified focus/physical gates, and the
final documentation follow-up are recorded in VALIDATION.md and the PR.

This increment does not finish G03/G04. The shared capture short path, continuous
focus/target invalidation, local action Backend/owned-injection ledger, independent
default-off local worker consent, overlay/tray and C06 remain required. Existing
input v5, escape/watchdog/cleanup and MIT are unchanged. No global G1 or remote
G2 action is enabled.

Primary API references:

- https://developer.apple.com/documentation/appkit/nsworkspace/frontmostapplication
- https://developer.apple.com/documentation/applicationservices/axisprocesstrusted()
- https://docs.rs/objc2-app-kit/0.3.2/objc2_app_kit/struct.NSRunningApplication.html
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getforegroundwindow
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getguithreadinfo
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-queryfullprocessimagenamew
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-openinputdesktop

## Notification invalidation and local bindings (implementation increment)

`context::watch::LocalContexts` adds explicit native notification ownership, not
another keyboard/mouse capture stack. Windows uses read-only OUTOFCONTEXT WinEvent
foreground/focus/destroy/desktop notifications plus a hidden NOACTIVATE session/
power/environment message window. macOS requires the main thread, seven Workspace
notifications and all required AX app/window/control subscriptions. Unsupported
required AX notifications fail closed; there is no polling-only ticket fallback.

Only `Signal::notify` runs in notification callbacks: one coalescing atomic OR.
The same versioned atomic word holds dirty/closed/ready plus a checked 61-bit epoch.
The owner drains bounded native work, acknowledges invalidations, refreshes its
actual cached target, and publishes ready by CAS. Notifications during refresh
leave the frame unready. This prevents ready ABA from relabelling an old trigger
with a new target generation. Overflow closes the owner rather than wrapping.

Clone `TriggerSource` before entering capture. Its `mark()` records fixed-size
worker identity/epoch and the original monotonic start; it performs no allocation,
random generation, target query or recognition. Queue loss/emergency can call
`invalidate()` from the existing short path. Capture uses that original mark;
changed generations, delayed events and target-cache differences reject instead
of following a new foreground target or restarting the deadline.

The worker creates random 32-byte worker/target values from OS entropy outside
callbacks. There is no PID, timestamp, fixed fixture or counter fallback. Contexts
expire at their original mark deadline, no more than the existing 5-second action
limit. Pending bindings and consumed leases share 128 resource slots until their
native references are dropped. Entries consume once before fallible native checks;
configuration, notifications, uncertainty, queue saturation and shutdown invalidate
old bindings. A consumed lease requires the same live owner to poll and perform
late native/permission/target/expiry checks. None of these values is a remote
credential, user consent or an action capability.

```
peerglide diagnose-context --observe-ms 100
```

This explicit 1–5000 ms diagnostic attempts subscription, polls and stops it; it
never calls capture/consume or mints an action context. Initial native setup/query
time is additional to the requested observation interval. Default diagnostics
still start no observer. Output is status/counts only, with `context_minted=false`
and `delivery_verified=false`; unavailable native access remains unavailable.
Cleanup failure is a failing smoke result, not accepted as a successful stop.
Windows cleanup detaches callback pointers first and poisons the thread after a
failed unregister. AX sources are removed before observer release, and borrowed
run-loop sources/thread desktops are never independently released.

Finite pumps are NOT total ordering barriers. OS notification delay, stalled
loops, application accessibility omissions, modal modes and transient permission/
session changes still need real native acceptance. Common-mode AX registration
and late native rechecks reduce these gaps but do not prove zero missed events or
atomic check-and-submit. No Backend or global G1 path is enabled by this increment.
The existing single capture lifecycle, ownership/escape/watchdog, explicit local
worker consent, owned action injection and C06 remain gates before execution.
