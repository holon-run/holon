#!/usr/bin/env python3
"""Lightweight internal markdown link checker for Holon docs.

Checks README.md and docs/website/ for broken relative/absolute .md links.
Site-root-absolute links (starting with /) in docs/website/ resolve relative
to the website content root (docs/website/), matching mdorigin behavior.

Run from the repository root:
    python3 docs/website/.tools/check-links.py
"""

import os, re, sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
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
            for m in LINK_RE.finditer(line):
                target = m.group(2).split('#')[0].split('?')[0]
                if target.startswith('http'):
                    continue
                if target.startswith('/'):
                    resolved = os.path.normpath(os.path.join(root_for_absolute, target.lstrip('/')))
                else:
                    resolved = os.path.normpath(os.path.join(curdir, target))
                if not os.path.exists(resolved):
                    rel = os.path.relpath(fpath, REPO_ROOT)
                    print(f"  BROKEN: {rel}:{i}: {m.group(2)}")
                    broken += 1

print("=== Holon docs link check ===")

# Top-level docs resolve relative to repo root
for f in ["README.md", "docs/architecture-overview.md", "docs/runtime-spec.md"]:
    fp = os.path.join(REPO_ROOT, f)
    check_file(fp, REPO_ROOT)

# Website docs resolve site-root-absolute links relative to docs/website/
for root, dirs, files in os.walk(WEBSITE_DIR):
    if '.tools/node_modules' in root:
        continue
    dirs[:] = [d for d in dirs if d != 'node_modules']
    for f in files:
        if f.endswith('.md'):
            check_file(os.path.join(root, f), WEBSITE_DIR)

print()
if broken == 0:
    print("ALL LINKS OK")
    sys.exit(0)
else:
    print(f"FOUND {broken} BROKEN LINK(S)")
    sys.exit(1)
