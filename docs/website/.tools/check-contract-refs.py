#!/usr/bin/env python3
"""Check repository links and declared implementation references in runtime docs."""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import unquote, urlsplit


REPO_ROOT = Path(__file__).resolve().parents[3]
WEBSITE_DIR = REPO_ROOT / "docs" / "website"

TOP_LEVEL_DOCS = (
    Path("README.md"),
    Path("docs/architecture-overview.md"),
    Path("docs/runtime-spec.md"),
)
AUTHORITATIVE_GLOBS = (
    "docs/website/spec/**/*.md",
    "docs/website/reference/**/*.md",
)
RFC_GLOB = "docs/rfcs/**/*.md"

MARKDOWN_LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
CODE_SPAN_RE = re.compile(r"`([^`\n]+)`")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")
HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*$")
IGNORE_RE = re.compile(r"<!--\s*contract-ref-ignore:\s*(.*?)\s*-->")
IMPLEMENTATION_HEADING_RE = re.compile(
    r"^#{1,6}\s+Implementation references?\s*$", re.IGNORECASE
)
REPOSITORY_PATH_RE = re.compile(
    r"^(?:src|tests|docs|scripts|builtin_templates|agent_templates|web-gui|\.github)/"
    r"[A-Za-z0-9_./-]+$"
)
REPOSITORY_ROOT_FILES = {"Cargo.lock", "Cargo.toml", "Makefile"}
EXTERNAL_SCHEMES = {"http", "https", "mailto", "tel", "data"}


@dataclass(frozen=True)
class Diagnostic:
    source: Path
    line: int
    target: str

    def format(self, repo_root: Path) -> str:
        return f"{self.source.relative_to(repo_root).as_posix()}:{self.line}:{self.target}"


def markdown_files(repo_root: Path) -> list[tuple[Path, bool]]:
    """Return checked files and whether code-span implementation refs are enabled."""
    files: dict[Path, bool] = {}
    for relative in TOP_LEVEL_DOCS:
        path = repo_root / relative
        if path.is_file():
            files[path] = False
    for pattern in AUTHORITATIVE_GLOBS:
        for path in repo_root.glob(pattern):
            if path.is_file():
                files[path] = True
    for path in repo_root.glob(RFC_GLOB):
        if path.is_file():
            files.setdefault(path, False)
    return sorted(files.items())


def strip_link_title(raw_target: str) -> str:
    target = raw_target.strip()
    if target.startswith("<") and ">" in target:
        return target[1 : target.index(">")]
    return target.split(maxsplit=1)[0]


def has_placeholder(path: str) -> bool:
    return any(token in path for token in ("*", "{", "}", "<", ">", "$"))


def candidate_paths(source: Path, path: str, repo_root: Path) -> list[Path]:
    decoded = unquote(path)
    if decoded.startswith("/"):
        base = (
            repo_root / "docs" / "website"
            if source.is_relative_to(repo_root / "docs" / "website")
            else repo_root
        )
        resolved = base / decoded.lstrip("/")
    else:
        resolved = source.parent / decoded

    if decoded.endswith("/"):
        candidates = [resolved / "README.md", resolved / "index.md", resolved]
    elif not resolved.suffix:
        candidates = [
            resolved.with_suffix(".md"),
            resolved / "README.md",
            resolved / "index.md",
            resolved,
        ]
    else:
        candidates = [resolved]
    return candidates


def resolve_target(source: Path, path: str, repo_root: Path) -> Path | None:
    for candidate in candidate_paths(source, path, repo_root):
        if candidate.exists():
            return candidate.resolve()
    return None


def heading_slug(text: str) -> str:
    text = re.sub(r"<[^>]+>", "", text)
    text = re.sub(r"!\[([^\]]*)\]\([^)]+\)", r"\1", text)
    text = re.sub(r"\[([^\]]+)\]\([^)]+\)", r"\1", text)
    text = text.replace("`", "").replace("*", "")
    text = re.sub(r"(?<!\w)_([^_]+)_(?!\w)", r"\1", text)
    text = text.strip().lower()
    text = re.sub(r"[^\w\s-]", "", text, flags=re.UNICODE)
    return re.sub(r"\s+", "-", text)


def anchors_for(path: Path) -> set[str]:
    anchors: set[str] = set()
    slug_counts: dict[str, int] = {}
    in_fence = False
    fence_marker = ""
    for line in path.read_text(encoding="utf-8").splitlines():
        fence = FENCE_RE.match(line)
        if fence:
            marker = fence.group(1)[0]
            if not in_fence:
                in_fence = True
                fence_marker = marker
            elif marker == fence_marker:
                in_fence = False
            continue
        if in_fence:
            continue
        for explicit in re.findall(r'<a\s+(?:name|id)=["\']([^"\']+)["\']', line):
            anchors.add(explicit)
        heading = HEADING_RE.match(line)
        if not heading:
            continue
        base = heading_slug(heading.group(2))
        if not base:
            continue
        count = slug_counts.get(base, 0)
        slug_counts[base] = count + 1
        anchors.add(base if count == 0 else f"{base}-{count}")
    return anchors


def visible_markdown_text(text: str) -> str:
    """Mask fenced blocks while preserving source offsets and line numbers."""
    visible: list[str] = []
    in_fence = False
    fence_marker = ""
    for raw_line in text.splitlines(keepends=True):
        line = raw_line.rstrip("\r\n")
        fence = FENCE_RE.match(line)
        if fence:
            marker = fence.group(1)[0]
            if not in_fence:
                in_fence = True
                fence_marker = marker
            elif marker == fence_marker:
                in_fence = False
            visible.append("".join("\n" if char == "\n" else " " for char in raw_line))
        elif in_fence:
            visible.append("".join("\n" if char == "\n" else " " for char in raw_line))
        else:
            visible.append(raw_line)
    return "".join(visible)


def reasoned_ignore_follows(text: str, end: int) -> bool:
    line_end = text.find("\n", end)
    suffix = text[end : line_end if line_end >= 0 else len(text)]
    ignore = IGNORE_RE.search(suffix)
    return bool(
        ignore
        and ignore.group(1).strip()
        and re.fullmatch(r"[\s.,;:]*", suffix[: ignore.start()])
    )


def repository_reference(token: str) -> str | None:
    target = token.strip().rstrip(".,;:")
    if has_placeholder(target) or "::" in target:
        return None
    if target in REPOSITORY_ROOT_FILES or REPOSITORY_PATH_RE.fullmatch(target):
        return target
    return None


def check_markdown_link(
    source: Path,
    line_number: int,
    raw_target: str,
    repo_root: Path,
    anchor_cache: dict[Path, set[str]],
) -> Diagnostic | None:
    target = strip_link_title(raw_target)
    parsed = urlsplit(target)
    if parsed.scheme.lower() in EXTERNAL_SCHEMES or target.startswith("//"):
        return None
    if has_placeholder(parsed.path):
        return None

    resolved = source.resolve() if not parsed.path else resolve_target(
        source, parsed.path, repo_root
    )
    if resolved is None:
        return Diagnostic(source, line_number, target)

    if parsed.fragment and resolved.suffix.lower() == ".md":
        anchors = anchor_cache.setdefault(resolved, anchors_for(resolved))
        if unquote(parsed.fragment) not in anchors:
            return Diagnostic(source, line_number, target)
    return None


def check_file(
    source: Path,
    repo_root: Path,
    check_declared_refs: bool,
    anchor_cache: dict[Path, set[str]],
) -> list[Diagnostic]:
    diagnostics: list[Diagnostic] = []
    text = source.read_text(encoding="utf-8")
    visible_text = visible_markdown_text(text)
    markdown_matches = list(MARKDOWN_LINK_RE.finditer(visible_text))
    markdown_ranges = [match.span() for match in markdown_matches]

    for match in markdown_matches:
        if reasoned_ignore_follows(text, match.end()):
            continue
        line_number = text.count("\n", 0, match.start()) + 1
        diagnostic = check_markdown_link(
            source, line_number, match.group(1), repo_root, anchor_cache
        )
        if diagnostic:
            diagnostics.append(diagnostic)

    in_fence = False
    fence_marker = ""
    in_last_verified = False
    in_implementation_section = False
    line_start = 0

    for line_number, raw_line in enumerate(text.splitlines(keepends=True), 1):
        line = raw_line.rstrip("\r\n")
        fence = FENCE_RE.match(line)
        if fence:
            marker = fence.group(1)[0]
            if not in_fence:
                in_fence = True
                fence_marker = marker
            elif marker == fence_marker:
                in_fence = False
            line_start += len(raw_line)
            continue
        if in_fence:
            line_start += len(raw_line)
            continue

        if IMPLEMENTATION_HEADING_RE.match(line):
            in_implementation_section = True
        elif in_implementation_section and HEADING_RE.match(line):
            in_implementation_section = False

        if "**Last verified:**" in line:
            in_last_verified = True
        elif in_last_verified and not line.lstrip().startswith(">"):
            in_last_verified = False

        code_matches = list(CODE_SPAN_RE.finditer(line))

        if check_declared_refs and (in_last_verified or in_implementation_section):
            for match in code_matches:
                start = line_start + match.start()
                end = line_start + match.end()
                if any(
                    link_start <= start and end <= link_end
                    for link_start, link_end in markdown_ranges
                ):
                    continue
                if reasoned_ignore_follows(text, end):
                    continue
                target = repository_reference(match.group(1))
                if target and not (repo_root / target).exists():
                    diagnostics.append(Diagnostic(source, line_number, target))

        line_start += len(raw_line)

    return diagnostics


def check_repository(repo_root: Path = REPO_ROOT) -> list[Diagnostic]:
    anchor_cache: dict[Path, set[str]] = {}
    diagnostics: list[Diagnostic] = []
    for source, check_declared_refs in markdown_files(repo_root):
        diagnostics.extend(
            check_file(source, repo_root, check_declared_refs, anchor_cache)
        )
    return diagnostics


def main(repo_root: Path | None = None) -> int:
    repo_root = repo_root or REPO_ROOT
    diagnostics = check_repository(repo_root)
    for diagnostic in diagnostics:
        print(diagnostic.format(repo_root))
    if diagnostics:
        print(f"FOUND {len(diagnostics)} BROKEN CONTRACT REFERENCE(S)")
        return 1
    print("ALL CONTRACT REFERENCES OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
