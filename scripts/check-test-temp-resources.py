#!/usr/bin/env python3

from __future__ import annotations

import re
import sys
from collections import Counter
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST = ROOT / "tests" / "test-temp-resource-allowlist.txt"
PERMANENT_TEMP_DIR = re.compile(r"\.(?:keep|into_path)\s*\(")


def load_allowlist() -> dict[str, tuple[int, str]]:
    entries: dict[str, tuple[int, str]] = {}
    for line_number, raw_line in enumerate(
        ALLOWLIST.read_text(encoding="utf-8").splitlines(), start=1
    ):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t", 2)
        if len(parts) != 3:
            raise ValueError(
                f"{ALLOWLIST.relative_to(ROOT)}:{line_number}: "
                "expected PATH<TAB>COUNT<TAB>REASON"
            )
        path, count_text, reason = parts
        if path in entries:
            raise ValueError(f"duplicate allowlist path: {path}")
        entries[path] = (int(count_text), reason)
    return entries


def find_permanent_temp_dirs() -> Counter[str]:
    counts: Counter[str] = Counter()
    for path in sorted((ROOT / "tests").rglob("*.rs")):
        relative = path.relative_to(ROOT).as_posix()
        for line in path.read_text(encoding="utf-8").splitlines():
            if PERMANENT_TEMP_DIR.search(line):
                counts[relative] += 1
    return counts


def main() -> int:
    try:
        expected = load_allowlist()
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1

    actual = find_permanent_temp_dirs()
    failures: list[str] = []
    for path in sorted(set(expected) | set(actual)):
        expected_count = expected.get(path, (0, ""))[0]
        actual_count = actual.get(path, 0)
        if actual_count != expected_count:
            failures.append(
                f"{path}: expected {expected_count} permanent temp-dir call(s), "
                f"found {actual_count}"
            )

    if failures:
        print("test temp-resource audit failed:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        print(
            "Use RAII-owned TempDir values, or update the allowlist count and reason "
            "when permanent retention is unavoidable.",
            file=sys.stderr,
        )
        return 1

    print(
        f"test temp-resource audit passed "
        f"({sum(actual.values())} allowlisted permanent call(s))"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
