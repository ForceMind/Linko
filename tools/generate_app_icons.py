#!/usr/bin/env python3
"""Render the approved repository SVG into new native icon assets on macOS.

Uses existing AppKit/Swift, sips and stdlib icon containers; never registers an app.
Committed outputs let CI build without these macOS generation tools.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "docs/design/linko-brand-v3/app-icon.svg"
MAC_SOURCE = ROOT / "docs/design/linko-brand-v3/app-icon-macos.svg"
OUTPUT = ROOT / "apps/peerglide-gui/assets/brand"
NAMES = ("app-icon.png", "app-icon-256.png", "Linko.icns", "Linko.ico", "icon-source.json", "app-icon-macos-256.png")

SWIFT = r'''
import AppKit
let input = CommandLine.arguments[1]
let output = CommandLine.arguments[2]
guard let image = NSImage(contentsOfFile: input),
      let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 1024,
        pixelsHigh: 1024, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
        isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
      let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
    fatalError("approved SVG could not be decoded")
}
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = context
image.draw(in: NSRect(x: 0, y: 0, width: 1024, height: 1024),
           from: .zero, operation: .copy, fraction: 1)
context.flushGraphics()
NSGraphicsContext.restoreGraphicsState()
guard let png = bitmap.representation(using: .png, properties: [:]) else {
    fatalError("icon PNG encoding failed")
}
try png.write(to: URL(fileURLWithPath: output), options: .withoutOverwriting)
'''


def png_size(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise ValueError("icon is not a PNG")
    return struct.unpack(">II", data[16:24])


def make_ico(images):
    offset = 6 + 16 * len(images)
    entries = []
    payload = []
    for size, data in images:
        if png_size(data) != (size, size) or not 1 <= size <= 256:
            raise ValueError("invalid ICO image size")
        entries.append(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0,
                                   1, 32, len(data), offset))
        payload.append(data)
        offset += len(data)
    return struct.pack("<HHH", 0, 1, len(images)) + b"".join(entries + payload)


def make_icns(images):
    chunks = []
    for kind, size, data in images:
        if png_size(data) != (size, size):
            raise ValueError("invalid ICNS image size")
        chunks.append(kind.encode("ascii") + struct.pack(">I", len(data) + 8) + data)
    payload = b"".join(chunks)
    return b"icns" + struct.pack(">I", len(payload) + 8) + payload


def main():
    for name in NAMES:
        path = OUTPUT / name
        if path.exists() or path.is_symlink():
            raise FileExistsError("refusing to replace existing icon assets")
    with tempfile.TemporaryDirectory(prefix="linko-app-icons-") as directory:
        temporary = Path(directory)
        script = temporary / "render.swift"
        script.write_text(SWIFT)
        master = temporary / "master.png"
        cache = Path(tempfile.gettempdir()) / "linko-app-icon-module-cache"
        env = {**os.environ, "CLANG_MODULE_CACHE_PATH": str(cache),
               "SWIFT_MODULECACHE_PATH": str(cache)}
        subprocess.run(["/usr/bin/swift", "-module-cache-path", str(cache),
                        str(script), str(SOURCE), str(master)], env=env, check=True)
        if png_size(master.read_bytes()) != (1024, 1024):
            raise ValueError("invalid rendered master size")
        sizes = {}
        for size in (16, 24, 32, 48, 64, 128, 256, 512, 1024):
            image = temporary / f"{size}.png"
            if size == 1024:
                shutil.copyfile(master, image)
            else:
                subprocess.run(["/usr/bin/sips", "-z", str(size), str(size),
                                str(master), "--out", str(image)], check=True,
                               stdout=subprocess.DEVNULL)
            sizes[size] = image
        # Legacy ICNS needs optical outer space; never shrink the Windows icon.
        mac_master = temporary / "mac-master.png"
        subprocess.run(["/usr/bin/swift", "-module-cache-path", str(cache),
                        str(script), str(MAC_SOURCE), str(mac_master)], env=env, check=True)
        mac_sizes = {}
        for size in (16, 32, 64, 128, 256, 512, 1024):
            image = temporary / f"mac-{size}.png"
            subprocess.run(["/usr/bin/sips", "-z", str(size), str(size),
                            str(mac_master), "--out", str(image)], check=True,
                           stdout=subprocess.DEVNULL)
            mac_sizes[size] = image
        # Modern macOS supports PNG-backed ICNS chunks. The explicit container
        # avoids depending on iconutil's iconset conversion service in a sandbox.
        icns = make_icns([(kind, size, mac_sizes[size].read_bytes()) for kind, size in
                          (("icp4", 16), ("icp5", 32), ("icp6", 64), ("ic07", 128),
                           ("ic08", 256), ("ic09", 512), ("ic10", 1024))])
        ico = make_ico([(size, sizes[size].read_bytes()) for size in (16, 24, 32, 48, 64, 128, 256)])
        files = {"app-icon.png": master.read_bytes(), "app-icon-256.png": sizes[256].read_bytes(),
                 "Linko.icns": icns, "Linko.ico": ico,
                 "app-icon-macos-256.png": mac_sizes[256].read_bytes()}
        metadata = {"source": str(SOURCE.relative_to(ROOT)),
                    "source_sha256": hashlib.sha256(SOURCE.read_bytes()).hexdigest(),
                    "macos_source": str(MAC_SOURCE.relative_to(ROOT)),
                    "macos_source_sha256": hashlib.sha256(MAC_SOURCE.read_bytes()).hexdigest(),
                    "macos_artwork_bounds": [100, 100, 824, 824],
                    "renderer": "macOS AppKit SVG; sips sizes; stdlib PNG-backed ICNS/ICO",
                    "assets": {name: {"sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
                               for name, data in files.items()}}
        files["icon-source.json"] = (json.dumps(metadata, ensure_ascii=False, indent=2) + "\n").encode()
        OUTPUT.mkdir(parents=True, exist_ok=True)
        for name, data in files.items():
            with (OUTPUT / name).open("xb") as output:
                output.write(data)
    print("Generated approved Linko native icons; no app was installed or launched")


if __name__ == "__main__":
    main()
