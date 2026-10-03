#!/usr/bin/env python3
"""Validate checked-in agent template files without compiling the Rust runtime."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


ROOTS = (Path("agent_templates"), Path("builtin_templates"))
TEMPLATE_ID = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
REQUIRED_FILES = ("AGENTS.md", "template.toml")
SKILL_KINDS = {"builtin", "github", "local"}


def fail(path: Path, message: str) -> None:
    raise ValueError(f"{path}: {message}")


def load_toml(path: Path) -> dict:
    try:
        with path.open("rb") as stream:
            value = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as error:
        fail(path, f"invalid TOML: {error}")
    if not isinstance(value, dict):
        fail(path, "top-level value must be a table")
    return value


def validate_manifest(path: Path, directory_name: str) -> None:
    manifest = load_toml(path)
    if manifest.get("schema") != "holon.agent_template.v1":
        fail(path, "schema must be holon.agent_template.v1")
    if manifest.get("id") != directory_name:
        fail(path, f"id must match template directory {directory_name!r}")
    for field in ("name", "summary"):
        if not isinstance(manifest.get(field), str) or not manifest[field].strip():
            fail(path, f"{field} must be a non-empty string")
    compatibility = manifest.get("compatibility")
    if not isinstance(compatibility, dict) or not isinstance(
        compatibility.get("holon"), str
    ):
        fail(path, "compatibility.holon must be a string")


def validate_skills(path: Path) -> None:
    manifest = load_toml(path)
    skills = manifest.get("skills")
    if not isinstance(skills, list):
        fail(path, "skills must be an array of tables")
    for index, skill in enumerate(skills):
        if not isinstance(skill, dict):
            fail(path, f"skills[{index}] must be a table")
        kind = skill.get("kind")
        if kind not in SKILL_KINDS:
            fail(path, f"skills[{index}].kind must be one of {sorted(SKILL_KINDS)}")
        if kind == "builtin":
            if not isinstance(skill.get("name"), str) or not skill["name"].strip():
                fail(path, f"skills[{index}].name must be a non-empty string")
        elif kind == "local":
            if not isinstance(skill.get("path"), str) or not skill["path"].strip():
                fail(path, f"skills[{index}].path must be a non-empty string")
        else:
            has_structured_ref = isinstance(skill.get("repo"), str) and isinstance(
                skill.get("path"), str
            )
            has_shorthand_ref = isinstance(skill.get("uses"), str)
            has_legacy_ref = isinstance(skill.get("package"), str)
            if not (has_structured_ref or has_shorthand_ref or has_legacy_ref):
                fail(
                    path,
                    f"skills[{index}] github entry needs repo/path, uses, or package",
                )


def template_directories() -> list[Path]:
    directories: list[Path] = []
    for root in ROOTS:
        if not root.is_dir():
            fail(root, "template root directory is missing")
        for path in sorted(root.iterdir()):
            if not path.is_dir():
                fail(path, "template root may contain directories only")
            if not TEMPLATE_ID.fullmatch(path.name):
                fail(path, "directory name is not a valid template id")
            directories.append(path)
    return directories


def main() -> int:
    try:
        directories = template_directories()
        template_ids = {path.name for path in directories}
        for directory in directories:
            for filename in REQUIRED_FILES:
                path = directory / filename
                if not path.is_file():
                    fail(path, "required file is missing")
            agents = directory / "AGENTS.md"
            if not agents.read_text(encoding="utf-8").strip():
                fail(agents, "file must not be empty")
            validate_manifest(directory / "template.toml", directory.name)
            skills = directory / "skills.toml"
            if skills.exists():
                validate_skills(skills)
            agents_text = agents.read_text(encoding="utf-8")
            for other_id in sorted(template_ids - {directory.name}):
                if other_id in agents_text:
                    fail(
                        agents,
                        f"contains a static reference to another template id {other_id!r}",
                    )
    except (OSError, ValueError) as error:
        print(f"template validation failed: {error}", file=sys.stderr)
        return 1
    print(f"validated {len(directories)} agent templates")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
