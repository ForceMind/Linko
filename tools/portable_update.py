"""Stage verified versions in a caller-selected portable update directory."""
import hashlib
import json
import os
from pathlib import Path
import shutil
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
    return not path.is_symlink() and path.is_dir()


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
