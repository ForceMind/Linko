#!/usr/bin/env python3
"""Derive the build version from the HEAD commit time in fixed UTC+08:00.

The same commit yields the same version on every platform: no zoneinfo, tzdata
or local clock is consulted. Output formats:

* display / package version: ``YYYY.M.D-HHMM`` (month/day unpadded, time padded)
* macOS CFBundleShortVersionString: ``YYYY.M.D``
* macOS CFBundleVersion: ``YYYYMMDD.<H*100+M>`` (no leading zeros after the dot)
"""
import datetime
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BEIJING = datetime.timezone(datetime.timedelta(hours=8))
_DISPLAY = re.compile(r"^([1-9][0-9]{3})\.([1-9][0-9]?)\.([1-9][0-9]?)-([0-9]{2})([0-9]{2})$")


def beijing_time(timestamp):
    return datetime.datetime.fromtimestamp(int(timestamp), BEIJING)


def display_version(timestamp):
    moment = beijing_time(timestamp)
    return f"{moment.year}.{moment.month}.{moment.day}-{moment.hour:02d}{moment.minute:02d}"


def parse_display_version(text):
    """Return (year, month, day, hour, minute) or raise ValueError."""
    match = _DISPLAY.fullmatch(text) if isinstance(text, str) else None
    if match is None:
        raise ValueError("version must look like YYYY.M.D-HHMM")
    year, month, day, hour, minute = (int(part) for part in match.groups())
    datetime.date(year, month, day)  # rejects impossible calendar dates
    if hour > 23 or minute > 59:
        raise ValueError("version time must be 0000-2359")
    return year, month, day, hour, minute


def validate_display_version(text):
    parse_display_version(text)
    return text


def bundle_short_version(timestamp):
    moment = beijing_time(timestamp)
    return f"{moment.year}.{moment.month}.{moment.day}"


def bundle_build_number(timestamp):
    moment = beijing_time(timestamp)
    return f"{moment.year:04d}{moment.month:02d}{moment.day:02d}.{moment.hour * 100 + moment.minute}"


def bundle_versions_from_display(text):
    """(CFBundleShortVersionString, CFBundleVersion) for a validated display version."""
    year, month, day, hour, minute = parse_display_version(text)
    return (f"{year}.{month}.{day}", f"{year:04d}{month:02d}{day:02d}.{hour * 100 + minute}")


def commit_timestamp(root=ROOT):
    """HEAD commit Unix time from git; raises OSError/CalledProcessError/ValueError."""
    result = subprocess.run(["git", "-C", str(root), "log", "-1", "--format=%ct"],
                            check=True, capture_output=True, text=True)
    text = result.stdout.strip()
    if not re.fullmatch(r"[0-9]{1,12}", text):
        raise ValueError("git did not return a commit timestamp")
    return int(text)


def main():
    try:
        line = "LINKO_VERSION=" + display_version(commit_timestamp()) + "\n"
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"cannot derive build version from git: {error}", file=sys.stderr)
        return 1
    # Bytes, not text: Windows must not turn the newline into CRLF inside $GITHUB_ENV.
    sys.stdout.buffer.write(line.encode("ascii"))
    sys.stdout.buffer.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
