# ADR-0009: Chinese application menu and cooperative macOS termination

Status: implementing; platform and native acceptance pending.

## Problem

The locked winit 0.30.13 default application menu uses English process-name labels
and sends Cmd+Q to NSApplication.terminate:. Its delegate has no ShouldTerminate
veto; WillTerminate closes windows through Destroyed/LoopExiting, bypassing the
GUI's cancellable CloseRequested path and asynchronous worker cleanup. A two-second
Drop fallback is not proof of owned-input recovery.

Sources: winit v0.30.13 [menu](https://github.com/rust-windowing/winit/blob/v0.30.13/src/platform_impl/macos/menu.rs),
[app state](https://github.com/rust-windowing/winit/blob/v0.30.13/src/platform_impl/macos/app_state.rs),
[event loop](https://github.com/rust-windowing/winit/blob/v0.30.13/src/platform_impl/macos/event_loop.rs),
and eframe 0.37 [integration](https://github.com/emilk/egui/blob/0.37.0/crates/eframe/src/native/epi_integration.rs).

## Decision

Install a GUI-owned Chinese application menu independently of optional tray
visibility. Own Show/Stop/Quit selectors only post into the GUI's existing atomic
mailbox; Cmd+Q uses the owned Quit selector. Keep About/Services/Hide/Show All as
ordinary AppKit actions. Menu key equivalents are local application shortcuts,
not new global input hooks or input authorization.

For normal Dock/system terminate requests, add only the reviewed delegate's missing
applicationShouldTerminate: selector. Do not replace the delegate or any existing
foreign IMP. Compile-time lockfile review requires exactly winit 0.30.13; runtime
checks require the exact delegate class/instance, expected superclass/lifecycle ABI,
owned bridge IMP/ABI and owned menu/command bindings. Unknown state returns an error.

The permanent Objective-C method forwards through a main-thread weak registry
bound to the current application/delegate instance. It posts Quit and returns
NSTerminateCancel, allowing the ordinary event loop to stop and await the worker.
It does no input work, filesystem/network I/O or blocking waits. Teardown detaches
the weak registration before clearing menu targets and restoring owned menu slots.
Objective-C cannot remove the added selector: with no matching owner it cancels
termination without dereferencing old objects. Failure therefore still permits
ordinary window close, but cannot silently enable new native input.

Create one mailbox with the real egui wake before installing any native menu owner.
Keep it stable for the App lifetime; tray removal/rebuild must never replace it.
Quit precedes worker polling, clears queued native upgrades, sends existing Stop,
waits for terminal/EOF/cleanup and finally closes the window. Recovery-unknown keeps
the existing warning and explicit-risk acknowledgment. Close-to-tray affects the
window close button, not Quit. Hidden windows continue logic without a repaint loop.

Missing/conflicting exit protection blocks new Mac native starts before preference
writes or process creation; communication diagnostics remain available. Detected
protection loss cancels uncommitted input intent, requests Stop for owned native
work and reveals the GUI. No saved preference restores native authority.

## Boundaries and validation

This does not intercept SIGKILL, Force Quit or guarantee OS shutdown/logout: a
cancelled system logout may require the user to request it again after cleanup.
It cannot prove physical key releases or defend hostile code in this process.
The bridge is process-local UI integration, not a competing capture lifecycle.

Test registry lifetime/stale teardown/ABI conflicts, stable mailbox across tray
changes, Quit before poll, unavailable protection and recovery-unknown. Real Mac
menu/Cmd+Q and system-terminate dispatch need native acceptance with no input first;
owned-worker waiting is separate from physical C06. Windows/Linux retain their
normal window-close handling. Input v5, independent grants, identity/config formats,
MIT and minimum Rust 1.95 remain unchanged.
