#!/usr/bin/env python3
"""Keep documentation directory links in one managed catalog per page."""

from collections import Counter
from pathlib import Path
import posixpath
import re
import sys
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent.parent
SECTIONS = ("getting-started", "concepts", "guides", "reference", "spec", "maintainers")
START = "<!-- INDEX:START -->"
END = "<!-- INDEX:END -->"
LINK = re.compile(r"\[[^\]\n]+\]\(([^)\s]+)\)")


def targets(text, page):
    for href in LINK.findall(text):
        url = urlsplit(href)
        if url.scheme or url.netloc or not url.path:
            continue
        path = unquote(url.path)
        if not path.startswith("/"):
            path = posixpath.join("/", str(page.parent), path)
        path = posixpath.normpath(path)
        if path.endswith(".md"):
            path = path[:-3]
        if path.endswith("/README"):
            path = path[:-7]
        yield path


def check_page(text, page):
    if text.count(START) != 1 or text.count(END) != 1:
        return ["expected exactly one managed index block"]
    before, _, rest = text.partition(START)
    index, _, after = rest.partition(END)
    if END in before:
        return ["managed index markers are out of order"]
    catalog = Counter(targets(index, page))
    errors = []
    if not catalog:
        errors.append("managed catalog is empty")
    for target, count in catalog.items():
        if count > 1:
            errors.append(f"duplicate catalog entry: {target}")
    for target in sorted(set(targets(before + after, page)) & catalog.keys()):
        errors.append(f"manual link repeats managed catalog entry: {target}")
    return errors


def main():
    errors = []
    for locale in ("", "zh-CN"):
        for section in SECTIONS:
            page = Path(locale) / section / "README.md"
            for error in check_page((ROOT / page).read_text(), page):
                errors.append(f"{page}: {error}")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(
        f"Documentation directories: {len(SECTIONS) * 2} pages have a single, "
        "non-duplicated catalog."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
