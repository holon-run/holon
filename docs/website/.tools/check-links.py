#!/usr/bin/env python3
"""Lightweight internal link checker for Holon docs website.

Checks README.md and docs/website/ for broken relative/absolute .md links.
Site-root-absolute links (starting with /) in docs/website/ resolve relative
to the website content root (docs/website/), matching mdorigin behavior.

Also validates navigation links in mdorigin.config.json and catches
trailing-slash mismatches (file-page URLs must not end with /).

Run from the repository root:
    python3 docs/website/.tools/check-links.py
"""

import json
import os
import re
import sys

REPO_ROOT = os.path.dirname(
    os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
)
WEBSITE_DIR = os.path.join(REPO_ROOT, "docs/website")
LINK_RE = re.compile(r'\[([^\]]*)\]\(([^)]*\.md[^)]*)\)')

broken = 0


def check_file(fpath, root_for_absolute):
    global broken
    if not os.path.exists(fpath):
        return
    curdir = os.path.dirname(fpath)
    with open(fpath) as fh:
        for i, line in enumerate(fh, 1):
            check_line(fpath, curdir, root_for_absolute, i, line)


def check_line(fpath, curdir, root_for_absolute, lineno, line):
    global broken
    for m in LINK_RE.finditer(line):
        target = m.group(2).split('#')[0].split('?')[0]
        if target.startswith('http'):
            continue
        if target.startswith('/'):
            resolved = os.path.normpath(
                os.path.join(root_for_absolute, target.lstrip('/'))
            )
        else:
            resolved = os.path.normpath(os.path.join(curdir, target))
        if not os.path.exists(resolved):
            rel = os.path.relpath(fpath, REPO_ROOT)
            print(f"  BROKEN: {rel}:{lineno}: {m.group(2)}")
            broken += 1


def collect_dir_pages():
    """Return set of URL paths for directory-based pages (those with README.md/index.md)."""
    dirs = set()
    for root, subdirs, files in os.walk(WEBSITE_DIR):
        if '.tools/node_modules' in root:
            continue
        subdirs[:] = [d for d in subdirs if d != 'node_modules']
        for f in files:
            if f in ('README.md', 'index.md'):
                rel = os.path.relpath(root, WEBSITE_DIR).replace('\\', '/')
                dirs.add('/' if rel == '.' else '/' + rel + '/')
    return dirs


def check_trailing_slashes():
    """Catch file-page URLs that incorrectly end with /."""
    global broken
    dir_pages = collect_dir_pages()
    TRAILING_LINK_RE = re.compile(r'\[([^\]]*)\]\((/[^)]*)\)')
    for root, subdirs, files in os.walk(WEBSITE_DIR):
        if '.tools/node_modules' in root:
            continue
        subdirs[:] = [d for d in subdirs if d != 'node_modules']
        for f in files:
            if not f.endswith('.md'):
                continue
            fpath = os.path.join(root, f)
            with open(fpath) as fh:
                for i, line in enumerate(fh, 1):
                    for m in TRAILING_LINK_RE.finditer(line):
                        url = m.group(2)
                        clean = url.split('#')[0].split('?')[0]
                        if clean.endswith('/') and clean not in dir_pages:
                            rel = os.path.relpath(fpath, REPO_ROOT)
                            print(
                                f"  TRAILING-SLASH: {rel}:{i}: {m.group(2)} "
                                f"(should be {m.group(2).rstrip('/')})"
                            )
                            broken += 1


def check_nav_config():
    """Check navigation links in mdorigin.config.json."""
    global broken
    config_path = os.path.join(WEBSITE_DIR, 'mdorigin.config.json')
    if not os.path.exists(config_path):
        return
    dir_pages = collect_dir_pages()
    with open(config_path) as f:
        config = json.load(f)
    for nav in config.get('topNav', []):
        href = nav['href']
        clean = href.split('#')[0].split('?')[0]
        if clean in dir_pages:
            continue
        print(f"  BROKEN-NAV: {href} ({nav['label']})")
        broken += 1


print("=== Holon docs link check ===")

# Top-level docs resolve relative to repo root
for f in ["README.md", "docs/architecture-overview.md", "docs/runtime-spec.md"]:
    fp = os.path.join(REPO_ROOT, f)
    check_file(fp, REPO_ROOT)

# Website docs resolve site-root-absolute links relative to docs/website/
for root, dirs, files in os.walk(WEBSITE_DIR):
    if '.tools/node_modules' in root:
        dirs[:] = []
        continue
    dirs[:] = [d for d in dirs if d != 'node_modules']
    for f in files:
        if f.endswith('.md'):
            check_file(os.path.join(root, f), WEBSITE_DIR)

print("=== Trailing-slash check ===")
check_trailing_slashes()
print("=== Navigation config check ===")
check_nav_config()

print()
if broken == 0:
    print("ALL LINKS OK")
    sys.exit(0)
else:
    print(f"FOUND {broken} BROKEN LINK(S)")
    sys.exit(1)
