from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from bridge import automation_api
from macro_io import parser


class ReleasePathTests(unittest.TestCase):
    def test_macro_paths_stay_in_macro_root(self):
        with TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            macro_root = base / "宏"
            with patch.object(parser, "BASE_DIR", base), patch.object(parser, "MACRO_ROOT", macro_root):
                self.assertEqual(parser.macro_abs_from_rel("宏/示例.json"), macro_root / "示例.json")
                for path in ("配置.json", "宏/../配置.json", "宏/示例.json:其他数据"):
                    with self.subTest(path=path), self.assertRaises(ValueError):
                        parser.macro_abs_from_rel(path)

    def test_macro_creation_accepts_only_a_filename(self):
        self.assertEqual(parser.ensure_json_suffix("示例"), "示例.json")
        for path in ("../示例", "目录/示例", "目录\\示例", "示例.json:其他数据"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                parser.ensure_json_suffix(path)

    def test_graph_path_preserves_relative_and_existing_absolute_paths(self):
        with TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            script_root = base / "自动化脚本"
            expected = script_root / "示例.auto.json"
            with (patch.object(automation_api, "BASE_DIR", base),
                  patch.object(automation_api, "AUTO_SCRIPT_ROOT", script_root)):
                self.assertEqual(automation_api._auto_script_path("自动化脚本/示例.auto.json"), expected)
                self.assertEqual(automation_api._auto_script_path(str(expected)), expected)
                for path in ("自动化脚本/../示例.auto.json", "自动化脚本/示例.json"):
                    with self.subTest(path=path), self.assertRaises(ValueError):
                        automation_api._auto_script_path(path)
