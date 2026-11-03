#!/usr/bin/env python3
"""Screen resolved SPDX metadata and collect upstream license/notice files.

This is an automated screening/notice collector, not a complete legal audit.
"""
import argparse
import json
import pathlib
import re
import shutil
import subprocess

ALLOWED = {"MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "ISC", "0BSD",
           "BSD-2-Clause", "BSD-3-Clause", "Zlib", "Unicode-3.0", "OFL-1.1",
           "Ubuntu-font-1.0", "CC0-1.0", "MIT-0", "Unlicense", "BSL-1.0"}


def select(expression):
    tokens = re.findall(r"\(|\)|[A-Za-z0-9.+-]+", expression.replace("/", " OR "))
    pos = 0

    def term():
        nonlocal pos
        token = tokens[pos]
        pos += 1
        if token == "(":
            value = either()
            assert tokens[pos] == ")"
            pos += 1
            return value
        if pos < len(tokens) and tokens[pos] == "WITH":
            token += " WITH " + tokens[pos + 1]
            pos += 2
        return token if token in ALLOWED else None

    def both():
        nonlocal pos
        value = term()
        while pos < len(tokens) and tokens[pos] == "AND":
            pos += 1
            other = term()
            value = f"{value} AND {other}" if value and other else None
        return value

    def either():
        nonlocal pos
        value = both()
        while pos < len(tokens) and tokens[pos] == "OR":
            pos += 1
            other = both()
            value = value or other
        return value

    answer = either()
    assert pos == len(tokens)
    return answer


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--inventory", type=pathlib.Path)
    parser.add_argument("--notices", type=pathlib.Path)
    args = parser.parse_args()
    data = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--format-version", "1"]))
    packages = sorted((p for p in data["packages"] if p["source"]), key=lambda p: (p["name"], p["version"]))
    rows = ["# Resolved dependency license inventory", "", "Generated from locked Cargo metadata. Includes platform-specific dependencies; not every listed package is linked on every OS. License selection and copied upstream notices require distribution review.", "", "| Package | Version | Declared expression | Selected terms |", "|---|---|---|---|"]
    for package in packages:
        expression = package.get("license") or ""
        chosen = select(expression) if expression else None
        if not chosen:
            raise SystemExit(f"Unreviewed license: {package['name']} {expression!r}")
        rows.append(f"| {package['name']} | {package['version']} | {expression} | {chosen} |")
        if args.notices:
            source = pathlib.Path(package["manifest_path"]).parent
            target = args.notices / f"{package['name']}-{package['version']}"
            target.mkdir(parents=True, exist_ok=True)
            (target / "LICENSE-SELECTION.txt").write_text(f"Declared: {expression}\nSelected: {chosen}\n", encoding="utf-8")
            copied = 0
            for file in source.rglob("*"):
                if file.is_file() and (
                    any(word in file.name.lower() for word in ("license", "copying", "copyright", "notice", "unlicense"))
                    or ("fonts" in file.relative_to(source).parts[:-1] and file.suffix.lower() == ".txt")
                ):
                    dest = target / file.relative_to(source)
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(file, dest)
                    copied += 1
            if not copied:
                # Some published crates omit top-level license files. Preserve the
                # complete published source and its license declaration for review.
                shutil.copytree(source,target/'published-source',dirs_exist_ok=True)
                (target/'NOTICE-REVIEW.txt').write_text('The crate archive contains no separate license file. Complete published source and Cargo license declaration are included. Inspect upstream license/attribution before stable distribution.\n',encoding='utf-8')
    text = "\n".join(rows) + "\n"
    if args.inventory:
        args.inventory.parent.mkdir(parents=True, exist_ok=True)
        args.inventory.write_text(text, encoding="utf-8")
    if args.notices:
        (args.notices / "INVENTORY.md").write_text(text, encoding="utf-8")
    print(f"PASS: {len(packages)} dependency license expressions have allowed selections")


if __name__ == "__main__":
    main()
