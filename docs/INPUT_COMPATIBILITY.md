# Input compatibility and C02 status

C02 is implementing, not accepted. The current finite HID map does not imply
complete OEM/numpad/media/IME support. Unknown physical key events cancel the
current control/switch intent, immediately disable local suppression and pass
through locally. A payload-free diagnostic explains that no forwarding occurred.
Remote pressed inputs are released through existing session recovery. Unknown
keys are tracked separately from HID usages and block acquisition while held;
startup sampling includes unknown keys in the sampled OS keycode ranges.

This is a safe fallback, not remote support for those keys. Consumer/media events
not delivered through the existing native keyboard callbacks remain outside this
coverage. Full extended-key mapping, capability negotiation, keyboard layouts and
physical Windows/macOS acceptance are still pending under C02/#3. No wire or
permission change is introduced. Protocol v3 normal-return ordering is unchanged.

Native hook tests cover cancellation, held-key guard, autorepeat, no raw input
forwarding, armed-toggle cancellation and bounded-queue fail-open behavior.
Cross compilation is only typechecking; platform CI and real hardware acceptance
are separate gates. C03 thread shutdown and C04 structured events remain pending.


## Physical scan and keypad follow-up

Windows capture now maps set-1 scan positions plus E0, rather than layout-dependent
virtual letters. Injection uses KEYEVENTF_SCANCODE and the matching extended/up
flags. Main/keypad Enter, navigation/keypad digits, and left/right modifiers stay
distinct. Startup VK samples use their own namespace so keypad Enter release can
clear a previously ambiguous VK_RETURN sample without leaving acquisition stuck.
This does not establish per-device state for two physical keyboards holding the
same key; real hardware acceptance remains required.

Both backends add HID keypad digits/operators/decimal/Enter and ISO backslash.
Num Lock/Clear is the physical HID position, not a promise of identical toggle
semantics between operating systems. Text comes from the target layout/IME;
physical forwarding is not Unicode text translation. Media, Pause/PrintScreen,
full OEM coverage and explicit peer capability negotiation remain pending. Older
backends may reject new keypad usages and stop safely; update both peers to use
the extended map. Network protocol v3 is unchanged by this mapping slice.

Windows tests check physical scan round trips, down/up flags, E0 distinctions and
startup keypad release. macOS CI verifies the new constants against the active
Apple SDK Events.h, independently of the Rust round-trip test. These are compile,
SDK and logic checks, not desktop input acceptance.

Primary API references:
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-keybdinput
- https://developer.apple.com/library/archive/technotes/tn2450/
- Active Apple SDK HIToolbox Events.h, checked by tools/verify_macos_keycodes.py.


## P01 input capabilities (v5)

The authenticated Hello advertises the compiled receive map. Its bitmap never
adds permission. Diagnostic/native-disabled/Linux endpoints advertise none.
Unsupported target input is passed locally before the callback can swallow it;
the old remote lease is released. The receiver independently enforces its map.
Both peers must update to v5. See ADR-0004; full OEM/media and real layout/IME
acceptance still remain pending. Capability negotiation does not create support
for keys absent from the backend tables.

## Extended function key coverage

Windows maps F13–F24 using the documented Set 1 scan codes. Capture requires
VK_F24 for the scan code that also represents Japanese LANG5. macOS maps
F13–F20, with virtual key constants checked against the active HIToolbox SDK.
The v5 bitmap advertises only the keys present in that receiver. Physical
keyboard coverage and cross-host behavior remain C06 acceptance items.

- [Microsoft scan-code table](https://learn.microsoft.com/en-us/windows/win32/inputdev/about-keyboard-input#scan-codes)
- [Apple HID usage table](https://developer.apple.com/library/archive/technotes/tn2450/)
