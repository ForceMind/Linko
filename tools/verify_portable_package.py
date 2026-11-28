#!/usr/bin/env python3
"""Verify a just-built portable package before any extraction or upload."""
import hashlib
import hmac
import json
import platform
import plistlib
import re
import stat
import struct
import unicodedata
import zipfile

import package

MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_EXPANDED_BYTES = 1024 * 1024 * 1024
MAX_ENTRIES = 16_384
MAX_METADATA_BYTES = 1024 * 1024
MAX_PE_OFFSET = 64 * 1024
ARCHITECTURES = {
    "amd64": "x86_64",
    "x86_64": "x86_64",
    "arm64": "arm64",
    "aarch64": "arm64",
}
PE_MACHINES = {"x86_64": 0x8664, "arm64": 0xAA64}
MACH_MACHINES = {"x86_64": 0x01000007, "arm64": 0x0100000C}
ELF_MACHINES = {"x86_64": 62, "arm64": 183}
WINDOWS_RESERVED = {
    "CON", "PRN", "AUX", "NUL", "CLOCK$",
    *(f"{prefix}{digit}" for prefix in ("COM", "LPT") for digit in "123456789¹²³"),
}


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def read_checksum(path, archive_name):
    with path.open("r", encoding="ascii", newline=None) as source:
        text = source.read(513)
    require(len(text) <= 512, "checksum record exceeds limit")
    line = text.removesuffix("\n")
    match = re.fullmatch(r"([0-9a-f]{64})  " + re.escape(archive_name), line)
    require(match is not None, "checksum does not name the expected archive")
    return match[1]


def safe_component(part):
    return (
        bool(part)
        and part not in (".", "..")
        and not part.endswith((" ", "."))
        and part.split(".", 1)[0].upper() not in WINDOWS_RESERVED
        and not any(ord(char) < 32 or char in '<>:"\\|?*' for char in part)
    )


def members(zipped, root):
    infos = zipped.infolist()
    require(0 < len(infos) <= MAX_ENTRIES, "package member count outside limits")
    result = {}
    folded = set()
    expanded = 0
    for entry in infos:
        parts = entry.filename.split("/")
        key = tuple(unicodedata.normalize("NFC", part).casefold() for part in parts)
        require(
            entry.orig_filename == entry.filename
            and not entry.is_dir()
            and len(parts) > 1
            and parts[0] == root
            and all(safe_component(part) for part in parts),
            "unsafe package member path",
        )
        mode = stat.S_IFMT(entry.external_attr >> 16)
        require(mode in (0, stat.S_IFREG), "package member is not a regular file")
        require(not entry.flag_bits & 1, "encrypted package member")
        require(key not in folded, "duplicate package member")
        folded.add(key)
        result["/".join(parts[1:])] = entry
        expanded += entry.file_size
        require(expanded <= MAX_EXPANDED_BYTES, "package expansion exceeds limit")
    for key in folded:
        require(
            all(key[:length] not in folded for length in range(1, len(key))),
            "package contains a file/directory collision",
        )
    return result


def verify_executable(source, system, machine):
    if system == "windows":
        header = source.read(64)
        require(len(header) == 64 and header[:2] == b"MZ", "invalid PE header")
        offset = int.from_bytes(header[60:64], "little")
        require(64 <= offset <= MAX_PE_OFFSET, "PE header offset outside limits")
        source.seek(offset)
        header = source.read(26)
        require(len(header) == 26 and header[:4] == b"PE\0\0", "invalid PE signature")
        cpu, = struct.unpack_from("<H", header, 4)
        flags, optional = struct.unpack_from("<HH", header, 22)
        require(
            cpu == PE_MACHINES[machine]
            and flags & 0x0002
            and not flags & 0x2000
            and optional == 0x20B,
            "Windows executable architecture or type differs",
        )
    elif system == "darwin":
        header = source.read(16)
        require(len(header) == 16 and header[:4] == b"\xcf\xfa\xed\xfe", "invalid Mach-O header")
        cpu, _, kind = struct.unpack_from("<III", header, 4)
        require(cpu == MACH_MACHINES[machine] and kind == 2, "macOS executable architecture or type differs")
    elif system == "linux":
        header = source.read(20)
        require(len(header) == 20 and header[:6] == b"\x7fELF\x02\x01", "invalid ELF header")
        kind, cpu = struct.unpack_from("<HH", header, 16)
        require(cpu == ELF_MACHINES[machine] and kind in (2, 3), "Linux executable architecture or type differs")
    else:
        raise ValueError("unsupported package platform")


def read_metadata(zipped, entries, name):
    entry = entries.get(name)
    require(entry is not None and 0 < entry.file_size <= MAX_METADATA_BYTES, "package metadata missing or oversized")
    with zipped.open(entry) as source:
        return source.read(MAX_METADATA_BYTES + 1)


def verify(archive, expected_digest, expected_source):
    system = expected_source["system"]
    machine = ARCHITECTURES.get(expected_source["machine"])
    require(machine is not None, "unsupported package architecture")
    require(
        re.fullmatch(r"[0-9a-f]{64}", expected_digest) is not None,
        "invalid expected package checksum",
    )
    require(not archive.is_symlink() and archive.is_file(), "package must be a regular ZIP")
    require(0 < archive.stat().st_size <= MAX_ARCHIVE_BYTES, "package size outside limits")
    with archive.open("rb") as source:
        digest = hashlib.sha256()
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
        actual = digest.hexdigest()
        require(hmac.compare_digest(actual, expected_digest), "package checksum differs")
        source.seek(0)
        with zipfile.ZipFile(source) as zipped:
            entries = members(zipped, archive.stem)
            for name in ("README.md", "LICENSE", "SECURITY.md"):
                require(name in entries and entries[name].file_size > 0, "package notices missing")
            require(
                json.loads(read_metadata(zipped, entries, package.SOURCE_MANIFEST)) == expected_source,
                "package source record differs",
            )
            if system == "darwin":
                contents = package.APP_BUNDLE_NAME + "/Contents/"
                info = plistlib.loads(read_metadata(zipped, entries, contents + "Info.plist"))
                expected_info = package.bundle_info(expected_source["build_version"])
                require(
                    all(info.get(field) == expected_info[field] for field in (
                        "CFBundleIdentifier", "CFBundleExecutable", "CFBundlePackageType",
                        "CFBundleShortVersionString", "CFBundleVersion",
                    )),
                    "macOS package identity or version differs",
                )
                paths = (contents + "MacOS/peerglide-gui", contents + "MacOS/peerglide")
            elif system == "windows":
                paths = ("Linko.exe", "peerglide.exe")
            else:
                paths = ("linko", "peerglide")
            for name in paths:
                require(name in entries, "GUI or worker missing from package")
                with zipped.open(entries[name]) as executable:
                    verify_executable(executable, system, machine)
            require(zipped.testzip() is None, "package member CRC differs")


def main():
    version = package.resolve_version(package.ROOT)
    system = platform.system()
    machine = platform.machine()
    expected = package.build_source(package.ROOT, version, system, machine)
    label = f"{package.PRODUCT_NAME}-{version}-{system.lower()}-{machine.lower()}"
    archive = package.ROOT / "dist" / (label + ".zip")
    digest = read_checksum(archive.with_suffix(".zip.sha256"), archive.name)
    verify(archive, digest, expected)
    print(f"PASS: {archive.name} checksum, paths, source, GUI and worker")


if __name__ == "__main__":
    main()
