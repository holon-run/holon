#!/usr/bin/env python3
"""Check the bilingual Blog and its home-page entry points before building."""

from html.parser import HTMLParser
from pathlib import Path
import re
import sys
from urllib.parse import unquote, urlsplit
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parent.parent
ARTICLES = {
    "what-is-holon",
    "why-work-items",
    "one-pr-one-work-item",
    "agents-in-a-small-team",
}
CJK = re.compile(r"[\u3400-\u9fff]")
DRAFT = re.compile(
    r"translation pending|Chinese (?:review )?draft|awaiting review|content placeholder"
    r"|中文审阅稿|待审阅|英文暂留占位|公开范围待确认|内容占位",
    re.IGNORECASE,
)
errors = []
if (ROOT / "dist").exists():
    errors.append("Move legacy docs/website/dist out of the content tree; build output belongs in .tools/dist.")


class References(HTMLParser):
    def __init__(self):
        super().__init__()
        self.targets = []

    def handle_starttag(self, tag, attrs):
        for name, value in attrs:
            if value and name in ("href", "src"):
                self.targets.append(value)
            elif value and name == "srcset":
                self.targets.extend(item.strip().split()[0] for item in value.split(","))


def resolve(page, target):
    url = urlsplit(target)
    if url.scheme or url.netloc or not url.path:
        return None
    path = unquote(url.path)
    resolved = ROOT / path.lstrip("/") if path.startswith("/") else page.parent / path
    if not resolved.suffix:
        candidates = (resolved / "README.md", resolved.with_suffix(".md"))
        return next((candidate for candidate in candidates if candidate.is_file()), resolved)
    return resolved


for locale in ("", "zh-CN"):
    base = ROOT / locale
    blog = base / "blog"
    actual = {page.stem for page in blog.glob("*.md")} - {"README"}
    if actual != ARTICLES:
        errors.append(f"{blog.relative_to(ROOT)}: unexpected article set {sorted(actual)}")

    pages = [base / "README.md", blog / "README.md"]
    pages.extend(blog / f"{slug}.md" for slug in sorted(ARTICLES))
    for page in pages:
        if not page.is_file():
            errors.append(f"Missing {page.relative_to(ROOT)}")
            continue
        text = page.read_text()
        if DRAFT.search(text):
            errors.append(f"{page.relative_to(ROOT)}: draft or placeholder copy")
        if not locale and CJK.search(text):
            errors.append(f"{page.relative_to(ROOT)}: untranslated Chinese text")
        parser = References()
        parser.feed(text)
        # Markdown images and links, including links without an .md suffix.
        targets = parser.targets + re.findall(r"\[[^\]]*\]\(([^)\s]+)\)", text)
        for target in targets:
            resolved = resolve(page, target)
            if resolved is None:
                continue
            if not resolved.exists():
                errors.append(f"{page.relative_to(ROOT)}: missing target {target}")
            elif not locale and resolved.suffix == ".svg":
                svg_text = " ".join(ET.parse(resolved).getroot().itertext())
                if CJK.search(svg_text):
                    errors.append(f"{page.relative_to(ROOT)}: untranslated diagram {target}")
            elif not locale and re.search(r"-zh(?:-narrow)?\.", resolved.name):
                errors.append(f"{page.relative_to(ROOT)}: Chinese asset {target}")
        if page.name == "README.md":
            linked = {
                Path(urlsplit(target).path).name.removesuffix(".md")
                for target in parser.targets
            }
            if not ARTICLES <= linked:
                errors.append(f"{page.relative_to(ROOT)}: missing article entry points")

if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
print("Blog checks passed: four articles per locale, translated copy, assets, and entry points.")
