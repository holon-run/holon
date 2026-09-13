#!/usr/bin/env python3
"""Keep machine-generated site pages in sync across locales.

Some site pages are produced by a generator rather than written by hand. Today
that is `reference/models.md`, generated with:

    cargo run --bin holon-docgen -- models > docs/website/reference/models.md

Translating such a page by hand is wasted work: the next regeneration replaces
the file. Instead, register the page in `.tools/generated-pages.json` and let
this script copy the generated English page to each configured locale path,
override the listed front matter fields with localized values, and prepend a
notice that explains the page is generated.

Usage (any cwd; the script resolves the site root from its own location):

    python3 docs/website/.tools/sync-generated-pages.py          # write copies
    python3 docs/website/.tools/sync-generated-pages.py --check  # fail if stale

CI runs the `--check` form, so a regeneration needs one extra sync command
instead of a new translation.
"""

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
WEBSITE_DIR = os.path.dirname(HERE)
MANIFEST_PATH = os.path.join(HERE, "generated-pages.json")


def split_front_matter(text):
    if not text.startswith("---\n"):
        raise ValueError("missing front matter")
    end = text.find("\n---\n", 4)
    if end == -1:
        raise ValueError("unterminated front matter")
    return text[4:end], text[end + len("\n---\n"):]


def apply_overrides(front_matter, overrides):
    lines = front_matter.split("\n")
    seen = set()
    result = []
    for line in lines:
        key = line.split(":", 1)[0].strip() if ":" in line else None
        if key in overrides:
            result.append(f"{key}: {overrides[key]}")
            seen.add(key)
        else:
            result.append(line)
    for key, value in overrides.items():
        if key not in seen:
            result.append(f"{key}: {value}")
    return "\n".join(result)


def render(source_text, target):
    front_matter, body = split_front_matter(source_text)
    front_matter = apply_overrides(front_matter, target.get("frontMatter", {}))
    parts = ["---", front_matter, "---"]
    notice = target.get("notice", "").strip()
    if notice:
        parts.extend(["", notice])
    parts.extend(["", body.lstrip("\n")])
    text = "\n".join(parts)
    if not text.endswith("\n"):
        text += "\n"
    return text


def read(path):
    if not os.path.exists(path):
        return None
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def main(argv):
    check_only = "--check" in argv
    with open(MANIFEST_PATH, encoding="utf-8") as handle:
        manifest = json.load(handle)

    written = []
    stale = []
    for page in manifest["pages"]:
        source_path = os.path.join(WEBSITE_DIR, page["source"])
        source_text = read(source_path)
        if source_text is None:
            print(f"  MISSING SOURCE: {page['source']}")
            return 1
        for target in page["targets"]:
            dest_rel = target["path"]
            dest_path = os.path.join(WEBSITE_DIR, dest_rel)
            expected = render(source_text, target)
            if read(dest_path) == expected:
                continue
            if check_only:
                stale.append(dest_rel)
            else:
                os.makedirs(os.path.dirname(dest_path), exist_ok=True)
                with open(dest_path, "w", encoding="utf-8") as handle:
                    handle.write(expected)
                written.append(dest_rel)

    if stale:
        print("Generated page copies are out of date:")
        for path in stale:
            print(f"  STALE: {path}")
        print("Run: python3 docs/website/.tools/sync-generated-pages.py")
        return 1

    if written:
        print("Synced generated page copies:")
        for path in written:
            print(f"  WROTE: {path}")
    else:
        print("Generated page copies are in sync.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
