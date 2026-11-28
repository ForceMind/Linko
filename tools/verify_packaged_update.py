#!/usr/bin/env python3
"""Rehearse a portable update with real packages on the current Actions runner."""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile
import zipfile

import package
import portable_update
import verify_portable_package as portable


def gh_json(*args):
    result = subprocess.run(["gh", "api", *args], check=True, capture_output=True, timeout=30)
    return json.loads(result.stdout)


def packaged_archive(folder):
    archives = list(folder.glob(f"{package.PRODUCT_NAME}-*.zip"))
    portable.require(len(archives) == 1, "expected a single CI package archive")
    archive = archives[0]
    digest = portable.read_checksum(archive.with_suffix(".zip.sha256"), archive.name)
    return archive, digest


def baseline_source(archive, review, current):
    with zipfile.ZipFile(archive) as zipped:
        entries = portable.members(zipped, archive.stem)
        source = json.loads(portable.read_metadata(zipped, entries, package.SOURCE_MANIFEST))
    portable.require(
        set(source) == set(current)
        and all(source[field] == current[field] for field in (
            "schema_version", "product", "application_id", "system", "machine",
        ))
        and source["review_commit"] == review
        and all(re.fullmatch(r"[0-9a-f]{40}", source[field] or "") for field in (
            "checkout_commit", "checkout_tree",
        )),
        "baseline package source differs from its verified Actions run",
    )
    return source


def find_baseline(root, current):
    repo = os.environ["GITHUB_REPOSITORY"]
    artifact = os.environ["LINKO_PACKAGE_ARTIFACT"]
    current_run = int(os.environ["GITHUB_RUN_ID"])
    branches = dict.fromkeys((os.environ["GITHUB_HEAD_REF"], os.environ["GITHUB_BASE_REF"]))
    for branch in branches:
        runs = gh_json(
            f"repos/{repo}/actions/workflows/ci.yml/runs", "--method", "GET",
            "-f", f"branch={branch}", "-f", "event=pull_request", "-f", "status=success",
            "-f", "per_page=6",
        )["workflow_runs"]
        for run in runs:
            run_id, head = run["id"], run["head_sha"]
            if (run_id >= current_run or head == current["review_commit"]
                    or run["head_branch"] != branch
                    or (run.get("head_repository") or {}).get("full_name") != repo
                    or run["conclusion"] != "success"):
                continue
            artifacts = gh_json(f"repos/{repo}/actions/runs/{run_id}/artifacts")["artifacts"]
            if not any(item["name"] == artifact and not item["expired"] for item in artifacts):
                continue
            folder = root / str(run_id)
            subprocess.run(
                ["gh", "run", "download", str(run_id), "--repo", repo, "--name", artifact,
                 "--dir", str(folder)],
                check=True, timeout=120,
            )
            archive, digest = packaged_archive(folder)
            source = baseline_source(archive, head, current)
            if source["build_version"] != current["build_version"]:
                print(f"Baseline run {run_id}, review {head}, archive {archive.name}")
                return archive, digest, source
    return None


def stage(versions, archive, digest, source):
    folder = portable_update.stage(versions, archive, digest, source)
    receipt = hashlib.sha256((folder / portable_update.RECEIPT).read_bytes()).hexdigest()
    payload = folder / portable_update.PAYLOAD
    entry = portable_update.launch_path(payload, source["system"])
    if source["system"] == "darwin":
        worker = entry / "Contents" / "MacOS" / "peerglide"
    else:
        worker = payload / ("peerglide.exe" if source["system"] == "windows" else "peerglide")
    return folder.name, digest, receipt, entry, worker


def activate(versions, staged):
    label, digest, receipt, entry, worker = staged
    portable.require(portable_update.activate(versions, label, digest, receipt) == entry,
                     "portable activation returned an unexpected entry")
    portable.require(portable_update.current_entry(versions) == entry,
                     "active portable entry did not round-trip")
    start_worker(versions, worker)


def start_worker(versions, worker):
    environment = {key: value for key, value in os.environ.items()
                   if key.upper() not in ("GH_TOKEN", "GITHUB_TOKEN")}
    subprocess.run([str(worker), "--version"], check=True, cwd=versions, env=environment, timeout=15)


def rehearse_rollback(versions, baseline, current):
    notice = versions / baseline[0] / portable_update.PAYLOAD / "README.md"
    state_file = versions / portable_update.ACTIVE
    active = state_file.read_bytes()
    with notice.open("r+b") as file:
        byte = file.read(1)
        portable.require(len(byte) == 1, "empty staged notice")
        file.seek(0)
        file.write(bytes([byte[0] ^ 1]))
        file.flush()
        try:
            try:
                portable_update.rollback(versions)
            except ValueError as error:
                portable.require(str(error) == "staged member changed", "unexpected rollback failure")
            else:
                raise ValueError("tampered baseline was accepted")
            portable.require(state_file.read_bytes() == active, "failed rollback changed selection")
        finally:
            file.seek(0)
            file.write(byte)
    portable.require(portable_update.current_entry(versions) == current[3],
                     "failed rollback altered the active entry")
    portable.require(portable_update.rollback(versions) == baseline[3],
                     "rollback selected an unexpected entry")
    portable.require(portable_update.current_entry(versions) == baseline[3],
                     "rolled-back entry did not round-trip")
    start_worker(versions, baseline[4])


def main():
    version = package.resolve_version(package.ROOT)
    source = package.build_source(package.ROOT, version, platform.system(), platform.machine())
    archive, digest = packaged_archive(Path(os.environ["LINKO_CURRENT_PACKAGE_DIR"]))
    with tempfile.TemporaryDirectory(prefix="linko-portable-rehearsal-",
                                     dir=os.environ["RUNNER_TEMP"]) as directory:
        root = Path(directory)
        versions = root / "versions"
        versions.mkdir()
        baseline = find_baseline(root, source)
        if baseline is not None:
            staged_baseline = stage(versions, *baseline)
            activate(versions, staged_baseline)
        current = stage(versions, archive, digest, source)
        activate(versions, current)
        print(f"PASS: staged, activated and launched worker from {archive.name}")
        if baseline is None:
            print("SKIP: no retained earlier verified package for this branch or its base")
        else:
            rehearse_rollback(versions, staged_baseline, current)
            print(f"PASS: rejected tampered rollback and restored {baseline[0].name}")


if __name__ == "__main__":
    main()
