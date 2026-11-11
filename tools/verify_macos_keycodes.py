#!/usr/bin/env python3
"""Check new physical key mappings against the active Apple SDK, without input access."""
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
keys.update({0x68 + n: f"F{13 + n}" for n in range(8)})
for usage, name in keys.items():
    match = re.search(rf"\bkVK_{name}\s*=\s*(0x[\dA-Fa-f]+|\d+)", header)
    assert match, f"missing SDK constant kVK_{name}"
    assert pairs[usage] == int(match[1], 0), f"HID {usage:#x} disagrees with SDK kVK_{name}"
print(f"PASS: {len(keys)} keypad, ISO and extended function key mappings match active Apple SDK constants")
