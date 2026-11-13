"""Stage verified versions in a caller-selected portable update directory."""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile

import package
import verify_macos_package
import verify_portable_package as portable

RECEIPT = "STAGED_VERSION.json"
PAYLOAD = "payload"
BLOCK_BYTES = 1024 * 1024


def expected_label(source):
    fields = ("build_version", "system", "machine")
    portable.require(
        isinstance(source, dict) and all(isinstance(source.get(field), str) for field in fields),
        "invalid portable source metadata",
    )
    label = (
        f"{package.PRODUCT_NAME}-{source['build_version']}-"
        f"{source['system']}-{source['machine']}"
    )
    portable.require(portable.safe_component(label), "invalid portable version label")
    return label


def copy_bounded(source, destination, maximum):
    digest = hashlib.sha256()
    written = 0
    with destination.open("xb") as output:
        for block in iter(lambda: source.read(BLOCK_BYTES), b""):
            written += len(block)
            portable.require(written <= maximum, "portable staging exceeds byte limit")
            output.write(block)
            digest.update(block)
        output.flush()
        os.fsync(output.fileno())
    return digest.hexdigest()


def regular_directory(path):
    if not path.is_dir() or path.is_symlink():
        return False
    attributes = getattr(path.lstat(), "st_file_attributes", 0)
    return not attributes & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)


def stage(versions, archive, expected_digest, expected_source):
    """Stage a new version. Leave any existing version and activation record alone.

    The caller supplies expected provenance and checksum from a trusted channel.
    Only the staged copy is used for archive verification and extraction.
    """
    versions, archive = Path(versions), Path(archive)
    portable.require(regular_directory(versions), "portable versions directory must already exist")
    versions = versions.resolve(strict=True)
    label = expected_label(expected_source)
    portable.require(archive.name == label + ".zip", "package name differs from expected version")
    portable.require(not archive.is_symlink() and archive.is_file(), "package must be a regular ZIP")
    portable.require(
        0 < archive.stat().st_size <= portable.MAX_ARCHIVE_BYTES,
        "package size outside limits",
    )
    destination = versions / label
    portable.require(
        not destination.exists() and not destination.is_symlink(),
        "portable version already exists",
    )
    with tempfile.TemporaryDirectory(prefix=".linko-stage-", dir=versions) as folder:
        workspace = Path(folder)
        staged_archive = workspace / archive.name
        with archive.open("rb") as source:
            copy_bounded(source, staged_archive, portable.MAX_ARCHIVE_BYTES)
        portable.verify(staged_archive, expected_digest, expected_source)

        contents = workspace / "version"
        payload = contents / PAYLOAD
        payload.mkdir(parents=True)
        hashes = {}
        with zipfile.ZipFile(staged_archive) as zipped:
            for relative, member in portable.members(zipped, label).items():
                file = payload.joinpath(*relative.split("/"))
                file.parent.mkdir(parents=True, exist_ok=True)
                with zipped.open(member) as source:
                    hashes[relative] = copy_bounded(source, file, member.file_size)
                if os.name != "nt" and member.external_attr >> 16 & 0o111:
                    file.chmod(0o755)
        if expected_source["system"] == "darwin" and sys.platform == "darwin":
            verify_macos_package.verify(payload / package.APP_BUNDLE_NAME)

        receipt = {
            "schema_version": 1,
            "archive_sha256": expected_digest,
            "source": expected_source,
            "members": hashes,
        }
        with (contents / RECEIPT).open("x", encoding="utf-8") as output:
            json.dump(receipt, output, ensure_ascii=True, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())

        # Exclusive reservation avoids replacing even an empty directory from a
        # competing stage. Only this invocation's reservation may be cleaned up.
        destination.mkdir()
        try:
            (contents / RECEIPT).rename(destination / RECEIPT)
            payload.rename(destination / PAYLOAD)
        except BaseException:
            shutil.rmtree(destination)
            raise
    return destination


ACTIVE = "ACTIVE_VERSION.json"
LOCK = ".linko-update.lock"
MAX_STATE_BYTES = 4 * 1024 * 1024


@contextlib.contextmanager
def update_lock(versions):
    path = versions / LOCK
    portable.require(not path.is_symlink(), "invalid portable update lock")
    flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
    with os.fdopen(os.open(path, flags, 0o600), "r+b") as lock:
        portable.require(stat.S_ISREG(os.fstat(lock.fileno()).st_mode), "invalid portable update lock")
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            raise ValueError("another portable update is in progress") from None
        # Closing the handle releases either platform's advisory lock.
        yield


def read_record(path):
    portable.require(not path.is_symlink() and path.is_file(), "invalid portable update record")
    with path.open("rb") as source:
        raw = source.read(MAX_STATE_BYTES + 1)
    portable.require(0 < len(raw) <= MAX_STATE_BYTES, "portable update record exceeds limit")
    data = json.loads(raw)
    portable.require(
        isinstance(data, dict)
        and type(data.get("schema_version")) is int
        and data["schema_version"] == 1,
        "unknown update record",
    )
    return data, hashlib.sha256(raw).hexdigest()


def valid_digest(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def version_pointer(value):
    portable.require(
        isinstance(value, dict)
        and set(value) == {"label", "receipt_sha256"}
        and isinstance(value["label"], str)
        and portable.safe_component(value["label"])
        and valid_digest(value["receipt_sha256"]),
        "invalid portable version pointer",
    )
    return value


def read_active(versions):
    path = versions / ACTIVE
    if not path.exists() and not path.is_symlink():
        return None
    active, _ = read_record(path)
    portable.require(set(active) == {"schema_version", "current", "previous"}, "invalid active record")
    version_pointer(active["current"])
    if active["previous"] is not None:
        version_pointer(active["previous"])
        portable.require(
            active["previous"]["label"] != active["current"]["label"],
            "duplicate active and previous versions",
        )
    return active


def read_staged(versions, label, expected_receipt=None, expected_archive=None):
    portable.require(portable.safe_component(label), "invalid staged version label")
    version = versions / label
    payload = version / PAYLOAD
    portable.require(regular_directory(version) and regular_directory(payload), "staged payload missing")
    receipt, receipt_digest = read_record(version / RECEIPT)
    portable.require(
        set(receipt) == {"schema_version", "archive_sha256", "source", "members"}
        and valid_digest(receipt["archive_sha256"])
        and isinstance(receipt["source"], dict)
        and isinstance(receipt["members"], dict),
        "invalid staged receipt",
    )
    if expected_receipt is not None:
        portable.require(receipt_digest == expected_receipt, "staged receipt changed")
    if expected_archive is not None:
        portable.require(receipt["archive_sha256"] == expected_archive, "staged archive digest differs")
    source = receipt["source"]
    portable.require(
        source.get("product") == package.PRODUCT_NAME
        and source.get("application_id") == package.APP_IDENTIFIER
        and expected_label(source) == label
        and source["system"] == platform.system().lower()
        and portable.ARCHITECTURES.get(source["machine"]) is not None
        and portable.ARCHITECTURES.get(source["machine"])
        == portable.ARCHITECTURES.get(platform.machine().lower()),
        "staged version belongs to another product or platform",
    )

    members = receipt["members"]
    portable.require(0 < len(members) <= portable.MAX_ENTRIES, "staged member count outside limits")
    actual = set()
    total = 0
    for folder, dirs, files in os.walk(payload, onerror=raise_walk_error, followlinks=False):
        for name in dirs:
            path = Path(folder) / name
            portable.require(regular_directory(path), "invalid staged directory")
        for name in files:
            path = Path(folder) / name
            relative = path.relative_to(payload).as_posix()
            portable.require(
                not path.is_symlink() and path.is_file()
                and len(actual) < len(members)
                and valid_digest(members.get(relative)),
                "unexpected staged member",
            )
            total += path.stat().st_size
            portable.require(total <= portable.MAX_EXPANDED_BYTES, "staged payload exceeds limit")
            digest = hashlib.sha256()
            with path.open("rb") as reader:
                for block in iter(lambda: reader.read(BLOCK_BYTES), b""):
                    digest.update(block)
            portable.require(digest.hexdigest() == members[relative], "staged member changed")
            actual.add(relative)
    portable.require(actual == set(members), "staged member missing")
    return {"label": label, "receipt_sha256": receipt_digest}, launch_path(payload, source["system"])


def raise_walk_error(error):
    raise error


def launch_path(payload, system):
    if system == "windows":
        entry = payload / "Linko.exe"
    elif system == "darwin":
        entry = payload / package.APP_BUNDLE_NAME
    else:
        entry = payload / "linko"
    portable.require(entry.exists(), "staged GUI entry is missing")
    return entry


def write_active(versions, current, previous):
    next_state = {"schema_version": 1, "current": current, "previous": previous}
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", prefix=".linko-selection-", dir=versions, delete=False
        ) as output:
            temporary = Path(output.name)
            json.dump(next_state, output, sort_keys=True)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, versions / ACTIVE)
        temporary = None
        if os.name != "nt":
            try:
                directory = os.open(versions, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
                try:
                    os.fsync(directory)
                finally:
                    os.close(directory)
            except OSError:
                raise RuntimeError("portable selection changed; directory durability unconfirmed") from None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def version_root(versions):
    versions = Path(versions)
    portable.require(regular_directory(versions), "portable versions directory must already exist")
    return versions.resolve(strict=True)


def activate(versions, label, expected_archive, expected_receipt):
    """Select a version using the trusted archive and the staging receipt digest."""
    portable.require(valid_digest(expected_archive), "invalid expected archive digest")
    portable.require(valid_digest(expected_receipt), "invalid expected receipt digest")
    versions = version_root(versions)
    with update_lock(versions):
        active = read_active(versions)
        selected, entry = read_staged(
            versions, label, expected_receipt=expected_receipt, expected_archive=expected_archive
        )
        previous = active["current"] if active is not None else None
        if previous != selected:
            portable.require(previous is None or previous["label"] != label, "current receipt changed")
            write_active(versions, selected, previous)
        return entry


def rollback(versions):
    """Restore the immediately previous verified version in one state update."""
    versions = version_root(versions)
    with update_lock(versions):
        active = read_active(versions)
        portable.require(active is not None and active["previous"] is not None, "no previous portable version")
        previous = active["previous"]
        selected, entry = read_staged(
            versions, previous["label"], expected_receipt=previous["receipt_sha256"]
        )
        write_active(versions, selected, active["current"])
        return entry


def current_entry(versions):
    versions = version_root(versions)
    with update_lock(versions):
        active = read_active(versions)
        portable.require(active is not None, "no selected portable version")
        current = active["current"]
        _, entry = read_staged(
            versions, current["label"], expected_receipt=current["receipt_sha256"]
        )
        return entry


def main():
    parser = argparse.ArgumentParser(description="Manage versions in a selected Linko portable directory")
    parser.add_argument("--versions", type=Path, required=True)
    commands = parser.add_subparsers(dest="operation", required=True)

    staging = commands.add_parser("stage")
    staging.add_argument("--archive", type=Path, required=True)
    staging.add_argument("--archive-sha256", required=True)
    staging.add_argument("--expected-source", type=Path, required=True)

    activation = commands.add_parser("activate")
    activation.add_argument("--label", required=True)
    activation.add_argument("--archive-sha256", required=True)
    activation.add_argument("--receipt-sha256", required=True)

    commands.add_parser("rollback")
    commands.add_parser("current")

    args = parser.parse_args()
    try:
        if args.operation == "stage":
            source, _ = read_record(args.expected_source)
            result = stage(args.versions, args.archive, args.archive_sha256, source)
            _, receipt_digest = read_record(result / RECEIPT)
            print(json.dumps({"label": result.name, "receipt_sha256": receipt_digest}))
        elif args.operation == "activate":
            print(activate(args.versions, args.label, args.archive_sha256, args.receipt_sha256))
        elif args.operation == "rollback":
            print(rollback(args.versions))
        else:
            print(current_entry(args.versions))
    except (OSError, ValueError, RuntimeError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
