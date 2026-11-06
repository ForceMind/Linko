# ADR-0007: Optional desktop tray and close lifecycle

Status: implementing; Windows/macOS runtime acceptance pending.

## Problem

The GUI currently closes by cooperatively stopping the worker and waiting for its
terminal cleanup/output. Daily KVM use also needs a recovery entry when the main
window is hidden. Hiding a window must never silently become startup consent,
input consent, or evidence that cleanup completed.

## Decision

Use a GUI-owned native status item on macOS's main thread, and a dedicated Windows
notification message window/thread. They do not install input hooks, global hotkeys,
services or startup entries. Linux remains explicitly unsupported for this slice.

The menu contains only Show, Stop and Quit in Chinese. It cannot enable native input,
change receiving/data grants or execute arbitrary commands. Callbacks coalesce these
three intents into atomic bits and wake the GUI; they do not read private contents,
perform file/network I/O, or execute worker commands. Stop/Quit cannot be dropped by
a full queue. Quit wins over Show, and Stop/Quit cancel queued upgrades before poll
can launch a successor.

Desktop choices live in a separate versioned `desktop-preferences.json`, using the
existing bounded store/locks. Input `settings.json`, its schema and device identity
are unchanged, so an older input GUI is not broken by a new optional desktop field.
Missing fields default off; invalid/future desktop files are retained read-only and
cannot enable a tray or background retention.

By default, closing the window keeps its existing Stop -> terminal cleanup/output
-> exit flow. Only an explicitly enabled tray, a currently available native recovery
entry, and the user's separate close-to-tray choice may turn Close into CancelClose
plus Hide. A failed/unavailable tray falls back to stopping and exiting. Disabling
the tray restores a hidden window before removing its recovery entry.

Hidden sessions keep their existing process polling. Native menu intents wake idle
GUI logic without a permanent repaint timer. Show restores the window. Stop uses
the existing cooperative stdin channel. Quit requests cleanup and closes only when
the worker/output is finished and recovery is confirmed; recovery-unknown reveals
the window and retains the existing warning/explicit-exit path. Hiding or quitting
never proves that the OS or target application received synthetic releases.

## Validation

Pure mailbox tests cover coalescing and concurrent floods. GUI lifecycle tests use
real non-native helper processes to cover hide-vs-stop, unavailable recovery entry,
Stop/Quit-before-poll, queued upgrade cancellation and unknown cleanup revealing the
window. Future desktop preferences must remain intact and input settings unchanged.
Native icon/menu creation, Explorer restart, hidden wake and teardown need platform
runtime checks. None of these tests count as physical C06 input acceptance.

The input ALPN remains v5. No pairing, data framing, G1/G2, signing trust root or
system permission policy changes are introduced by this decision.
