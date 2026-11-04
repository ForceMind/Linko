"""Regressions for license selection and complete bundled font notices."""
import json
import pathlib
import tempfile
import unittest
from unittest.mock import patch
import third_party


class Notices(unittest.TestCase):
    def test_embedded_chinese_font_notice_is_included_without_cargo_package(self):
        with tempfile.TemporaryDirectory() as folder:
            out = pathlib.Path(folder) / "notices"
            with patch("sys.argv", ["third_party.py", "--notices", str(out)]), patch("subprocess.check_output", return_value=b'{"packages":[]}'):
                third_party.main()
            target = out / "bundled-font-NotoSansSC"
            self.assertEqual((target / "OFL.txt").read_bytes(), (third_party.BUNDLED_FONT / "OFL.txt").read_bytes())
            self.assertTrue((target / "SOURCE.md").is_file())
            self.assertIn("Noto Sans SC", (out / "INVENTORY.md").read_text())

    def test_and_keeps_all_required_terms(self):
        self.assertIsNone(third_party.select("MIT AND GPL-3.0-only"))
        self.assertEqual(third_party.select("GPL-3.0-only OR MIT"), "MIT")
        self.assertEqual(third_party.select("(MIT OR Apache-2.0) AND OFL-1.1"), "MIT AND OFL-1.1")

    def test_font_notices_are_not_omitted_when_one_license_is_found(self):
        with tempfile.TemporaryDirectory() as folder:
            base = pathlib.Path(folder)
            source = base / "source"
            fonts = source / "fonts"
            fonts.mkdir(parents=True)
            names = ["OFL.txt", "UFL.txt", "Hack-Regular.txt", "egui-icons.txt", "emoji-mit-license.txt"]
            for name in names:
                (fonts / name).write_text("notice for " + name, encoding="utf-8")
            (source / "Cargo.toml").write_text("", encoding="utf-8")
            data = {"packages": [{"name": "test-fonts", "version": "1.0.0", "source": "registry", "license": "MIT AND OFL-1.1 AND Ubuntu-font-1.0", "manifest_path": str(source / "Cargo.toml")}]}
            out = base / "notices"
            with patch("sys.argv", ["third_party.py", "--notices", str(out)]), patch("subprocess.check_output", return_value=json.dumps(data).encode()):
                third_party.main()
            for name in names:
                self.assertEqual((out / "test-fonts-1.0.0" / "fonts" / name).read_text(), "notice for " + name)


if __name__ == "__main__":
    unittest.main()
