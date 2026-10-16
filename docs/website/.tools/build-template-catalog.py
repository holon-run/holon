#!/usr/bin/env python3
"""Render the official template snapshot into an isolated mdorigin content root.

Requires Python 3.11+. Reads repository data only; never fetches or installs skills.
"""

from __future__ import annotations

import html
import importlib.util
import json
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile


REPO = Path(__file__).resolve().parents[3]
START = "<!-- TEMPLATES:START -->"
END = "<!-- TEMPLATES:END -->"
GITHUB = "https://github.com/holon-run/holon"
CATEGORIES = {
    "engineering": ("Engineering collaboration", "工程协作"),
    "content": ("Content and knowledge", "内容与知识"),
    "productivity": ("Personal and team productivity", "个人与团队效率"),
    "operations": ("Operations", "运行维护"),
    "other": ("Other roles", "其他角色"),
}


def load_validator():
    spec = importlib.util.spec_from_file_location(
        "template_validator", REPO / "scripts/validate-agent-templates.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


if sys.version_info < (3, 11):
    raise SystemExit("Template catalog requires Python 3.11+ (tomllib).")

VALIDATOR = load_validator()


def regular(path: Path, directory=False):
    if path.is_symlink() or not (path.is_dir() if directory else path.is_file()):
        raise ValueError(f"{path}: expected a non-symlink {'directory' if directory else 'file'}")


def text(value, label):
    if not isinstance(value, str) or not value.strip() or any(ord(c) < 32 for c in value):
        raise ValueError(f"{label}: expected non-empty single-line text")
    return value


def normalize_skill(skill):
    """Keep declared references, including a missing ref, distinct from installs."""
    if skill["kind"] == "local":
        return {"kind": "local", "path": skill["path"], "reference": skill["path"]}
    if "repo" in skill:
        result = dict(skill)
        result["ref"] = skill.get("ref")
        result["reference"] = skill["repo"] + "/" + skill["path"]
        if result["ref"]:
            result["reference"] += "@" + result["ref"]
        return result
    value = skill.get("uses", skill.get("package"))
    result = {"kind": "github", "reference": value, "ref": None}
    if "uses" in skill:
        if value.startswith(("https://github.com/", "http://github.com/")):
            parts = value.split("/")[3:]
            repo, ref, path = "/".join(parts[:2]), parts[3], "/".join(parts[4:])
        else:
            separator = "@" if "@" in value else "#"
            package, ref = value.rsplit(separator, 1)
            owner, name, path = package.split("/", 2)
            repo = f"{owner}/{name}"
        result.update(repo=repo, path=path, ref=ref)
    else:
        # Package @ suffix selects a skill, not a git ref.
        if value.startswith(("https://github.com/", "http://github.com/")):
            parts = value.split("/")[3:]
            repo = "/".join(parts[:2]).removesuffix(".git")
            name = parts[1].removesuffix(".git")
            path = "/".join(parts[4:]) if len(parts) > 4 else f"skills/{name}"
            ref = parts[3] if len(parts) > 3 and parts[2] == "tree" else None
        else:
            repo, separator, name = value.rpartition("@")
            if not separator:
                repo, name = value, value.split("/")[-1].removesuffix(".git")
            repo = repo.removesuffix(".git")
            path, ref = f"skills/{name}", None
        result.update(repo=repo, path=path, ref=ref, package=value)
    return result


def read_catalog(repo: Path, sha: str):
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("source SHA must be a full lowercase commit SHA")
    root = repo / "agent_templates"
    regular(root, directory=True)
    display_path = repo / "docs/website/template-display.json"
    regular(display_path)
    display = json.loads(display_path.read_text(encoding="utf-8"))
    if (not isinstance(display, dict) or type(display.get("schema")) is not int
            or display["schema"] != 1 or not isinstance(display.get("templates"), dict)):
        raise ValueError("invalid template display schema")
    overrides = display["templates"]
    entries = []
    ranks = []
    for directory in sorted(root.iterdir()):
        regular(directory, directory=True)
        if not VALIDATOR.TEMPLATE_ID.fullmatch(directory.name):
            raise ValueError(f"{directory}: invalid template directory id")
        manifest_path = directory / "template.toml"
        agents = directory / "AGENTS.md"
        regular(manifest_path)
        regular(agents)
        if not agents.read_text(encoding="utf-8").strip():
            raise ValueError(f"{agents}: empty role contract")
        VALIDATOR.validate_manifest(manifest_path, directory.name)
        manifest = VALIDATOR.load_toml(manifest_path)
        for field in ("name", "summary"):
            text(manifest[field], f"{directory.name}.{field}")
        skills_path = directory / "skills.toml"
        skills = []
        if skills_path.exists() or skills_path.is_symlink():
            regular(skills_path)
            VALIDATOR.validate_skills(skills_path)
            skills = [normalize_skill(s) for s in VALIDATOR.load_toml(skills_path)["skills"]]
        override = overrides.get(directory.name, {})
        if not isinstance(override, dict):
            raise ValueError(f"{directory.name}: invalid display override")
        category = override.get("category", "other")
        if not isinstance(category, str) or category not in CATEGORIES:
            raise ValueError(f"{directory.name}: unknown category {category}")
        rank = override.get("featuredRank")
        if rank is not None:
            if type(rank) is not int or rank < 1 or rank in ranks:
                raise ValueError(f"{directory.name}: invalid or duplicate featured rank")
            ranks.append(rank)
        localized = {}
        for locale in ("en", "zh-CN"):
            fields = override.get(locale, {})
            if not isinstance(fields, dict) or set(fields) - {"name", "summary"}:
                raise ValueError(f"{directory.name}: invalid {locale} display fields")
            localized[locale] = {
                field: text(fields.get(field, manifest[field]), f"{directory.name}.{locale}.{field}")
                for field in ("name", "summary")
            }
        source_path = f"agent_templates/{directory.name}"
        source_url = f"{GITHUB}/tree/{sha}/{source_path}"
        entries.append({
            "id": manifest["id"], "name": manifest["name"], "summary": manifest["summary"],
            "compatibility": manifest["compatibility"], "skills": skills,
            "category": category, "featuredRank": rank, "localized": localized,
            "sourcePath": source_path, "sourceUrl": source_url,
            "contractUrl": f"{GITHUB}/blob/{sha}/{source_path}/AGENTS.md",
            "mainUrl": f"{GITHUB}/tree/main/{source_path}", "createSelector": source_url,
        })
    if not entries:
        raise ValueError("official template catalog is empty")
    stale = set(overrides) - {entry["id"] for entry in entries}
    if stale:
        raise ValueError(f"stale display overrides: {', '.join(sorted(stale))}")
    if sorted(ranks) != list(range(1, 7)):
        raise ValueError("homepage requires exactly six featured ranks (1..6)")
    return {"schema": 1, "source": {"repo": "holon-run/holon", "ref": sha, "sha": sha},
            "templates": entries}


def escape(value):
    return html.escape(str(value), quote=True)


def category_name(category, locale):
    return CATEGORIES[category][locale == "zh-CN"]


def category_icon(category):
    paths = {
        "engineering": '<path d="m8 6-6 6 6 6m8-12 6 6-6 6m-3-15-2 18"/>',
        "content": '<path d="M12 5C8 2 4 3 2 4v15c3-2 7-2 10 0 3-2 7-2 10 0V4c-2-1-6-2-10 1Zm0 0v14"/>',
        "productivity": '<rect x="5" y="4" width="14" height="17" rx="2"/><path d="M9 4V2h6v2m-7 9 3 3 5-6"/>',
        "operations": '<path d="M3 12h4l3-8 4 16 3-8h4"/>',
        "other": '<rect x="3" y="3" width="7" height="7" rx="2"/><rect x="14" y="3" width="7" height="7" rx="2"/><rect x="3" y="14" width="7" height="7" rx="2"/><rect x="14" y="14" width="7" height="7" rx="2"/>',
    }
    return (
        f'<span class="template-icon template-icon--{category}">'
        '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" '
        'stroke="currentColor" stroke-width="1.6" stroke-linecap="round" '
        f'stroke-linejoin="round" aria-hidden="true" focusable="false">{paths[category]}</svg></span>'
    )


def card_heading(entry, locale, href=None):
    label = escape(entry["localized"][locale]["name"])
    if href:
        label = f'<a href="{href}">{label}</a>'
    return (
        '<div class="template-card-top">'
        f'{category_icon(entry["category"])}'
        f'<span class="template-category">{escape(category_name(entry["category"], locale))}</span></div>'
        f'<h3>{label}</h3>'
    )


def render_home(catalog, locale):
    zh = locale == "zh-CN"
    base = "/zh-CN" if zh else ""
    title = "Agent 模板" if zh else "Agent Templates"
    lede = ("选择一个长期角色，让它在明确权限内持续跟进工作。" if zh else
            "Choose a long-lived role to follow through within the permissions you set.")
    cards = []
    featured = sorted((e for e in catalog["templates"] if e["featuredRank"]),
                      key=lambda e: e["featuredRank"])
    for entry in featured:
        label = entry["localized"][locale]
        count = len(entry["skills"])
        skills = f"{count} 个技能引用" if zh else f"{count} skill references"
        href = f'{base}/templates/#{entry["id"]}'
        cards.append(
            '<article class="template-card">'
            f'{card_heading(entry, locale, href)}'
            f'<p class="template-summary">{escape(label["summary"])}</p>'
            f'<p class="template-card-footer"><span>{skills}</span><span aria-hidden="true">↗</span></p>'
            '</article>'
        )
    all_label = f'查看全部 {len(catalog["templates"])} 个模板' if zh else f'Explore all {len(catalog["templates"])} templates'
    how = "如何使用模板" if zh else "How to use templates"
    return (
        f'<section class="home-templates" aria-labelledby="agent-templates">\n'
        '<div class="template-section-heading">'
        f'<div><h2 id="agent-templates">{title}</h2>\n<p>{lede}</p></div>'
        f'<a class="template-browse" href="{base}/templates/">{all_label} <span aria-hidden="true">→</span></a></div>\n'
        '<div class="template-grid">\n' + "\n".join(cards) + '\n</div>\n'
        f'<a class="template-guide" href="{base}/reference/agent-templates">{how} →</a>\n'
        '</section>'
    )


def render_directory(catalog, locale):
    zh = locale == "zh-CN"
    base = "/zh-CN" if zh else ""
    title = "Agent 模板" if zh else "Agent Templates"
    summary = ("找到合适的长期角色，让 Agent 在明确权限内持续跟进。" if zh else
               "Find a long-lived role to follow through within the permissions you set.")
    notice = ("这是构建快照，不是 GitHub 实时列表。创建前请安装 Holon，并检查角色契约、技能依赖、环境与权限；展示不代表已安装或已配置。固定模板 SHA 不会固定未指定 ref 的外部技能。" if zh else
              "This is a build snapshot, not a live GitHub listing. Install Holon and review the role contract, skill dependencies, environment and permissions before creating an agent. Listing does not mean installed or configured. Pinning a template SHA does not pin external skills without a ref.")
    result = [
        f"---\ntitle: {title}\nsummary: {summary}\n---\n",
        '<div class="template-directory">',
        '<header class="template-directory-heading">'
        f'<p class="template-eyebrow">{"从一个角色开始" if zh else "A ROLE TO START WITH"}</p>'
        f'<h1>{title}</h1><p>{summary}</p>'
        f'<a class="template-guide" href="{base}/reference/agent-templates">{"如何使用模板" if zh else "How to use templates"} →</a></header>',
        '<div class="template-toolbar">',
        '<div class="template-search" data-template-search hidden>'
        f'<label for="template-query">{"搜索角色" if zh else "Search roles"}</label>'
        f'<input id="template-query" type="search" placeholder="{"搜索名称、职责或技能…" if zh else "Search names, roles or skills…"}" '
        'autocomplete="off" aria-controls="template-catalog"></div>',
        f'<nav class="template-filters" aria-label="{"模板分类" if zh else "Template categories"}">'
        f'<a href="#template-catalog" data-template-filter="all" aria-current="true">{"全部" if zh else "All"} <span>{len(catalog["templates"])}</span></a>' + "".join(
            f'<a href="#{key}" data-template-filter="{key}">{escape(category_name(key, locale))}</a>'
            for key in CATEGORIES if any(e["category"] == key for e in catalog["templates"])
        ) + '</nav></div>',
        '<p class="template-results" role="status" data-template-results hidden></p>',
        '<div class="template-catalog" id="template-catalog">',
    ]
    for category in CATEGORIES:
        entries = [e for e in catalog["templates"] if e["category"] == category]
        if not entries:
            continue
        result.append(
            f'<section class="template-group" id="{category}" data-template-group>'
            f'<h2>{escape(category_name(category, locale))} <span>{len(entries)}</span></h2>'
            '<div class="template-grid">'
        )
        for entry in entries:
            label = entry["localized"][locale]
            command = f'holon agent create {shlex.quote(entry["id"])} --template {shlex.quote(entry["createSelector"])}'
            result.extend([
                f'<article class="template-entry" id="{entry["id"]}">',
                f'<div class="template-overview" data-template-category="{category}">'
                f'{card_heading(entry, locale)}'
                f'<p class="template-summary">{escape(label["summary"])}</p>'
                f'<p class="template-card-footer">{len(entry["skills"])} {"个技能引用" if zh else "skill references"}</p></div>',
                '<details class="template-details">'
                f'<summary>{"查看详情" if zh else "View details"}</summary>'
                '<div class="template-details-body">'
                f'<p>{escape(label["summary"])}</p>'
                f'<p class="template-meta">Holon <code>{escape(entry["compatibility"]["holon"])}</code></p>',
                f'<p><a href="{entry["sourceUrl"]}">{"模板源码（快照）" if zh else "Template source (snapshot)"}</a> · '
                f'<a href="{entry["contractUrl"]}">AGENTS.md</a> · '
                f'<a href="{entry["mainUrl"]}">{"跟随 main" if zh else "Follow main"}</a></p>',
                f'<h4>{"创建 Agent" if zh else "Create an agent"}</h4>'
                f'<p class="template-meta">{"创建前请安装 Holon，并检查角色契约、技能依赖、环境与权限。展示不代表已安装或已配置。" if zh else "Before creating, install Holon and check the role contract, skills, environment and permissions. Listing does not mean installed or configured."}</p>',
                f'<pre><code>{escape(command)}</code></pre>',
                '<div class="template-copy-row">'
                f'<button type="button" data-template-copy hidden>{"复制命令" if zh else "Copy command"}</button>'
                '<span class="template-copy-status" role="status"></span></div>',
            ])
            if entry["skills"]:
                result.append(f'<h4>{"声明的技能引用" if zh else "Declared skill references"}</h4><ul class="template-skills">')
                for skill in entry["skills"]:
                    ref = skill.get("ref")
                    note = (f"ref: {ref}" if ref else
                            ("本地路径" if zh else "local path") if skill["kind"] == "local" else
                            ("跟随默认引用" if zh else "follows default reference"))
                    result.append(f'<li><code>{escape(skill["reference"])}</code> — {escape(note)}</li>')
                result.append('</ul>')
            result.append('</div></details></article>')
        result.append('</div></section>')
    result.extend([
        '</div>',
        '<p class="template-empty" data-template-empty hidden>'
        f'{"没有匹配的角色，试试其他关键词或分类。" if zh else "No matching roles. Try another search or category."}</p>',
        '<details class="template-source">'
        f'<summary>{"关于目录与构建快照" if zh else "About this catalog and snapshot"}</summary>'
        f'<p>{notice}</p><p>{"来源" if zh else "Source"}: '
        f'<a href="{GITHUB}/tree/{catalog["source"]["sha"]}/agent_templates">holon-run/holon</a> · '
        f'<code>{catalog["source"]["sha"]}</code></p></details>',
        '</div>\n<script src="/assets/template-catalog.js" defer></script>\n',
    ])
    # Separate HTML blocks so Markdown search chunking can split role content.
    return "\n\n".join(result)


def replace_slot(source, rendered):
    if source.count(START) != 1 or source.count(END) != 1:
        raise ValueError("homepage must contain exactly one template slot")
    before, _, rest = source.partition(START)
    if END in before:
        raise ValueError("homepage template slot markers out of order")
    _, _, after = rest.partition(END)
    return before + START + "\n" + rendered + "\n" + END + after


def build(repo: Path, output: Path, sha: str):
    site = repo / "docs/website"
    regular(repo / "docs", directory=True)
    regular(site, directory=True)
    if output.is_symlink():
        raise ValueError("output must not be a symlink")
    output = output.resolve()
    if output == site.resolve() or output in site.resolve().parents or output == repo.resolve():
        raise ValueError("output must not replace source content")
    if site.resolve() in output.parents and output.relative_to(site.resolve()).parts[0] != ".tools":
        raise ValueError("output inside the website must be under .tools")
    templates = (repo / "agent_templates").resolve()
    if output == templates or templates in output.parents:
        raise ValueError("output must not replace template sources")
    catalog = read_catalog(repo, sha)
    # Never copy tools, dependency trees, build output or symlink targets.
    ignored = {".tools", ".git", "node_modules", "dist", "__pycache__",
               "template-display.json", "wrangler.jsonc", ".gitignore"}

    def ignore(directory, names):
        for name in names:
            if name not in ignored and (Path(directory) / name).is_symlink():
                raise ValueError(f"{Path(directory) / name}: site symlinks are not supported")
        return ignored.intersection(names)

    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="template-content-", dir=output.parent) as temporary:
        staging = Path(temporary) / "content"
        shutil.copytree(site, staging, ignore=ignore)
        for locale in ("en", "zh-CN"):
            prefix = staging / "zh-CN" if locale == "zh-CN" else staging
            home = prefix / "README.md"
            home.write_text(replace_slot(home.read_text(encoding="utf-8"), render_home(catalog, locale)), encoding="utf-8")
            directory = prefix / "templates"
            directory.mkdir(exist_ok=True)
            (directory / "README.md").write_text(render_directory(catalog, locale), encoding="utf-8")
        assets = staging / "assets"
        assets.mkdir(exist_ok=True)
        (assets / "agent-templates.json").write_text(
            json.dumps(catalog, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        previous = Path(temporary) / "previous"
        if output.exists():
            output.rename(previous)
        try:
            shutil.move(str(staging), output)
        except OSError:
            if output.exists():
                shutil.rmtree(output)
            if previous.exists():
                previous.rename(output)
            raise
    return catalog


def main():
    try:
        sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()
        output = REPO / "docs/website/.tools/dist/content"
        catalog = build(REPO, output, sha)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Template catalog build failed: {error}", file=sys.stderr)
        return 1
    print(f"Rendered {len(catalog['templates'])} official templates at {sha} into {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
