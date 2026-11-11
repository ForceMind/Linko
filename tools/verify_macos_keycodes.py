#!/usr/bin/env python3
"""Check physical key mappings and scroll fields against the active Apple SDK."""
import pathlib
import re
import subprocess

sdk = pathlib.Path(subprocess.check_output(["xcrun", "--show-sdk-path"], text=True).strip())
headers = list((sdk / "System/Library/Frameworks/Carbon.framework").glob("**/Events.h"))
assert headers, f"Events.h not found under SDK {sdk}"
header = headers[0].read_text()
source = pathlib.Path("crates/peerglide-platform/src/macos.rs").read_text()
pairs = {int(a, 0): int(b, 0) for a, b in re.findall(r"\((0x[\da-f]+|\d+),\s*(0x[\da-f]+|\d+)\)", source)}
keys = {0x53: "ANSI_KeypadClear", 0x54: "ANSI_KeypadDivide", 0x55: "ANSI_KeypadMultiply", 0x56: "ANSI_KeypadMinus", 0x57: "ANSI_KeypadPlus", 0x58: "ANSI_KeypadEnter", 0x63: "ANSI_KeypadDecimal", 0x64: "ISO_Section"}
keys.update({0x59 + n: f"ANSI_Keypad{n+1}" for n in range(9)})
keys[0x62] = "ANSI_Keypad0"
for usage, name in keys.items():
    match = re.search(rf"\bkVK_{name}\s*=\s*(0x[\dA-Fa-f]+|\d+)", header)
    assert match, f"missing SDK constant kVK_{name}"
    assert pairs[usage] == int(match[1], 0), f"HID {usage:#x} disagrees with SDK kVK_{name}"
print(f"PASS: {len(keys)} keypad/ISO key mappings match active Apple SDK constants")

frameworks = sdk / "System/Library/Frameworks"
graphics = [
    header
    for framework in ("CoreGraphics.framework", "ApplicationServices.framework")
    for header in (frameworks / framework).glob("**/CGEventTypes.h")
]
assert graphics, f"CGEventTypes.h not found under SDK {sdk}"
event_types = graphics[0].read_text()
wheel_fields = {
    "WHEEL_AXIS1": "kCGScrollWheelEventDeltaAxis1",
    "WHEEL_AXIS2": "kCGScrollWheelEventDeltaAxis2",
    "WHEEL_CONTINUOUS": "kCGScrollWheelEventIsContinuous",
    "WHEEL_FIXED_AXIS1": "kCGScrollWheelEventFixedPtDeltaAxis1",
    "WHEEL_FIXED_AXIS2": "kCGScrollWheelEventFixedPtDeltaAxis2",
}
for local, apple in wheel_fields.items():
    local_match = re.search(rf"const {local}: u32 = (\d+);", source)
    sdk_match = re.search(rf"\b{apple}\s*=\s*(0x[\dA-Fa-f]+|\d+)", event_types)
    assert local_match and sdk_match, f"missing scroll field {local} or {apple}"
    assert int(local_match[1]) == int(sdk_match[1], 0), f"{local} disagrees with {apple}"
print(f"PASS: {len(wheel_fields)} scroll fields match active Apple SDK constants")
