#!/usr/bin/env python3
"""Validate checked-in agent template files without compiling the Rust runtime."""

from __future__ import annotations

import re
import sys
import tomllib
import unicodedata
from pathlib import Path


ROOTS = (Path("agent_templates"), Path("builtin_templates"))
TEMPLATE_ID = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
REQUIRED_FILES = ("AGENTS.md", "template.toml")
SKILL_KINDS = {"github", "local"}


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


def validate_github_skill_package(path: Path, index: int, field: str, value: object) -> str:
    if not isinstance(value, str):
        fail(path, f"skills[{index}].{field} must be a string")
    if not value.strip():
        fail(path, f"skills[{index}].{field} must not be empty")
    if value.strip() != value:
        fail(
            path,
            f"skills[{index}].{field} must not contain leading or trailing whitespace",
        )
    if value.startswith("-"):
        fail(path, f"skills[{index}].{field} must not start with '-'")
    if any(
        unicodedata.category(character) == "Cc" or character in " \t\n\r\v\f"
        for character in value
    ):
        fail(
            path,
            f"skills[{index}].{field} must not contain whitespace or control characters",
        )
    return value


def validate_github_skill_repo(path: Path, index: int, value: object) -> str:
    repo = validate_github_skill_package(path, index, "repo", value)
    owner, separator, name = repo.partition("/")
    if not separator or not owner or not name or "/" in name:
        fail(path, f"skills[{index}].repo must be in owner/repo form")
    return repo


def validate_github_skill_path(path: Path, index: int, value: object) -> str:
    skill_path = validate_github_skill_package(path, index, "path", value)
    if skill_path != "." and (
        skill_path.startswith("/") or skill_path.startswith("-")
    ):
        fail(
            path,
            f"skills[{index}].path must be a relative repository path",
        )
    if skill_path != "." and any(
        part in {"", ".", ".."} for part in skill_path.split("/")
    ):
        fail(
            path,
            f"skills[{index}].path must not contain empty, '.' or '..' segments",
        )
    return skill_path


def validate_github_skill_ref(path: Path, index: int, value: object) -> str:
    return validate_github_skill_package(path, index, "ref", value)


def validate_github_skill_name(path: Path, index: int, value: str) -> None:
    if value in {".", ".."} or "/" in value or "\\" in value:
        fail(
            path,
            f"skills[{index}] package skill name must be a plain skill directory name",
        )
    validate_github_skill_package(path, index, "package", value)


def validate_github_skill_uses(path: Path, index: int, value: object) -> None:
    uses = validate_github_skill_package(path, index, "uses", value)
    if uses.startswith(("https://github.com/", "http://github.com/")):
        tree_path = uses.split("/", 3)[3]
        parts = tree_path.split("/")
        if len(parts) < 5 or not parts[0] or not parts[1] or parts[2] != "tree":
            fail(
                path,
                f"skills[{index}].uses URL must be a tree URL "
                "(owner/repo/tree/ref/path)",
            )
        validate_github_skill_repo(path, index, f"{parts[0]}/{parts[1].removesuffix('.git')}")
        validate_github_skill_ref(path, index, parts[3])
        validate_github_skill_path(path, index, "/".join(parts[4:]))
        return

    path_ref, separator, git_ref = uses.rpartition("@")
    if not separator:
        path_ref, separator, git_ref = uses.rpartition("#")
    if not separator or not path_ref or not git_ref:
        fail(
            path,
            f"skills[{index}].uses must include a ref via "
            "owner/repo/path@ref or owner/repo/path#ref",
        )
    parts = path_ref.split("/")
    if len(parts) < 3 or not parts[0] or not parts[1] or not "/".join(parts[2:]):
        fail(
            path,
            f"skills[{index}].uses must be in owner/repo/path@ref form",
        )
    validate_github_skill_repo(path, index, f"{parts[0]}/{parts[1]}")
    validate_github_skill_path(path, index, "/".join(parts[2:]))
    validate_github_skill_ref(path, index, git_ref)


def validate_github_skill(path: Path, index: int, skill: dict) -> None:
    package = skill.get("package")
    uses = skill.get("uses")
    repo = skill.get("repo")
    skill_path = skill.get("path")
    git_ref = skill.get("ref")

    present = []
    if package is not None:
        present.append("package")
    if uses is not None:
        present.append("uses")
    if repo is not None or skill_path is not None or git_ref is not None:
        present.append("structured")
    if len(present) != 1:
        fail(
            path,
            f"skills[{index}] github entry must use exactly one of "
            "package, uses, or structured repo/path/ref fields",
        )
    if package is not None:
        package_value = validate_github_skill_package(path, index, "package", package)
        if "@" in package_value:
            remote_package, skill_name = package_value.rsplit("@", 1)
            if "/" in remote_package:
                validate_github_skill_package(path, index, "package", remote_package)
                validate_github_skill_name(path, index, skill_name)
        return
    if uses is not None:
        validate_github_skill_uses(path, index, uses)
        return
    validate_github_skill_repo(path, index, repo)
    validate_github_skill_path(path, index, skill_path)
    if git_ref is not None:
        validate_github_skill_ref(path, index, git_ref)


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
        if kind == "local":
            if not isinstance(skill.get("path"), str) or not skill["path"].strip():
                fail(path, f"skills[{index}].path must be a non-empty string")
        else:
            validate_github_skill(path, index, skill)


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
