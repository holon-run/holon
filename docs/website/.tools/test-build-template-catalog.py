#!/usr/bin/env python3
"""Regression tests for the isolated official template catalog build.

Run with Python 3.11+ from any working directory. All fixtures are temporary;
the real generator and manifest validator are loaded without invoking their CLIs.
"""

import html
import importlib.util
import json
from pathlib import Path
import re
import shlex
import shutil
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().with_name("build-template-catalog.py")
SPEC = importlib.util.spec_from_file_location("build_template_catalog", SCRIPT)
BUILDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILDER)
SHA = "0123456789abcdef" * 2 + "01234567"


class TemplateCatalogTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="template-catalog-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.site = self.repo / "docs/website"
        self.templates = self.repo / "agent_templates"
        self.output = self.root / "content"
        self.templates.mkdir(parents=True)
        self.ids = [f"role-{number}" for number in range(1, 7)]
        self.display = {"schema": 1, "templates": {}}
        # Reverse creation order so filesystem order cannot satisfy sorting tests.
        for rank in range(6, 0, -1):
            template_id = f"role-{rank}"
            self.add_template(template_id)
            self.display["templates"][template_id] = {
                "category": "engineering",
                "featuredRank": rank,
            }
        self.write_display()
        self.home = (
            "# Before\n\n" + BUILDER.START + "\nold template content\n"
            + BUILDER.END + "\n\n## After\n"
        )
        for locale in ("", "zh-CN/"):
            self.write(self.site / locale / "README.md", self.home)
        self.write(self.site / "assets/site.css", "body { color: black; }\n")
        self.write(self.site / "assets/logo.svg", "<svg></svg>\n")
        self.write(self.site / "mdorigin.config.json", '{"title": "Fixture"}\n')
        self.write(self.site / "reference/agent-templates.md", "# Template guide\n")

    def write(self, path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")
        return path

    def add_template(self, template_id, *, name=None, summary=None):
        manifest = (
            'schema = "holon.agent_template.v1"\n'
            f"id = {json.dumps(template_id)}\n"
            f"name = {json.dumps(name or template_id.title())}\n"
            f"summary = {json.dumps(summary or 'Role contract for ' + template_id)}\n"
            '[compatibility]\nholon = ">=0.1.0"\n'
        )
        directory = self.templates / template_id
        self.write(directory / "template.toml", manifest)
        self.write(directory / "AGENTS.md", f"# {template_id}\n\nOwn this role.\n")
        return directory

    def write_display(self):
        self.write(
            self.site / "template-display.json",
            json.dumps(self.display, ensure_ascii=False),
        )

    def set_skills(self, entries, template_id="role-1"):
        blocks = []
        for entry in entries:
            blocks.append("[[skills]]\n" + "\n".join(
                f"{key} = {json.dumps(value)}" for key, value in entry.items()
            ))
        self.write(self.templates / template_id / "skills.toml", "\n\n".join(blocks))

    def catalog(self):
        return BUILDER.read_catalog(self.repo, SHA)

    def build(self, output=None):
        return BUILDER.build(self.repo, output or self.output, SHA)

    def entry(self, catalog, template_id="role-1"):
        return next(entry for entry in catalog["templates"] if entry["id"] == template_id)

    def output_catalog(self):
        return json.loads((self.output / "assets/agent-templates.json").read_text())

    def detail(self, template_id="role-1", locale=""):
        return (self.output / locale / "templates" / template_id / "README.md").read_text()

    def snapshot(self, directory):
        """Record contents and link targets without following symlinks."""
        return {
            path.relative_to(directory).as_posix(): (
                ("link", str(path.readlink())) if path.is_symlink()
                else ("directory",) if path.is_dir()
                else ("file", path.read_bytes())
            )
            for path in sorted(directory.rglob("*"))
        }

    def assert_rejected(self, pattern):
        with self.assertRaisesRegex(ValueError, pattern):
            self.catalog()

    def test_valid_catalog_contains_snapshot_metadata_and_compatibility(self):
        catalog = self.build()
        self.assertEqual(catalog["schema"], 1)
        self.assertEqual(catalog["source"], {
            "repo": "holon-run/holon", "ref": SHA, "sha": SHA,
        })
        self.assertEqual(catalog, self.output_catalog())
        self.assertEqual(self.entry(catalog)["compatibility"], {"holon": ">=0.1.0"})
        for entry in catalog["templates"]:
            with self.subTest(template=entry["id"]):
                path = f"agent_templates/{entry['id']}"
                base = f"https://github.com/holon-run/holon"
                self.assertEqual(entry["sourcePath"], path)
                self.assertEqual(entry["sourceUrl"], f"{base}/tree/{SHA}/{path}")
                self.assertEqual(entry["createSelector"], entry["sourceUrl"])
                self.assertEqual(entry["contractUrl"], f"{base}/blob/{SHA}/{path}/AGENTS.md")
                self.assertEqual(entry["mainUrl"], f"{base}/tree/main/{path}")

    def test_packaged_email_assistant_renders_localized_catalog_and_skill_sources(self):
        repo = SCRIPT.parents[3]
        template_id = "email-assistant"
        shutil.copytree(repo / "agent_templates" / template_id,
                        self.templates / template_id)
        display = json.loads((repo / "docs/website/template-display.json").read_text())
        self.display["templates"][template_id] = display["templates"][template_id]
        self.write_display()

        catalog = self.build()
        entry = self.entry(catalog, template_id)
        self.assertEqual(entry["category"], "productivity")
        self.assertIsNone(entry["featuredRank"])
        self.assertEqual(entry["localized"]["zh-CN"]["name"], "邮件助理")
        declarations = BUILDER.VALIDATOR.load_toml(
            repo / "agent_templates" / template_id / "skills.toml")["skills"]
        self.assertEqual(
            {(skill["repo"], skill["path"]) for skill in declarations},
            {("holon-run/holon", "skills/email-workflow"),
             ("holon-run/uxc", "skills/uxc"),
             ("holon-run/agentinbox", "skills/agentinbox"),
             ("holon-run/sview", "skills/sview")},
        )
        self.assertTrue((repo / "skills/email-workflow/SKILL.md").is_file())
        for locale in ("", "zh-CN/"):
            fields = entry["localized"]["zh-CN"] if locale else entry
            page = (self.output / locale / "templates/README.md").read_text()
            self.assertIn(fields["name"], page)
            self.assertIn(fields["summary"], page)
            self.assertIn(entry["createSelector"], self.detail(template_id, locale))
            self.assertNotIn(template_id,
                             (self.output / locale / "README.md").read_text())

    def test_nonfeatured_add_modify_delete_updates_all_generated_outputs(self):
        self.build()
        directory = self.add_template("added-role", summary="Original nonfeatured role")
        self.build()
        self.assertIn("added-role", [e["id"] for e in self.output_catalog()["templates"]])
        for locale in ("", "zh-CN/"):
            page = self.output / locale / "templates/README.md"
            self.assertIn("Original nonfeatured role", page.read_text())
            self.assertIn("Original nonfeatured role", self.detail("added-role", locale))
            self.assertNotIn("added-role", (self.output / locale / "README.md").read_text())
        self.add_template("added-role", summary="Changed nonfeatured role")
        self.build()
        self.assertEqual(self.entry(self.output_catalog(), "added-role")["summary"],
                         "Changed nonfeatured role")
        for locale in ("", "zh-CN/"):
            page = (self.output / locale / "templates/README.md").read_text()
            self.assertIn("Changed nonfeatured role", page)
            self.assertNotIn("Original nonfeatured role", page)
            self.assertIn("Changed nonfeatured role", self.detail("added-role", locale))
        shutil.rmtree(directory)
        self.build()
        self.assertNotIn("added-role", [e["id"] for e in self.output_catalog()["templates"]])
        for locale in ("", "zh-CN/"):
            self.assertNotIn("added-role",
                             (self.output / locale / "templates/README.md").read_text())
            self.assertFalse((self.output / locale / "templates/added-role").exists())

    def test_catalog_sort_is_stable_and_featured_rank_controls_home_order(self):
        for template_id in ("z-role", "a-role", "middle-role"):
            self.add_template(template_id)
        self.display["templates"]["role-1"]["featuredRank"] = 6
        self.display["templates"]["role-6"]["featuredRank"] = 1
        self.write_display()
        catalog = self.build()
        ids = [e["id"] for e in catalog["templates"]]
        self.assertEqual(ids, sorted(ids))
        expected = ["role-6", "role-2", "role-3", "role-4", "role-5", "role-1"]
        for locale in ("", "zh-CN/"):
            home = (self.output / locale / "README.md").read_text()
            self.assertEqual(re.findall(r'/templates/([^/"]+)/', home), expected)
            self.assertEqual(home.count('<article class="template-card">'), 6)
        directory = (self.output / "templates/README.md").read_text()
        self.assertEqual(re.findall(r'<article class="template-entry" id="([^"]+)">',
                                    directory), self.ids + ["a-role", "middle-role", "z-role"])

    def test_empty_template_root_is_rejected(self):
        for path in self.templates.iterdir():
            shutil.rmtree(path)
        self.assert_rejected("empty")

    def test_missing_template_root_is_rejected(self):
        shutil.rmtree(self.templates)
        self.assert_rejected("directory")

    def test_invalid_directory_id_is_rejected(self):
        self.add_template("Invalid_Role")
        self.assert_rejected("invalid template directory id")

    def test_non_directory_template_entry_is_rejected(self):
        self.write(self.templates / "stray-file", "not a template")
        self.assert_rejected("directory")

    def test_duplicate_manifest_id_in_another_directory_is_rejected(self):
        directory = self.add_template("duplicate-role")
        source = (self.templates / "role-1/template.toml").read_text()
        self.write(directory / "template.toml", source)
        self.assert_rejected("id must match template directory")

    def test_missing_agents_contract_is_rejected(self):
        (self.templates / "role-1/AGENTS.md").unlink()
        self.assert_rejected("AGENTS.md")

    def test_empty_agents_contract_is_rejected(self):
        self.write(self.templates / "role-1/AGENTS.md", " \n\t")
        self.assert_rejected("empty role contract")

    def test_missing_manifest_is_rejected(self):
        (self.templates / "role-1/template.toml").unlink()
        self.assert_rejected("template.toml")

    def test_malformed_template_toml_is_rejected(self):
        self.write(self.templates / "role-1/template.toml", "schema = [")
        self.assert_rejected("invalid TOML")

    def test_wrong_template_schema_is_rejected(self):
        path = self.templates / "role-1/template.toml"
        self.write(path, path.read_text().replace("holon.agent_template.v1", "unknown.v2"))
        self.assert_rejected("schema")

    def test_missing_compatibility_is_rejected(self):
        path = self.templates / "role-1/template.toml"
        self.write(path, path.read_text().split("[compatibility]")[0])
        self.assert_rejected("compatibility.holon")

    def test_empty_or_multiline_manifest_text_is_rejected(self):
        for field in ("name", "summary"):
            for value in ("", "line\nbreak"):
                with self.subTest(field=field, value=value):
                    self.add_template("role-1")
                    path = self.templates / "role-1/template.toml"
                    source = re.sub(rf"^{field} = .*$",
                                    lambda _: f"{field} = {json.dumps(value)}",
                                    path.read_text(), flags=re.MULTILINE)
                    self.write(path, source)
                    self.assert_rejected(field)

    def test_invalid_source_sha_is_rejected(self):
        for sha in ("", "main", "1234567", SHA.upper(), "a" * 39, "g" * 40):
            with self.subTest(sha=sha), self.assertRaisesRegex(ValueError, "SHA"):
                BUILDER.read_catalog(self.repo, sha)

    def test_skills_manifest_is_optional(self):
        catalog = self.build()
        self.assertTrue(all(entry["skills"] == [] for entry in catalog["templates"]))
        self.assertIn("0 skill references", (self.output / "README.md").read_text())

    def test_directory_is_a_progressively_enhanced_role_gallery(self):
        self.build()
        for locale in ("", "zh-CN/"):
            page = (self.output / locale / "templates/README.md").read_text()
            with self.subTest(locale=locale):
                self.assertIn('data-template-search hidden', page)
                self.assertIn('aria-controls="template-catalog"', page)
                self.assertIn('href="#engineering" data-template-filter="engineering"', page)
                self.assertIn('data-template-results hidden', page)
                self.assertIn('data-template-empty hidden', page)
                self.assertIn('<script src="/assets/template-catalog.js" defer></script>', page)
                self.assertNotIn('<details class="template-details">', page)
                self.assertNotIn('<details open', page)
                self.assertNotIn('data-template-copy', page)
                overviews = re.findall(
                    r'<div class="template-overview"[\s\S]*?</p></div>', page,
                )
                self.assertEqual(len(overviews), 6)
                for overview in overviews:
                    self.assertIn('class="template-summary"', overview)
                    self.assertNotIn("holon agent create", overview)
                    self.assertNotIn(SHA, overview)
                for template_id in self.ids:
                    detail = self.detail(template_id, locale)
                    self.assertIn("holon agent create", detail)
                    self.assertIn(SHA, detail)
                    self.assertIn("AGENTS.md", detail)
                    self.assertIn("Holon", detail)
                self.assertIn("检查角色契约" if locale else "review the role contract", page)

    def test_gallery_controls_are_localized_and_snapshot_is_secondary(self):
        catalog = self.catalog()
        for locale, labels in (
            ("en", ("Search roles", "View details", "About this catalog and snapshot")),
            ("zh-CN", ("搜索角色", "查看详情", "关于目录与构建快照")),
        ):
            page = BUILDER.render_directory(catalog, locale)
            for label in labels:
                self.assertIn(label, page)
            detail = BUILDER.render_detail(catalog["templates"][0], locale)
            self.assertIn("复制命令" if locale == "zh-CN" else "Copy command", detail)
            self.assertGreater(page.index(SHA), page.index('class="template-catalog"'))
            source = page.split('<details class="template-source">')[1]
            self.assertIn(SHA, source)
            self.assertIn("未指定 ref" if locale == "zh-CN" else "without a ref", source)

    def test_home_gallery_has_browse_link_without_creation_metadata(self):
        catalog = self.catalog()
        for locale, base in (("en", ""), ("zh-CN", "/zh-CN")):
            page = BUILDER.render_home(catalog, locale)
            self.assertIn(f'class="template-browse" href="{base}/templates/"', page)
            self.assertEqual(page.count('class="template-card"'), 6)
            self.assertEqual(page.count('aria-hidden="true" focusable="false"'), 6)
            self.assertNotIn("holon agent create", page)
            self.assertNotIn(SHA, page)

    def test_explicit_empty_skills_list_is_supported(self):
        self.write(self.templates / "role-1/skills.toml", "skills = []\n")
        self.assertEqual(self.entry(self.catalog())["skills"], [])

    def test_local_skills_are_declarations_not_paths_to_install(self):
        reference = "../skills/not-installed"
        self.set_skills([{"kind": "local", "path": reference}])
        skill = self.entry(self.build())["skills"][0]
        self.assertEqual(skill, {"kind": "local", "path": reference, "reference": reference})
        self.assertFalse((self.repo / reference).exists())
        self.assertIn("local path", self.detail())
        self.assertIn("本地路径", self.detail(locale="zh-CN"))

    def test_structured_skill_missing_ref_stays_unpinned(self):
        self.set_skills([{"kind": "github", "repo": "acme/tools", "path": "skills/audit"}])
        skill = self.entry(self.build())["skills"][0]
        self.assertIsNone(skill["ref"])
        self.assertEqual(skill["reference"], "acme/tools/skills/audit")
        self.assertNotIn(SHA, skill["reference"])
        self.assertIn("follows default reference",
                      self.detail())
        self.assertIn("跟随默认引用",
                      self.detail(locale="zh-CN"))

    def test_structured_skill_explicit_ref_is_preserved(self):
        self.set_skills([{
            "kind": "github", "repo": "acme/tools", "path": "skills/audit", "ref": "v1.2",
        }])
        skill = self.entry(self.catalog())["skills"][0]
        self.assertEqual(skill["ref"], "v1.2")
        self.assertEqual(skill["reference"], "acme/tools/skills/audit@v1.2")

    def test_uses_shorthand_supports_at_and_hash_refs(self):
        references = ["acme/tools/skills/audit@release/v1", "acme/tools/skills/audit#v2"]
        self.set_skills([{"kind": "github", "uses": value} for value in references])
        skills = self.entry(self.catalog())["skills"]
        for skill, reference, ref in zip(skills, references, ("release/v1", "v2")):
            with self.subTest(reference=reference):
                self.assertEqual(skill["reference"], reference)
                self.assertEqual((skill["repo"], skill["path"], skill["ref"]),
                                 ("acme/tools", "skills/audit", ref))

    def test_uses_github_tree_url_preserves_ref(self):
        reference = "https://github.com/acme/tools/tree/v3/skills/audit"
        self.set_skills([{"kind": "github", "uses": reference}])
        skill = self.entry(self.catalog())["skills"][0]
        self.assertEqual(skill["reference"], reference)
        self.assertEqual((skill["repo"], skill["path"], skill["ref"]),
                         ("acme/tools", "skills/audit", "v3"))

    def test_package_at_suffix_selects_skill_not_git_ref(self):
        self.set_skills([{"kind": "github", "package": "acme/tools@audit"}])
        skill = self.entry(self.catalog())["skills"][0]
        self.assertEqual(skill["reference"], "acme/tools@audit")
        self.assertEqual(skill["package"], "acme/tools@audit")
        self.assertEqual((skill["repo"], skill["path"], skill["ref"]),
                         ("acme/tools", "skills/audit", None))

    def test_package_url_and_bare_repo_ref_semantics(self):
        cases = (
            ("acme/tools.git", "acme/tools", "skills/tools", None),
            ("https://github.com/acme/tools.git", "acme/tools", "skills/tools", None),
            ("https://github.com/acme/tools/tree/v4/skills/audit",
             "acme/tools", "skills/audit", "v4"),
        )
        self.set_skills([{"kind": "github", "package": case[0]} for case in cases])
        for skill, (reference, repo, path, ref) in zip(
                self.entry(self.catalog())["skills"], cases):
            with self.subTest(reference=reference):
                self.assertEqual(skill["reference"], reference)
                self.assertEqual((skill["repo"], skill["path"], skill["ref"]),
                                 (repo, path, ref))

    def test_malformed_skills_toml_is_rejected(self):
        self.write(self.templates / "role-1/skills.toml", "[[skills]")
        self.assert_rejected("invalid TOML")

    def test_invalid_skills_schema_and_unpinned_uses_are_rejected(self):
        for contents in (
            'skills = "not an array"',
            '[[skills]]\nkind = "unknown"',
            '[[skills]]\nkind = "github"\nuses = "acme/tools/skills/audit"',
            '[[skills]]\nkind = "github"\nrepo = "acme/tools"',
        ):
            with self.subTest(contents=contents):
                self.write(self.templates / "role-1/skills.toml", contents)
                self.assert_rejected("skills")

    def test_html_injection_in_names_summaries_and_skill_text_is_escaped(self):
        name = '<img src=x onerror="alert(1)"> & \'name\''
        summary = '<script>alert("summary")</script> & details'
        self.add_template("role-1", name=name, summary=summary)
        self.set_skills([{"kind": "local", "path": '<img src=x onerror="alert(2)">'}])
        catalog = self.build()
        self.assertEqual(self.entry(catalog)["name"], name)
        for locale in ("", "zh-CN/"):
            for relative in ("README.md", "templates/README.md", "templates/role-1/README.md"):
                with self.subTest(locale=locale, page=relative):
                    page = (self.output / locale / relative).read_text()
                    for value in (name, summary):
                        self.assertIn(html.escape(value, quote=True), page)
                        self.assertNotIn(value, page)
            page = self.detail(locale=locale).split("---", 2)[-1]
            self.assertIn("&lt;img src=x onerror=", page)
            self.assertNotIn('<img src=x onerror=', page)

    def test_create_commands_use_exact_snapshot_sha_in_both_locales(self):
        self.build()
        for locale in ("", "zh-CN/"):
            for template_id in self.ids:
                page = self.detail(template_id, locale)
                commands = re.findall(r"<pre><code>(.*?)</code></pre>", page)
                self.assertEqual(len(commands), 1)
                command = commands[0]
                self.assertEqual(shlex.split(html.unescape(command)), [
                    "holon", "agent", "create", template_id, "--template",
                    f"https://github.com/holon-run/holon/tree/{SHA}/agent_templates/{template_id}",
                ])

    def test_cards_link_to_bilingual_detail_pages_without_expanding(self):
        self.set_skills([{"kind": "github", "repo": "acme/tools", "path": "skills/audit"}])
        catalog = self.build()
        for locale in ("", "zh-CN"):
            base = f"/{locale}" if locale else ""
            directory = (self.output / locale / "templates/README.md").read_text()
            self.assertNotIn('class="template-details"', directory)
            self.assertNotIn("<pre>", directory)
            self.assertIn('data-template-keywords="acme/tools/skills/audit"', directory)
            for entry in catalog["templates"]:
                self.assertIn(f'href="{base}/templates/{entry["id"]}/"', directory)
                page = self.detail(entry["id"], locale)
                self.assertIn(f'href="{base}/templates/"', page)
                self.assertIn(f'href="{base}/getting-started/"', page)
                self.assertIn(entry["contractUrl"], page)
                self.assertIn(entry["sourceUrl"], page)
                self.assertIn('data-template-copy hidden', page)
                self.assertIn('src="/assets/template-catalog.js"', page)
                self.assertEqual(page.count("<h1>"), 1)

    def test_detail_skill_links_preserve_refs_and_do_not_link_local_paths(self):
        self.set_skills([
            {"kind": "github", "repo": "acme/tools", "path": "skills/audit", "ref": "release/v1"},
            {"kind": "github", "repo": "acme/tools", "path": "skills/default"},
            {"kind": "local", "path": "../skills/private"},
        ])
        self.build()
        page = self.detail()
        self.assertIn('href="https://github.com/acme/tools/tree/release%2Fv1/skills/audit"', page)
        self.assertIn('href="https://github.com/acme/tools/tree/HEAD/skills/default"', page)
        self.assertIn("<code>../skills/private</code>", page)
        self.assertNotIn('href="../skills/private"', page)

    def test_new_role_without_display_override_uses_other_category_and_manifest_text(self):
        self.add_template("new-role", name="New role", summary="New summary")
        catalog = self.build()
        entry = self.entry(catalog, "new-role")
        self.assertEqual(entry["category"], "other")
        self.assertIsNone(entry["featuredRank"])
        for locale in ("en", "zh-CN"):
            self.assertEqual(entry["localized"][locale],
                             {"name": "New role", "summary": "New summary"})
        self.assertIn("Other roles", (self.output / "templates/README.md").read_text())
        self.assertIn("其他角色", (self.output / "zh-CN/templates/README.md").read_text())

    def test_partial_chinese_override_falls_back_to_manifest_not_english_override(self):
        self.display["templates"]["role-1"].update({
            "en": {"name": "English override", "summary": "English summary"},
            "zh-CN": {"name": "中文角色"},
        })
        self.write_display()
        entry = self.entry(self.build())
        self.assertEqual(entry["localized"]["zh-CN"],
                         {"name": "中文角色", "summary": "Role contract for role-1"})
        for relative in ("README.md", "templates/README.md"):
            page = (self.output / "zh-CN" / relative).read_text()
            self.assertIn("中文角色", page)
            self.assertNotIn("English override", page)

    def test_unknown_category_is_rejected(self):
        self.display["templates"]["role-1"]["category"] = "unrecognized"
        self.write_display()
        self.assert_rejected("unknown category")

    def test_stale_display_override_is_rejected(self):
        self.display["templates"]["deleted-role"] = {"category": "other"}
        self.write_display()
        self.assert_rejected("stale display overrides.*deleted-role")

    def test_duplicate_featured_rank_is_rejected(self):
        self.display["templates"]["role-2"]["featuredRank"] = 1
        self.write_display()
        self.assert_rejected("duplicate featured rank")

    def test_featured_ranks_must_be_exactly_one_through_six(self):
        for rank in (None, 7, 0, -1, True, "1"):
            with self.subTest(rank=rank):
                self.display["templates"]["role-1"]["featuredRank"] = rank
                self.write_display()
                self.assert_rejected("featured rank")

    def test_seventh_featured_role_is_rejected(self):
        self.add_template("role-7")
        self.display["templates"]["role-7"] = {"featuredRank": 7}
        self.write_display()
        self.assert_rejected("exactly six featured ranks")

    def test_display_schema_is_validated(self):
        for value in ({"templates": {}}, {"schema": 2, "templates": {}},
                      {"schema": 1, "templates": []}):
            with self.subTest(value=value):
                self.write(self.site / "template-display.json", json.dumps(value))
                self.assert_rejected("display schema")

    def test_display_top_level_must_be_an_object(self):
        for value in ([], None, "invalid", 1):
            with self.subTest(value=value):
                self.write(self.site / "template-display.json", json.dumps(value))
                self.assert_rejected("display schema")

    def test_malformed_display_json_is_rejected(self):
        self.write(self.site / "template-display.json", '{"schema":')
        with self.assertRaises(ValueError):
            self.catalog()

    def test_display_override_must_be_an_object(self):
        self.display["templates"]["role-1"] = []
        self.write_display()
        self.assert_rejected("invalid display override")

    def test_invalid_localized_display_fields_are_rejected(self):
        override = self.display["templates"]["role-1"]
        for fields in ([], {"unknown": "value"}, {"name": ""}, {"summary": "line\nbreak"}):
            with self.subTest(fields=fields):
                override["zh-CN"] = fields
                self.write_display()
                self.assert_rejected("display fields|single-line text")

    def test_slot_replacement_preserves_surrounding_content(self):
        self.assertEqual(BUILDER.replace_slot(self.home, "<p>New</p>"),
                         "# Before\n\n" + BUILDER.START + "\n<p>New</p>\n"
                         + BUILDER.END + "\n\n## After\n")

    def test_missing_slot_markers_are_rejected(self):
        for source in ("", BUILDER.START, BUILDER.END):
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "slot"):
                BUILDER.replace_slot(source, "rendered")

    def test_duplicate_slot_markers_are_rejected(self):
        for source in (self.home * 2, BUILDER.START + self.home, self.home + BUILDER.END):
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "slot"):
                BUILDER.replace_slot(source, "rendered")

    def test_reversed_slot_markers_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "out of order"):
            BUILDER.replace_slot(BUILDER.END + "\n" + BUILDER.START, "rendered")

    def test_template_file_symlinks_cannot_escape_repository(self):
        for filename in ("template.toml", "AGENTS.md", "skills.toml"):
            with self.subTest(filename=filename):
                path = self.templates / "role-1" / filename
                original = path.read_text() if path.exists() else None
                target = self.write(self.root / f"external-{filename}", original or "skills = []")
                if path.exists():
                    path.unlink()
                path.symlink_to(target)
                self.assert_rejected("non-symlink")
                path.unlink()
                if original is not None:
                    self.write(path, original)

    def test_broken_optional_skills_symlink_is_rejected(self):
        (self.templates / "role-1/skills.toml").symlink_to(self.root / "missing")
        self.assert_rejected("non-symlink")

    def test_template_directory_symlink_cannot_escape_repository(self):
        original = self.templates / "role-1"
        external = self.root / "external-role"
        original.rename(external)
        original.symlink_to(external, target_is_directory=True)
        self.assert_rejected("non-symlink")

    def test_template_root_symlink_is_rejected(self):
        external = self.root / "external-templates"
        self.templates.rename(external)
        self.templates.symlink_to(external, target_is_directory=True)
        self.assert_rejected("non-symlink")

    def test_display_file_symlink_is_rejected(self):
        path = self.site / "template-display.json"
        external = self.root / "external-display.json"
        path.rename(external)
        path.symlink_to(external)
        self.assert_rejected("non-symlink")

    def test_website_file_and_directory_symlinks_are_rejected(self):
        for name, directory in (("external.md", False), ("external-assets", True)):
            with self.subTest(name=name):
                target = self.root / name
                if directory:
                    target.mkdir()
                    self.write(target / "secret.txt", "outside site")
                else:
                    self.write(target, "outside site")
                link = self.site / name
                link.symlink_to(target, target_is_directory=directory)
                with self.assertRaisesRegex(ValueError, "site symlinks"):
                    self.build()
                link.unlink()
                self.assertFalse(self.output.exists())

    def test_website_root_symlink_cannot_escape_repository(self):
        external = self.root / "external-site"
        self.site.rename(external)
        self.site.symlink_to(external, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "non-symlink"):
            self.build()

    def test_website_parent_symlink_cannot_escape_repository(self):
        docs = self.repo / "docs"
        external = self.root / "external-docs"
        docs.rename(external)
        docs.symlink_to(external, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.build()

    def test_staging_keeps_all_source_bytes_unchanged_and_copies_assets_config(self):
        before = self.snapshot(self.repo)
        self.build()
        self.assertEqual(before, self.snapshot(self.repo))
        for relative in ("assets/site.css", "assets/logo.svg", "mdorigin.config.json",
                         "reference/agent-templates.md"):
            with self.subTest(relative=relative):
                self.assertEqual((self.site / relative).read_bytes(),
                                 (self.output / relative).read_bytes())
        for locale in ("", "zh-CN/"):
            self.assertEqual((self.site / locale / "README.md").read_text(), self.home)
            self.assertFalse((self.site / locale / "templates").exists())
        self.assertFalse((self.site / "assets/agent-templates.json").exists())

    def test_tools_dependencies_and_build_output_are_not_copied(self):
        ignored = (".tools", "node_modules", "dist", ".git", "__pycache__")
        for base in (self.site, self.site / "assets/nested"):
            for name in ignored:
                self.write(base / name / "sentinel", "must never be copied")
        for name in ("wrangler.jsonc", ".gitignore"):
            self.write(self.site / name, "must never be copied")
        self.build()
        for path in self.output.rglob("*"):
            self.assertTrue(set(path.relative_to(self.output).parts).isdisjoint(ignored))
        for name in ("template-display.json", "wrangler.jsonc", ".gitignore"):
            self.assertFalse((self.output / name).exists())

    def test_default_tools_dist_output_is_supported(self):
        output = self.site / ".tools/dist/content"
        self.build(output)
        self.assertTrue((output / "assets/agent-templates.json").is_file())
        self.assertFalse((output / ".tools").exists())
        self.assertEqual((self.site / "README.md").read_text(), self.home)

    def test_validation_failure_preserves_existing_output(self):
        self.build()
        before = self.snapshot(self.output)
        self.write(self.templates / "role-1/template.toml", "invalid [")
        with self.assertRaises(ValueError):
            self.build()
        self.assertEqual(before, self.snapshot(self.output))

    def test_late_staging_failure_preserves_output_and_source_and_cleans_temporary(self):
        self.build()
        before_output = self.snapshot(self.output)
        self.write(self.site / "zh-CN/README.md", "# Missing slot\n")
        before_source = self.snapshot(self.repo)
        with self.assertRaisesRegex(ValueError, "slot"):
            self.build()
        self.assertEqual(before_output, self.snapshot(self.output))
        self.assertEqual(before_source, self.snapshot(self.repo))
        self.assertEqual(list(self.root.glob("template-content-*")), [])

    def test_copy_failure_preserves_existing_output(self):
        self.build()
        before = self.snapshot(self.output)
        external = self.write(self.root / "outside.md", "outside")
        (self.site / "outside.md").symlink_to(external)
        with self.assertRaisesRegex(ValueError, "site symlinks"):
            self.build()
        self.assertEqual(before, self.snapshot(self.output))
        self.assertEqual(list(self.root.glob("template-content-*")), [])

    def test_publication_failure_preserves_existing_output(self):
        self.build()
        before = self.snapshot(self.output)
        with mock.patch.object(BUILDER.shutil, "move",
                               side_effect=OSError("publication failed")):
            with self.assertRaisesRegex(OSError, "publication failed"):
                self.build()
        self.assertTrue(self.output.is_dir(), "Publication failure removed the prior output")
        self.assertEqual(before, self.snapshot(self.output))
        self.assertEqual(list(self.root.glob("template-content-*")), [])

    def test_repeat_build_is_byte_identical_and_removes_stale_output(self):
        self.build()
        first = self.snapshot(self.output)
        self.write(self.output / "stale.md", "left over from old output")
        self.build()
        self.assertEqual(first, self.snapshot(self.output))
        self.assertEqual(list(self.root.glob("template-content-*")), [])

    def test_output_must_not_replace_source_or_ancestors(self):
        before = self.snapshot(self.repo)
        for output in (self.site, self.site.parent, self.repo, self.root):
            with self.subTest(output=output), self.assertRaisesRegex(
                    ValueError, "must not replace source"):
                self.build(output)
        self.assertEqual(before, self.snapshot(self.repo))


if __name__ == "__main__":
    unittest.main()
