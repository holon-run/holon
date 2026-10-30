#!/usr/bin/env python3
"""Regression tests for the lightweight agent template validator."""

import itertools
import runpy
import unittest
from pathlib import Path


VALIDATOR = runpy.run_path(str(Path(__file__).with_name("validate-agent-templates.py")))


class GithubSkillValidationTests(unittest.TestCase):
    def validate(self, fields: dict) -> None:
        VALIDATOR["validate_github_skill"](
            Path("skills.toml"), 0, {"kind": "github", **fields}
        )

    def test_accepts_each_reference_form(self) -> None:
        for fields in (
            {"package": "owner/repo@skill"},
            {"uses": "owner/repo/skills/example@main"},
            {"uses": "owner/repo/skills/example#main"},
            {"uses": "https://github.com/owner/repo/tree/main/skills/example"},
            {"repo": "owner/repo", "path": "skills/example"},
            {"repo": "owner/repo", "path": "skills/example", "ref": "main"},
        ):
            with self.subTest(fields=fields):
                self.validate(fields)

    def test_rejects_mixed_reference_forms(self) -> None:
        forms = (
            {"package": "owner/repo@skill"},
            {"uses": "owner/repo/skills/example@main"},
            {"repo": "owner/repo", "path": "skills/example"},
        )
        for size in (2, 3):
            for combination in itertools.combinations(forms, size):
                fields = {key: value for form in combination for key, value in form.items()}
                with self.subTest(fields=fields):
                    with self.assertRaisesRegex(ValueError, "exactly one"):
                        self.validate(fields)

    def test_rejects_missing_or_incomplete_references(self) -> None:
        for fields in (
            {},
            {"repo": "owner/repo"},
            {"path": "skills/example"},
            {"ref": "main"},
            {"package": ""},
            {"uses": ""},
            {"package": "owner/repo@skill", "ref": "main"},
            {"uses": "owner/repo/skills/example@main", "path": "skills/example"},
        ):
            with self.subTest(fields=fields):
                with self.assertRaises(ValueError):
                    self.validate(fields)


class VideoProducerSkillTests(unittest.TestCase):
    def test_motion_reference_is_pinned_alongside_production_skills(self) -> None:
        path = (
            Path(__file__).resolve().parent.parent
            / "agent_templates/video-producer/skills.toml"
        )
        VALIDATOR["validate_skills"](path)
        skills = VALIDATOR["load_toml"](path)["skills"]
        motion = [
            skill
            for skill in skills
            if skill.get("repo") == "showheyohtaki/motion-prompt"
        ]
        self.assertEqual(len(motion), 1)
        self.assertEqual(motion[0]["kind"], "github")
        self.assertEqual(motion[0]["path"], "skills/motion-prompt")
        self.assertRegex(motion[0]["ref"], r"^[0-9a-f]{40}$")
        sources = {(skill.get("repo"), skill.get("path")) for skill in skills}
        self.assertIn(("holon-run/holon", "skills/video-production"), sources)
        self.assertIn(("remotion-dev/skills", "skills/remotion-best-practices"), sources)


if __name__ == "__main__":
    unittest.main()
