import datetime
import subprocess
import unittest
from unittest.mock import patch

import build_version as bv


def unix(*parts):
    """UTC Unix time for an explicit UTC wall-clock moment."""
    return int(datetime.datetime(*parts, tzinfo=datetime.timezone.utc).timestamp())


class BuildVersion(unittest.TestCase):
    def test_examples_from_spec(self):
        stamp = unix(2026, 11, 10, 9, 49)  # 17:49 Beijing
        self.assertEqual(bv.display_version(stamp), "2026.11.10-1749")
        self.assertEqual(bv.bundle_short_version(stamp), "2026.11.10")
        self.assertEqual(bv.bundle_build_number(stamp), "20261110.1749")
        stamp = unix(2026, 11, 10, 1, 5)  # 09:05 Beijing
        self.assertEqual(bv.display_version(stamp), "2026.11.10-0905")
        self.assertEqual(bv.bundle_build_number(stamp), "20261110.905")

    def test_utc_evening_rolls_into_next_beijing_day(self):
        stamp = unix(2026, 12, 31, 16, 0)  # 2027-01-01 00:00 Beijing
        self.assertEqual(bv.display_version(stamp), "2027.1.1-0000")
        self.assertEqual(bv.bundle_short_version(stamp), "2027.1.1")
        self.assertEqual(bv.bundle_build_number(stamp), "20270101.0")

    def test_just_before_beijing_midnight_stays_on_same_day(self):
        stamp = unix(2026, 3, 9, 15, 59)  # 2026-03-09 23:59 Beijing
        self.assertEqual(bv.display_version(stamp), "2026.3.9-2359")
        self.assertEqual(bv.bundle_build_number(stamp), "20260309.2359")

    def test_unpadded_month_and_day_but_padded_time(self):
        stamp = unix(2026, 2, 3, 20, 7)  # 2026-02-04 04:07 Beijing
        self.assertEqual(bv.display_version(stamp), "2026.2.4-0407")
        self.assertEqual(bv.bundle_versions_from_display("2026.2.4-0407"),
                         ("2026.2.4", "20260204.407"))

    def test_display_and_bundle_agree(self):
        stamp = unix(2026, 7, 8, 23, 30)
        self.assertEqual(bv.bundle_versions_from_display(bv.display_version(stamp)),
                         (bv.bundle_short_version(stamp), bv.bundle_build_number(stamp)))

    def test_validation_accepts_canonical_values(self):
        for value in ("2026.11.10-1749", "2026.1.1-0000", "2028.2.29-2359"):
            self.assertEqual(bv.validate_display_version(value), value)

    def test_validation_rejects_malformed_input(self):
        for value in ("", "0.3.23-dev", "2026.11.10", "2026.11.10-749", "2026.11.10-17490",
                      "2026.01.10-1749", "2026.11.01-1749", "2026.13.1-1749",
                      "2026.2.30-1749", "2027.2.29-1749", "2026.11.10-2400",
                      "2026.11.10-1760", "2026.11.10-17:49", " 2026.11.10-1749",
                      "2026.11.10-1749\n", "v2026.11.10-1749", "26.11.10-1749", None, 20261110):
            with self.subTest(value=value), self.assertRaises(ValueError):
                bv.validate_display_version(value)

    def test_commit_timestamp_reads_git_and_rejects_bad_output(self):
        ok = subprocess.CompletedProcess([], 0, stdout="1700000000\n", stderr="")
        with patch.object(bv.subprocess, "run", return_value=ok) as run:
            self.assertEqual(bv.commit_timestamp("/repo"), 1700000000)
            self.assertEqual(run.call_args[0][0][:3], ["git", "-C", "/repo"])
        bad = subprocess.CompletedProcess([], 0, stdout="not-a-time\n", stderr="")
        with patch.object(bv.subprocess, "run", return_value=bad):
            with self.assertRaises(ValueError):
                bv.commit_timestamp("/repo")


if __name__ == "__main__":
    unittest.main()
