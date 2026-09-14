#!/usr/bin/env python3
"""Regression tests for directory catalog ownership and URL normalization."""

import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "check_directories", Path(__file__).with_name("check-directories.py")
)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
PAGE = Path("zh-CN/guides/README.md")
INDEX = "\n<!-- INDEX:START -->\n- [Guide](./work-items.md)\n<!-- INDEX:END -->\n"


class DirectoryChecks(unittest.TestCase):
    def test_single_catalog_with_cross_section_link(self):
        self.assertEqual(
            checker.check_page("[Concepts](/zh-CN/concepts/)" + INDEX, PAGE), []
        )

    def test_manual_route_and_markdown_links_duplicate_catalog(self):
        for href in (
            "/zh-CN/guides/work-items",
            "/zh-CN/guides/work-items.md",
            "./work-items.md#example",
            "work-items/?view=full",
        ):
            with self.subTest(href=href):
                errors = checker.check_page(f"- [Manual]({href})" + INDEX, PAGE)
                self.assertTrue(any("manual link repeats" in e for e in errors))

    def test_duplicate_inside_catalog(self):
        text = INDEX.replace("<!-- INDEX:END -->", "- [Again](work-items)\n<!-- INDEX:END -->")
        self.assertTrue(any("duplicate catalog" in e for e in checker.check_page(text, PAGE)))

    def test_link_after_catalog_is_also_checked(self):
        self.assertTrue(checker.check_page(INDEX + "[Again](work-items.md)", PAGE))

    def test_missing_empty_or_reversed_markers(self):
        for text in ("", checker.START + checker.END, checker.END + checker.START):
            with self.subTest(text=text):
                self.assertTrue(checker.check_page(text, PAGE))


if __name__ == "__main__":
    unittest.main()
