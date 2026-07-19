#!/usr/bin/env python3
"""Unit tests for check-contract-refs.py."""

import importlib.util
import io
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path


SCRIPT = Path(__file__).with_name("check-contract-refs.py")
SPEC = importlib.util.spec_from_file_location("check_contract_refs", SCRIPT)
CHECKER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CHECKER
SPEC.loader.exec_module(CHECKER)


class ContractReferenceCheckerTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)
        (self.root / "docs/website/spec").mkdir(parents=True)
        (self.root / "docs/website/reference").mkdir(parents=True)
        (self.root / "docs/rfcs").mkdir(parents=True)
        (self.root / "src/runtime").mkdir(parents=True)

    def tearDown(self):
        self.tempdir.cleanup()

    def write(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def diagnostics(self):
        return CHECKER.check_repository(self.root)

    def test_relative_site_root_and_anchor_links(self):
        self.write(
            "docs/website/reference/target.md",
            "# Target heading\n\n## Repeated\n\n## Repeated\n",
        )
        self.write(
            "docs/website/spec/source.md",
            "\n".join(
                (
                    "[relative](../reference/target.md#target-heading)",
                    "[site root](/reference/target#repeated-1)",
                )
            ),
        )
        self.assertEqual(self.diagnostics(), [])

    def test_multiline_markdown_link_is_checked(self):
        self.write(
            "docs/website/spec/source.md",
            "[broken\nlink](missing.md)\n",
        )
        diagnostics = self.diagnostics()
        self.assertEqual(
            [(item.line, item.target) for item in diagnostics],
            [(1, "missing.md")],
        )

    def test_directory_route_fragment_resolves_index_page(self):
        self.write("docs/website/concepts/README.md", "# Present\n")
        self.write(
            "docs/website/spec/source.md",
            "[missing anchor](/concepts/#missing)\n",
        )
        diagnostics = self.diagnostics()
        self.assertEqual(
            [(item.line, item.target) for item in diagnostics],
            [(1, "/concepts/#missing")],
        )

    def test_heading_slug_preserves_word_underscore(self):
        self.write("docs/website/reference/target.md", "# foo_bar\n")
        self.write(
            "docs/website/spec/source.md",
            "[anchor](../reference/target.md#foo_bar)\n",
        )
        self.assertEqual(self.diagnostics(), [])

    def test_missing_path_and_anchor_report_exact_lines(self):
        source = self.write(
            "docs/website/spec/source.md",
            "[missing](missing.md)\n[anchor](#not-present)\n",
        )
        diagnostics = self.diagnostics()
        self.assertEqual(
            [(item.line, item.target) for item in diagnostics],
            [(1, "missing.md"), (2, "#not-present")],
        )
        self.assertEqual(
            diagnostics[0].format(self.root),
            f"{source.relative_to(self.root).as_posix()}:1:missing.md",
        )

    def test_last_verified_checks_only_repository_paths(self):
        self.write("src/runtime/current.rs", "")
        self.write(
            "docs/website/spec/source.md",
            "\n".join(
                (
                    "> **Last verified:** against `src/runtime/current.rs`,",
                    "> `AgentStatus`, and `src/runtime/missing.rs`.",
                    "",
                    "Ordinary example: `src/runtime/not-checked.rs`.",
                )
            ),
        )
        diagnostics = self.diagnostics()
        self.assertEqual(
            [(item.line, item.target) for item in diagnostics],
            [(2, "src/runtime/missing.rs")],
        )

    def test_fenced_examples_placeholders_and_rfc_code_spans_are_ignored(self):
        self.write(
            "docs/website/spec/source.md",
            "\n".join(
                (
                    "```markdown",
                    "[example](missing.md)",
                    "```",
                    "> **Last verified:** `src/runtime/{future}.rs`.",
                )
            ),
        )
        self.write(
            "docs/rfcs/proposed.md",
            "Proposed path: `src/runtime/future.rs`.\n",
        )
        self.assertEqual(self.diagnostics(), [])

    def test_reasoned_line_ignore_suppresses_reference(self):
        self.write(
            "docs/website/spec/source.md",
            (
                "> **Last verified:** `src/runtime/future.rs` "
                "<!-- contract-ref-ignore: proposed source split -->\n"
            ),
        )
        self.assertEqual(self.diagnostics(), [])

    def test_reasoned_ignore_suppresses_only_adjacent_reference(self):
        self.write(
            "docs/website/spec/source.md",
            (
                "> **Last verified:** `src/runtime/missing.rs` and "
                "`src/runtime/future.rs` "
                "<!-- contract-ref-ignore: proposed source split -->\n"
            ),
        )
        diagnostics = self.diagnostics()
        self.assertEqual(
            [(item.line, item.target) for item in diagnostics],
            [(1, "src/runtime/missing.rs")],
        )

    def test_blank_ignore_reason_does_not_suppress_reference(self):
        self.write(
            "docs/website/spec/source.md",
            (
                "> **Last verified:** `src/runtime/future.rs` "
                "<!-- contract-ref-ignore: -->\n"
            ),
        )
        diagnostics = self.diagnostics()
        self.assertEqual(len(diagnostics), 1)
        self.assertEqual(diagnostics[0].target, "src/runtime/future.rs")

    def test_declared_refs_cover_ci_and_root_contract_files(self):
        self.write(
            "docs/website/spec/source.md",
            (
                "## Implementation references\n\n"
                "- `.github/workflows/missing.yml`\n"
                "- `Cargo.toml`\n"
            ),
        )
        diagnostics = self.diagnostics()
        self.assertEqual(
            [(item.line, item.target) for item in diagnostics],
            [(3, ".github/workflows/missing.yml"), (4, "Cargo.toml")],
        )

    def test_code_formatted_link_is_checked_once(self):
        self.write(
            "docs/website/spec/source.md",
            "## Implementation references\n\n"
            "[`src/runtime/missing.rs`](missing.md)\n",
        )
        diagnostics = self.diagnostics()
        self.assertEqual(
            [(item.line, item.target) for item in diagnostics],
            [(3, "missing.md")],
        )

    def test_main_prints_machine_locatable_diagnostic(self):
        self.write("docs/website/spec/source.md", "[missing](missing.md)\n")
        output = io.StringIO()
        with redirect_stdout(output):
            status = CHECKER.main(self.root)
        self.assertEqual(status, 1)
        self.assertIn(
            "docs/website/spec/source.md:1:missing.md", output.getvalue()
        )


if __name__ == "__main__":
    unittest.main()
