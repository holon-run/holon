use tempfile::tempdir;

use super::test_support::{EnvGuard, MockGithubServer};
use super::*;

mod archive_tests;

const TEST_BUILTINS: &[BuiltinTemplate] = &[BuiltinTemplate {
    template_id: DEFAULT_AGENT_TEMPLATE_ID,
    agents_md: "# Holon Default Agent\n",
    template_toml:
        "schema = 'holon.agent_template.v1'\nid = 'holon-default'\nname = 'Holon Default Agent'\n",
    skill_names: &[],
}];
const TEST_TEMPLATES: TemplateEnvironment =
    TemplateEnvironment::new(&crate::TestIo, TEST_BUILTINS, &[], test_archive_base);

fn test_archive_base() -> String {
    env::var(GITHUB_TEMPLATE_API_BASE_ENV)
        .unwrap_or_else(|_| holon_github::archive::DEFAULT_ARCHIVE_BASE.to_string())
}

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

fn templates_root() -> Result<PathBuf> {
    Ok(templates_root_for_home(&user_home_dir()?))
}

#[test]
fn seed_builtin_templates_is_noop() {
    let home = tempdir().unwrap();
    let templates = templates_root_for_home(home.path());

    seed_builtin_templates_for_home(home.path()).unwrap();

    assert!(!templates.exists());
}

#[test]
fn template_parse_public_layouts_fit_slot_allowance() {
    assert!(std::mem::size_of::<toml::de::DeValue<'_>>() <= 128);
    assert!(std::mem::size_of::<TemplateManifest>() <= 128);
    assert!(std::mem::size_of::<TemplateSkillFileRef>() <= 128);
    assert!(std::mem::size_of::<TemplateSkillRef>() <= 128);
    let spanned_value = toml::de::DeValue::parse("[]").unwrap();
    let spanned_table = toml::de::DeTable::parse("").unwrap();
    assert!(std::mem::size_of_val(&spanned_value) <= 128);
    assert!(std::mem::size_of_val(&spanned_table) <= 128);
}

#[test]
fn discover_agent_templates_catalog_lists_local_templates_without_builtin_entries() {
    let user_home = tempdir().unwrap();
    let agent_home = tempdir().unwrap();
    let user_templates = templates_root_for_home(user_home.path());
    let worker = user_templates.join("worker");
    fs::create_dir_all(&worker).unwrap();
    fs::write(
        worker.join(TEMPLATE_AGENTS_FILENAME),
        "# Worker\n\nDoes worker things\n",
    )
    .unwrap();
    let source_skill = user_home.path().join("source-skill");
    fs::create_dir_all(&source_skill).unwrap();
    fs::write(source_skill.join("SKILL.md"), "# Source\n").unwrap();
    fs::write(
        worker.join(TEMPLATE_SKILLS_FILENAME),
        format!(
            "[[skills]]\nkind = \"local\"\npath = \"{}\"\n",
            source_skill.display()
        ),
    )
    .unwrap();

    let local = agent_home
        .path()
        .join("agent_templates")
        .join("local-agent");
    fs::create_dir_all(&local).unwrap();
    fs::write(
        local.join(TEMPLATE_AGENTS_FILENAME),
        "# Local agent\n\nLocal template\n",
    )
    .unwrap();

    let catalog =
        discover_agent_templates_catalog(TEST_TEMPLATES, Some(user_home.path()), agent_home.path());

    assert!(catalog
        .iter()
        .all(|entry| entry.source != AgentTemplateSourceKind::Builtin));
    assert!(!catalog
        .iter()
        .any(|entry| entry.catalog_id == "builtin:holon-default"));

    let worker_entry = catalog
        .iter()
        .find(|entry| entry.catalog_id == "user_global:worker")
        .unwrap();
    assert_eq!(worker_entry.template, "worker");
    assert_eq!(worker_entry.source, AgentTemplateSourceKind::UserGlobal);
    assert_eq!(worker_entry.path.as_deref(), Some(worker.as_path()));
    assert_eq!(worker_entry.description, "Does worker things");
    assert_eq!(
        worker_entry.included_skills,
        vec![source_skill.display().to_string()]
    );

    let local_entry = catalog
        .iter()
        .find(|entry| entry.catalog_id == "agent_home:local-agent")
        .unwrap();
    assert_eq!(local_entry.template, local.display().to_string());
    assert_eq!(local_entry.path.as_deref(), Some(local.as_path()));
}

#[test]
fn discover_agent_templates_catalog_prefers_agent_home_over_user_global() {
    let user_home = tempdir().unwrap();
    let agent_home = tempdir().unwrap();
    let user_templates = templates_root_for_home(user_home.path());

    let user_worker = user_templates.join("worker");
    fs::create_dir_all(&user_worker).unwrap();
    fs::write(
        user_worker.join(TEMPLATE_AGENTS_FILENAME),
        "# User worker\n\nUser-global worker\n",
    )
    .unwrap();

    let agent_templates = agent_home.path().join("agent_templates");
    let agent_worker = agent_templates.join("worker");
    fs::create_dir_all(&agent_worker).unwrap();
    fs::write(
        agent_worker.join(TEMPLATE_AGENTS_FILENAME),
        "# Agent worker\n\nAgent-home worker\n",
    )
    .unwrap();
    let agent_default = agent_templates.join("holon-default");
    fs::create_dir_all(&agent_default).unwrap();
    fs::write(
        agent_default.join(TEMPLATE_AGENTS_FILENAME),
        "# Agent default\n\nAgent-home default\n",
    )
    .unwrap();

    let catalog =
        discover_agent_templates_catalog(TEST_TEMPLATES, Some(user_home.path()), agent_home.path());

    assert!(catalog
        .iter()
        .any(|entry| entry.catalog_id == "agent_home:worker"));
    assert!(!catalog
        .iter()
        .any(|entry| entry.catalog_id == "user_global:worker"));
    assert!(catalog
        .iter()
        .any(|entry| entry.catalog_id == "agent_home:holon-default"));
    assert!(!catalog
        .iter()
        .any(|entry| entry.catalog_id == "builtin:holon-default"));
}

#[tokio::test]
async fn prefixed_template_selector_resolves_requested_source_when_shadowed() {
    let user_home = tempdir().unwrap();
    let agent_home = tempdir().unwrap();
    let user_templates = templates_root_for_home(user_home.path());

    let user_worker = user_templates.join("worker");
    fs::create_dir_all(&user_worker).unwrap();
    fs::write(
        user_worker.join(TEMPLATE_AGENTS_FILENAME),
        "# User worker\n\nUser-global worker\n",
    )
    .unwrap();

    let agent_worker = agent_home.path().join("agent_templates").join("worker");
    fs::create_dir_all(&agent_worker).unwrap();
    fs::write(
        agent_worker.join(TEMPLATE_AGENTS_FILENAME),
        "# Agent worker\n\nAgent-home worker\n",
    )
    .unwrap();

    let builtin = resolve_template(
        TEST_TEMPLATES,
        "builtin:holon-default",
        user_home.path(),
        agent_home.path(),
    )
    .await
    .unwrap();
    assert!(builtin.agents_md.contains("Holon Default Agent"));

    let user = resolve_template(
        TEST_TEMPLATES,
        "user_global:worker",
        user_home.path(),
        agent_home.path(),
    )
    .await
    .unwrap();
    assert_eq!(user.agents_md, "# User worker\n\nUser-global worker\n");

    let agent = resolve_template(
        TEST_TEMPLATES,
        "agent_home:worker",
        user_home.path(),
        agent_home.path(),
    )
    .await
    .unwrap();
    assert_eq!(agent.agents_md, "# Agent worker\n\nAgent-home worker\n");
}

#[tokio::test]
async fn local_catalog_ids_with_install_suffix_resolve_unchanged() {
    let user_home = tempdir().unwrap();
    let agent_home = tempdir().unwrap();
    let install_id = "code-reviewer@official";

    let user_template = templates_root_for_home(user_home.path()).join(install_id);
    fs::create_dir_all(&user_template).unwrap();
    fs::write(
        user_template.join(TEMPLATE_AGENTS_FILENAME),
        "# User reviewer\n\nOfficial user-global reviewer\n",
    )
    .unwrap();

    let agent_template = agent_home.path().join("agent_templates").join(install_id);
    fs::create_dir_all(&agent_template).unwrap();
    fs::write(
        agent_template.join(TEMPLATE_AGENTS_FILENAME),
        "# Agent reviewer\n\nOfficial agent-home reviewer\n",
    )
    .unwrap();

    let catalog =
        discover_agent_templates_catalog(TEST_TEMPLATES, Some(user_home.path()), agent_home.path());
    assert!(catalog
        .iter()
        .any(|entry| entry.catalog_id == "agent_home:code-reviewer@official"));

    let user = resolve_template(
        TEST_TEMPLATES,
        "user_global:code-reviewer@official",
        user_home.path(),
        agent_home.path(),
    )
    .await
    .unwrap();
    assert_eq!(
        user.agents_md,
        "# User reviewer\n\nOfficial user-global reviewer\n"
    );

    let agent = resolve_template(
        TEST_TEMPLATES,
        "agent_home:code-reviewer@official",
        user_home.path(),
        agent_home.path(),
    )
    .await
    .unwrap();
    assert_eq!(
        agent.agents_md,
        "# Agent reviewer\n\nOfficial agent-home reviewer\n"
    );
}

#[test]
fn local_prefixed_selectors_still_reject_path_like_install_ids() {
    let user_home = tempdir().unwrap();
    let agent_home = tempdir().unwrap();

    let error = resolve_template_catalog_entry(
        TEST_TEMPLATES,
        "user_global:../reviewer@official",
        user_home.path(),
        agent_home.path(),
    )
    .unwrap_err();

    assert!(error
        .to_string()
        .contains("template install_id must not be path-like"));
}

#[test]
fn template_description_skips_multiline_html_comments() {
    assert_eq!(
        template_description("<!--\nmetadata\n-->\n# Heading\n\nVisible description\n"),
        "Visible description"
    );
    assert_eq!(template_description("<!-- hidden --> Visible\n"), "Visible");
}

#[tokio::test]
async fn initialize_agent_home_from_template_id_materializes_agents_md_and_local_skill() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    let templates = templates_root().unwrap();
    fs::create_dir_all(&templates).unwrap();

    let source_skill = home.path().join("source-skill");
    fs::create_dir_all(&source_skill).unwrap();
    fs::write(
        source_skill.join("SKILL.md"),
        "---\nname: source\n---\nbody",
    )
    .unwrap();

    let template_dir = templates.join("worker");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "worker instructions").unwrap();
    fs::write(
        template_dir.join(TEMPLATE_SKILLS_FILENAME),
        format!(
            "[[skills]]\nkind = \"local\"\npath = \"{}\"\n",
            source_skill.display()
        ),
    )
    .unwrap();

    let agent_home = home.path().join("agent");
    let provenance = initialize_agent_home_from_template(TEST_TEMPLATES, &agent_home, "worker")
        .await
        .unwrap();

    assert!(matches!(
        provenance.source,
        TemplateProvenanceSource::TemplateId { .. }
    ));
    let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
    assert!(agents_md.contains("worker instructions"));
    assert!(agents_md.contains("## Holon Agent Home"));
    assert!(agents_md.contains("memory/self.md"));
    assert!(agent_home.join("skills/source-skill/SKILL.md").exists());
    assert!(agent_memory_self_path(&agent_home).is_file());
    assert!(agent_memory_operator_path(&agent_home).is_file());
    assert!(agent_home.join("notes").is_dir());
    assert!(agent_home.join("work").is_dir());
    assert!(agent_home.join("tmp").is_dir());
    assert!(agent_home.join(".holon/state").is_dir());
    assert!(agent_home.join(".holon/ledger").is_dir());
    assert!(agent_home.join(".holon/indexes").is_dir());
    assert!(agent_home.join(".holon/cache").is_dir());
    assert!(template_provenance_path(&agent_home).is_file());
}

#[tokio::test]
async fn catalog_id_initializes_from_agent_home_catalog() {
    let home = tempdir().unwrap();
    let catalog_agent_home = tempdir().unwrap();
    let template_dir = catalog_agent_home
        .path()
        .join("agent_templates")
        .join("worker");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("AGENTS.md"),
        "catalog worker instructions",
    )
    .unwrap();

    let agent_home = home.path().join("agent-catalog-id");
    let provenance = initialize_agent_home_from_template_with_catalog(
        TEST_TEMPLATES,
        &agent_home,
        home.path(),
        catalog_agent_home.path(),
        "agent_home:worker",
    )
    .await
    .unwrap();

    assert!(fs::read_to_string(agent_home.join("AGENTS.md"))
        .unwrap()
        .contains("catalog worker instructions"));
    assert!(matches!(
        provenance.source,
        TemplateProvenanceSource::TemplateId {
            ref template_id,
            ref path
        } if template_id == "worker" && path == &template_dir
    ));

    let bare_agent_home = home.path().join("agent-bare-id");
    initialize_agent_home_from_template_with_catalog(
        TEST_TEMPLATES,
        &bare_agent_home,
        home.path(),
        catalog_agent_home.path(),
        "worker",
    )
    .await
    .unwrap();

    assert!(fs::read_to_string(bare_agent_home.join("AGENTS.md"))
        .unwrap()
        .contains("catalog worker instructions"));
}

#[tokio::test]
async fn template_catalog_selector_error_lists_known_ids() {
    let home = tempdir().unwrap();
    let catalog_agent_home = tempdir().unwrap();
    let template_dir = catalog_agent_home
        .path()
        .join("agent_templates")
        .join("worker");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("AGENTS.md"),
        "catalog worker instructions",
    )
    .unwrap();

    let agent_home = home.path().join("agent");
    let err = initialize_agent_home_from_template_with_catalog(
        TEST_TEMPLATES,
        &agent_home,
        home.path(),
        catalog_agent_home.path(),
        "missing",
    )
    .await
    .unwrap_err();
    let message = err.to_string();

    assert!(message.contains("unknown template selector: missing"));
    assert!(message.contains("known template ids/catalog ids"));
    assert!(message.contains("agent_home:worker"));
    assert!(message.contains("worker"));
    assert!(message.contains(&template_dir.display().to_string()));
    assert!(message.contains("agent_home"));
}

#[tokio::test]
async fn template_without_directory_guidance_still_gets_required_agent_home_guidance() {
    let home = tempdir().unwrap();
    let template_dir = home.path().join("template");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "role only").unwrap();

    let agent_home = home.path().join("agent");
    initialize_agent_home_from_template_with_home(
        TEST_TEMPLATES,
        &agent_home,
        home.path(),
        template_dir.to_str().unwrap(),
    )
    .await
    .unwrap();

    let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
    assert!(agents_md.starts_with("role only"));
    assert!(agents_md.contains("## Holon Agent Home"));
    assert!(agents_md.contains("`.holon/` is runtime-owned"));
    assert!(agents_md.contains("active project workspace"));
}

#[tokio::test]
async fn initialize_agent_home_from_absolute_template_path_works() {
    let home = tempdir().unwrap();
    let template_dir = home.path().join("template");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "absolute template").unwrap();

    let agent_home = home.path().join("agent");
    let provenance = initialize_agent_home_from_template(
        TEST_TEMPLATES,
        &agent_home,
        template_dir.to_str().unwrap(),
    )
    .await
    .unwrap();

    assert!(matches!(
        provenance.source,
        TemplateProvenanceSource::LocalPath { .. }
    ));
    let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
    assert!(agents_md.contains("absolute template"));
    assert!(agents_md.contains("## Holon Agent Home"));
}

#[tokio::test]
async fn ensure_agent_home_agents_md_without_template_writes_blank_agents_md() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    seed_builtin_templates().unwrap();

    let agent_home = home.path().join("agent");
    fs::create_dir_all(&agent_home).unwrap();
    fs::create_dir_all(agent_home.join(".holon/state")).unwrap();
    fs::write(agent_home.join(".holon/state/agent.json"), "{}").unwrap();

    let record = ensure_agent_home_agents_md_without_template_with_home(
        TEST_TEMPLATES,
        &agent_home,
        home.path(),
    )
    .await
    .unwrap()
    .expect("missing AGENTS.md should be materialized");
    assert!(record.selector.is_none());
    assert!(matches!(record.source, TemplateProvenanceSource::Blank));
    let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
    assert!(agents_md.contains("# Agent"));
    assert!(agents_md.contains("## Holon Agent Home"));
    assert!(!agents_md.contains("Holon Default Agent"));
}

#[tokio::test]
async fn ensure_agent_home_agents_md_rolls_back_on_skill_failure() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    let templates = templates_root().unwrap();
    fs::create_dir_all(&templates).unwrap();

    let valid_skill = home.path().join("valid-skill");
    fs::create_dir_all(&valid_skill).unwrap();
    fs::write(valid_skill.join("SKILL.md"), "# Valid Skill\n").unwrap();

    let template_dir = templates.join("broken");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "broken").unwrap();
    fs::write(
            template_dir.join(TEMPLATE_SKILLS_FILENAME),
            format!(
                "[[skills]]\nkind = \"local\"\npath = \"{}\"\n[[skills]]\nkind = \"local\"\npath = \"relative/path\"\n",
                valid_skill.display()
            ),
        )
        .unwrap();

    let agent_home = home.path().join("agent");
    fs::create_dir_all(&agent_home).unwrap();
    fs::create_dir_all(agent_home.join(".holon/state")).unwrap();
    fs::write(agent_home.join(".holon/state/agent.json"), "{}").unwrap();

    let err = ensure_agent_home_agents_md_from_template(TEST_TEMPLATES, &agent_home, "broken")
        .await
        .unwrap_err();

    assert!(err.to_string().contains("absolute"));
    assert!(!agent_home.join("AGENTS.md").exists());
    assert!(!template_provenance_path(&agent_home).exists());
    assert!(!agent_home.join("skills/valid-skill").exists());
    assert!(agent_home.join("skills").is_dir());
}

#[tokio::test]
async fn initialize_agent_home_fails_closed_on_invalid_skill_ref() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    let templates = templates_root().unwrap();
    fs::create_dir_all(&templates).unwrap();
    let template_dir = templates.join("broken");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "broken").unwrap();
    fs::write(
        template_dir.join(TEMPLATE_SKILLS_FILENAME),
        r#"[[skills]]
kind = "local"
path = "relative/path"
"#,
    )
    .unwrap();

    let agent_home = home.path().join("agent");
    let err = initialize_agent_home_from_template(TEST_TEMPLATES, &agent_home, "broken")
        .await
        .unwrap_err();

    assert!(err.to_string().contains("absolute"));
    assert!(!agent_home.exists());
}

#[tokio::test]
async fn initialize_agent_home_restores_preexisting_empty_agent_home_on_failure() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    let templates = templates_root().unwrap();
    fs::create_dir_all(&templates).unwrap();
    let template_dir = templates.join("broken");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "broken").unwrap();
    fs::write(
        template_dir.join(TEMPLATE_SKILLS_FILENAME),
        r#"[[skills]]
kind = "local"
path = "relative/path"
"#,
    )
    .unwrap();

    let agent_home = home.path().join("agent");
    fs::create_dir_all(&agent_home).unwrap();

    let err = initialize_agent_home_from_template(TEST_TEMPLATES, &agent_home, "broken")
        .await
        .unwrap_err();

    assert!(err.to_string().contains("absolute"));
    assert!(agent_home.exists());
    assert!(fs::read_dir(&agent_home).unwrap().next().is_none());
}

#[test]
fn parse_skill_refs_accepts_structured_github_skill_refs() {
    let home = tempdir().unwrap();
    let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
    fs::write(
        &manifest_path,
        r#"[[skills]]
kind = "github"
repo = "holon-run/holon"
path = "skills/ghx"
ref = "main"

[[skills]]
kind = "github"
repo = "owner/repo"
path = "nested/skills/demo"
"#,
    )
    .unwrap();

    let refs = parse_skill_refs(TEST_TEMPLATES, manifest_path).unwrap();
    assert_eq!(refs.len(), 2);
    assert!(matches!(
        &refs[0],
        TemplateSkillRef::Github(TemplateGithubSkillRef::Structured {
            repo,
            path,
            git_ref: Some(git_ref),
        }) if repo == "holon-run/holon" && path == "skills/ghx" && git_ref == "main"
    ));
    assert!(matches!(
        &refs[1],
        TemplateSkillRef::Github(TemplateGithubSkillRef::Structured {
            repo,
            path,
            git_ref: None,
        }) if repo == "owner/repo" && path == "nested/skills/demo"
    ));
}

#[test]
fn parse_skill_refs_accepts_repo_root_github_skill_path() {
    let home = tempdir().unwrap();
    let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
    fs::write(
        &manifest_path,
        r#"[[skills]]
kind = "github"
repo = "blader/humanizer"
path = "."
"#,
    )
    .unwrap();

    let refs = parse_skill_refs(TEST_TEMPLATES, manifest_path).unwrap();
    assert_eq!(refs.len(), 1);
    assert!(matches!(
        &refs[0],
        TemplateSkillRef::Github(TemplateGithubSkillRef::Structured {
            repo,
            path,
            git_ref: None,
        }) if repo == "blader/humanizer" && path == "."
    ));

    let kind = template_github_skill_install_kind(&TemplateGithubSkillRef::Structured {
        repo: "blader/humanizer".into(),
        path: ".".into(),
        git_ref: None,
    })
    .unwrap();
    assert!(matches!(
        kind,
        SkillInstallKind::Remote {
            package,
            skill: None,
            mode: SkillInstallMode::Linked,
        } if package == "https://github.com/blader/humanizer/tree/HEAD/."
    ));
}

#[test]
fn parse_skill_refs_accepts_github_uses_shorthand() {
    let home = tempdir().unwrap();
    let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
    fs::write(
        &manifest_path,
        r#"[[skills]]
kind = "github"
uses = "holon-run/holon/skills/ghx@main"

[[skills]]
kind = "github"
uses = "owner/repo/nested/skills/demo#v1.2.3"

[[skills]]
kind = "github"
uses = "https://github.com/holon-run/holon/tree/main/skills/github-pr-fix"
"#,
    )
    .unwrap();

    let refs = parse_skill_refs(TEST_TEMPLATES, manifest_path).unwrap();
    assert_eq!(refs.len(), 3);
    assert!(matches!(
        &refs[0],
        TemplateSkillRef::Github(TemplateGithubSkillRef::Structured {
            repo,
            path,
            git_ref: Some(git_ref),
        }) if repo == "holon-run/holon" && path == "skills/ghx" && git_ref == "main"
    ));
    assert!(matches!(
        &refs[1],
        TemplateSkillRef::Github(TemplateGithubSkillRef::Structured {
            repo,
            path,
            git_ref: Some(git_ref),
        }) if repo == "owner/repo" && path == "nested/skills/demo" && git_ref == "v1.2.3"
    ));
    assert!(matches!(
        &refs[2],
        TemplateSkillRef::Github(TemplateGithubSkillRef::Structured {
            repo,
            path,
            git_ref: Some(git_ref),
        }) if repo == "holon-run/holon" && path == "skills/github-pr-fix" && git_ref == "main"
    ));
}

#[test]
fn parse_skill_refs_accepts_legacy_github_package_refs() {
    let home = tempdir().unwrap();
    let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
    fs::write(
        &manifest_path,
        r#"[[skills]]
kind = "github"
package = "owner/repo@skill"

[[skills]]
kind = "github"
package = "@scope/package"
"#,
    )
    .unwrap();

    let refs = parse_skill_refs(TEST_TEMPLATES, manifest_path).unwrap();
    assert_eq!(refs.len(), 2);
    assert!(matches!(
        &refs[0],
        TemplateSkillRef::Github(TemplateGithubSkillRef::LegacyPackage { package })
            if package == "owner/repo@skill"
    ));
    assert!(matches!(
        &refs[1],
        TemplateSkillRef::Github(TemplateGithubSkillRef::LegacyPackage { package })
            if package == "@scope/package"
    ));
}

#[test]
fn parse_skill_refs_rejects_builtin_skill_refs_with_migration_hint() {
    let home = tempdir().unwrap();
    let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
    fs::write(
        &manifest_path,
        r#"[[skills]]
kind = "builtin"
name = "ghx"
"#,
    )
    .unwrap();

    let err = parse_skill_refs(TEST_TEMPLATES, manifest_path).unwrap_err();
    let message = format!("{err:?}");
    assert!(message.contains("no longer supports kind = \"builtin\""));
    assert!(message.contains("repo = \"holon-run/holon\""));
    assert!(message.contains("path = \"skills/ghx\""));
}

#[test]
fn template_github_skill_install_kind_uses_structured_tree_url() {
    let kind = template_github_skill_install_kind(&TemplateGithubSkillRef::Structured {
        repo: "holon-run/holon".into(),
        path: "skills/ghx".into(),
        git_ref: Some("main".into()),
    })
    .unwrap();
    assert!(matches!(
        kind,
        SkillInstallKind::Remote {
            package,
            skill: None,
            mode: SkillInstallMode::Linked,
        } if package == "https://github.com/holon-run/holon/tree/main/skills/ghx"
    ));
}

#[test]
fn template_github_skill_install_kind_uses_head_for_omitted_ref() {
    let kind = template_github_skill_install_kind(&TemplateGithubSkillRef::Structured {
        repo: "holon-run/holon".into(),
        path: "skills/ghx".into(),
        git_ref: None,
    })
    .unwrap();
    assert!(matches!(
        kind,
        SkillInstallKind::Remote {
            package,
            skill: None,
            mode: SkillInstallMode::Linked,
        } if package == "https://github.com/holon-run/holon/tree/HEAD/skills/ghx"
    ));
}

#[test]
fn template_github_skill_install_kind_splits_legacy_skill_selector() {
    let kind = template_github_skill_install_kind(&TemplateGithubSkillRef::LegacyPackage {
        package: "owner/repo@skill".into(),
    })
    .unwrap();
    assert!(matches!(
        kind,
        SkillInstallKind::Remote {
            package,
            skill: Some(skill),
            mode: SkillInstallMode::Linked,
        } if package == "owner/repo" && skill == "skill"
    ));

    let scoped = template_github_skill_install_kind(&TemplateGithubSkillRef::LegacyPackage {
        package: "@scope/package".into(),
    })
    .unwrap();
    assert!(matches!(
        scoped,
        SkillInstallKind::Remote {
            package,
            skill: None,
            mode: SkillInstallMode::Linked,
        } if package == "@scope/package"
    ));
}

#[test]
fn parse_skill_refs_rejects_invalid_github_skill_refs() {
    let home = tempdir().unwrap();
    let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
    fs::write(
        &manifest_path,
        r#"[[skills]]
kind = "github"
package = "owner/repo@../bad"
"#,
    )
    .unwrap();
    let err = parse_skill_refs(TEST_TEMPLATES, manifest_path).unwrap_err();
    assert!(err.to_string().contains("plain skill directory name"));
}

#[test]
fn parse_skill_refs_rejects_ambiguous_github_uses_refs() {
    let home = tempdir().unwrap();
    let manifest_path = home.path().join(TEMPLATE_SKILLS_FILENAME);
    fs::write(
        &manifest_path,
        r#"[[skills]]
kind = "github"
uses = "owner/repo@skill"
"#,
    )
    .unwrap();
    let err = parse_skill_refs(TEST_TEMPLATES, manifest_path.clone()).unwrap_err();
    assert!(format!("{err:?}").contains("owner/repo/path@ref"));

    fs::write(
        &manifest_path,
        r#"[[skills]]
kind = "github"
repo = "owner/repo"
path = "skills/demo"
uses = "owner/repo/skills/demo@main"
"#,
    )
    .unwrap();
    let err = parse_skill_refs(TEST_TEMPLATES, manifest_path).unwrap_err();
    assert!(format!("{err:?}").contains("exactly one"));
}

#[test]
fn user_home_dir_falls_back_to_userprofile() {
    let _lock = self::lock_env();
    let profile = tempdir().unwrap();
    let _home = EnvGuard::set("HOME", String::new());
    let _userprofile = EnvGuard::set("USERPROFILE", profile.path().display().to_string());

    assert_eq!(user_home_dir().unwrap(), profile.path());
}

#[test]
fn agent_template_source_kind_remote_label_roundtrip() {
    assert_eq!(AgentTemplateSourceKind::Remote.label(), "remote");
    assert_eq!(
        AgentTemplateSourceKind::from_label("remote"),
        Some(AgentTemplateSourceKind::Remote)
    );
}

#[test]
fn local_catalog_entry_has_name_and_schema_from_template_toml() {
    let home = tempdir().unwrap();
    let templates = templates_root_for_home(home.path());
    let worker = templates.join("worker");
    fs::create_dir_all(&worker).unwrap();
    fs::write(
        worker.join(TEMPLATE_AGENTS_FILENAME),
        "# Worker\n\nDoes worker things\n",
    )
    .unwrap();
    fs::write(
            worker.join(TEMPLATE_MANIFEST_FILENAME),
            "schema = \"holon.agent_template.v1\"\nid = \"worker\"\nname = \"Worker Agent\"\nsummary = \"A worker agent\"\n",
        )
        .unwrap();

    let entries = discover_local_templates(
        TEST_TEMPLATES,
        &templates,
        AgentTemplateSourceKind::UserGlobal,
        false,
    );
    let entry = entries.iter().find(|e| e.template_id == "worker").unwrap();
    assert_eq!(entry.name, "Worker Agent");
    assert_eq!(
        entry.schema_version.as_deref(),
        Some("holon.agent_template.v1")
    );
    assert_eq!(entry.description, "A worker agent");
}

#[test]
fn resolve_agent_template_detail_local_with_skills() {
    let home = tempdir().unwrap();
    let templates = templates_root_for_home(home.path());
    let worker = templates.join("worker");
    fs::create_dir_all(&worker).unwrap();
    fs::write(
        worker.join(TEMPLATE_AGENTS_FILENAME),
        "# Worker\n\nDoes worker things\n",
    )
    .unwrap();
    let skill_dir = home.path().join("my-skill");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "# My Skill\n").unwrap();
    fs::write(
        worker.join(TEMPLATE_SKILLS_FILENAME),
        format!(
            "[[skills]]\nkind = \"local\"\npath = \"{}\"\n",
            skill_dir.display()
        ),
    )
    .unwrap();

    let entries = discover_local_templates(
        TEST_TEMPLATES,
        &templates,
        AgentTemplateSourceKind::UserGlobal,
        false,
    );
    let entry = entries.iter().find(|e| e.template_id == "worker").unwrap();
    let detail = resolve_agent_template_detail(TEST_TEMPLATES, entry).unwrap();
    assert!(!detail.agents_md.is_empty());
    assert_eq!(detail.skills.len(), 1);
    assert_eq!(detail.skills[0].kind, "local");
    assert_eq!(detail.skills[0].reference, skill_dir.display().to_string());
}

// ---- #1984: GitHub repo discovery tests ----

#[test]
fn parse_github_repo_url_basic() {
    let parsed = parse_github_repo_url("https://github.com/owner/repo").unwrap();
    assert_eq!(parsed.owner, "owner");
    assert_eq!(parsed.repo, "repo");
    assert_eq!(parsed.git_ref, None);
}

#[test]
fn parse_github_repo_url_with_ref() {
    let parsed = parse_github_repo_url("https://github.com/owner/repo/tree/dev").unwrap();
    assert_eq!(parsed.owner, "owner");
    assert_eq!(parsed.repo, "repo");
    assert_eq!(parsed.git_ref.as_deref(), Some("dev"));
}

#[test]
fn parse_github_repo_url_rejects_template_tree_url() {
    let err = parse_github_repo_url("https://github.com/owner/repo/tree/main/templates/reviewer")
        .unwrap_err();
    assert!(err.to_string().contains("must be"));
}

#[test]
fn parse_github_repo_url_rejects_non_github() {
    assert!(parse_github_repo_url("https://gitlab.com/owner/repo").is_err());
    assert!(parse_github_repo_url("https://example.com/owner/repo").is_err());
}

#[test]
fn parse_github_repo_url_rejects_query_and_fragment() {
    assert!(parse_github_repo_url("https://github.com/owner/repo?foo=bar").is_err());
    assert!(parse_github_repo_url("https://github.com/owner/repo#section").is_err());
}

#[tokio::test]
async fn github_template_repo_api_preserves_typed_rate_limits() {
    let _lock = self::lock_env();
    let server = MockGithubServer::start(vec![("/repos/owner/repo", 429, "{}".into())]);
    let _base = EnvGuard::set(
        GITHUB_TEMPLATE_API_BASE_ENV,
        format!("http://{}", server.addr),
    );
    let github = GitHubTemplateClient::with_token(Some("test-token".into()))
        .await
        .unwrap();
    let errors = [fetch_default_branch_with_client(&github, "owner", "repo")
        .await
        .unwrap_err()];
    for error in errors {
        assert!(error
            .downcast_ref::<holon_github::client::RateLimit>()
            .is_some());
        assert!(!format!("{error:#}").contains("test-token"));
    }
    assert_eq!(server.requests().len(), 1);
}

/// Build a mock GitHub API server that serves configured responses.
#[tokio::test]
async fn fetch_default_branch_resolves_repo_info() {
    let _lock = self::lock_env();
    let server = MockGithubServer::start(vec![(
        "/repos/owner/repo",
        200,
        serde_json::json!({"default_branch": "develop"}).to_string(),
    )]);
    let _api_guard = EnvGuard::set(
        "HOLON_TEMPLATE_GITHUB_API_BASE",
        format!("http://{}", server.addr),
    );

    let branch = fetch_default_branch("owner", "repo").await.unwrap();
    assert_eq!(branch, "develop");
}

#[tokio::test]
async fn provenance_records_schema_version_from_template_toml() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    let templates = templates_root().unwrap();
    let template_dir = templates.join("versioned");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "versioned template").unwrap();
    fs::write(
        template_dir.join(TEMPLATE_MANIFEST_FILENAME),
        r#"schema = "holon.agent_template.v1"
id = "versioned"
name = "Versioned"
"#,
    )
    .unwrap();

    let agent_home = home.path().join("agent-versioned");
    let provenance = initialize_agent_home_from_template(TEST_TEMPLATES, &agent_home, "versioned")
        .await
        .unwrap();

    assert_eq!(
        provenance.schema_version.as_deref(),
        Some("holon.agent_template.v1")
    );
}

#[tokio::test]
async fn provenance_schema_version_none_when_no_template_toml() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    let templates = templates_root().unwrap();
    let template_dir = templates.join("bare");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "bare template").unwrap();

    let agent_home = home.path().join("agent-bare");
    let provenance = initialize_agent_home_from_template(TEST_TEMPLATES, &agent_home, "bare")
        .await
        .unwrap();

    assert!(provenance.schema_version.is_none());
}

#[tokio::test]
async fn template_modification_after_init_does_not_change_agent() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    let templates = templates_root().unwrap();
    let template_dir = templates.join("mutable");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "original instructions").unwrap();

    let agent_home = home.path().join("agent-mutable");
    initialize_agent_home_from_template(TEST_TEMPLATES, &agent_home, "mutable")
        .await
        .unwrap();

    // Modify the template source after agent initialization.
    fs::write(
        template_dir.join("AGENTS.md"),
        "modified instructions that should not propagate",
    )
    .unwrap();

    let agents_md = fs::read_to_string(agent_home.join("AGENTS.md")).unwrap();
    assert!(agents_md.contains("original instructions"));
    assert!(!agents_md.contains("modified instructions"));
}

#[tokio::test]
async fn re_applying_template_to_existing_agent_fails() {
    let _lock = self::lock_env();
    let home = tempdir().unwrap();
    let _guard = EnvGuard::set("HOME", home.path().display().to_string());
    let templates = templates_root().unwrap();
    let template_dir = templates.join("reapply");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "reapply template").unwrap();

    let agent_home = home.path().join("agent-reapply");
    initialize_agent_home_from_template(TEST_TEMPLATES, &agent_home, "reapply")
        .await
        .unwrap();

    // Second initialization on the now-populated agent home must fail.
    let err = initialize_agent_home_from_template(TEST_TEMPLATES, &agent_home, "reapply")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[test]
fn parsed_github_template_url_parses_tree_url() {
    let url = ParsedGithubTemplateUrl::parse(
        "https://github.com/holon-run/templates/tree/main/my-template",
    )
    .unwrap();
    assert_eq!(url.owner, "holon-run");
    assert_eq!(url.repo, "templates");
    assert_eq!(url.all_after_tree, "main/my-template");
}

#[test]
fn parsed_github_template_url_parses_nested_path() {
    let url = ParsedGithubTemplateUrl::parse(
        "https://github.com/owner/repo/tree/v1.0.0/nested/path/to/template",
    )
    .unwrap();
    assert_eq!(url.owner, "owner");
    assert_eq!(url.repo, "repo");
    assert_eq!(url.all_after_tree, "v1.0.0/nested/path/to/template");
}

#[test]
fn parsed_github_template_url_parses_without_subpath() {
    let url = ParsedGithubTemplateUrl::parse("https://github.com/owner/repo/tree/main").unwrap();
    assert_eq!(url.owner, "owner");
    assert_eq!(url.repo, "repo");
    assert_eq!(url.all_after_tree, "main");
}

#[test]
fn parsed_github_template_url_rejects_non_github_url() {
    assert!(ParsedGithubTemplateUrl::parse("https://gitlab.com/owner/repo").is_err());
    assert!(ParsedGithubTemplateUrl::parse("not-a-url").is_err());
}

#[test]
fn parsed_github_template_url_rejects_missing_tree_segment() {
    assert!(ParsedGithubTemplateUrl::parse("https://github.com/owner/repo").is_err());
}

#[test]
fn remove_user_template_removes_existing_and_reports_missing() {
    let tmp = tempdir().unwrap();
    let templates_root = templates_root_for_home(tmp.path());
    let template_dir = templates_root.join("my-test-template");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(template_dir.join("AGENTS.md"), "# test").unwrap();

    remove_user_template(tmp.path(), "my-test-template").unwrap();
    let err = remove_user_template(tmp.path(), "my-test-template").unwrap_err();
    assert!(!template_dir.exists());

    assert!(err.to_string().contains("not found"));
}

#[cfg(unix)]
#[test]
fn copy_template_dir_rejects_symlinks() {
    use std::os::unix::fs::symlink;
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("AGENTS.md"), "# test").unwrap();
    // Create a symlink pointing outside the template directory.
    symlink("/etc/hostname", src.join("evil")).unwrap();

    let err = copy_template_dir(&src, &dst).unwrap_err();
    assert!(
        err.to_string().contains("symlink"),
        "expected symlink rejection, got: {err}"
    );
}

#[test]
fn validate_github_template_url_parses_and_reports() {
    let result =
        validate_github_template_url("https://github.com/owner/repo/tree/main/templates/dev")
            .unwrap();
    assert_eq!(result, "owner/repo/tree/main/templates/dev");

    assert!(validate_github_template_url("https://gitlab.com/owner/repo").is_err());
}

#[test]
fn parsed_github_template_url_preserves_slash_in_ref() {
    let url = ParsedGithubTemplateUrl::parse(
            "https://github.com/holon-run/holon-test/tree/feat/seed-test-templates/agent_templates/test-developer",
        )
        .unwrap();
    assert_eq!(url.owner, "holon-run");
    assert_eq!(url.repo, "holon-test");
    // all_after_tree preserves the full ref+path for downstream resolution
    assert_eq!(
        url.all_after_tree,
        "feat/seed-test-templates/agent_templates/test-developer"
    );
}

#[test]
fn validate_github_template_url_preserves_slash_in_ref() {
    let result = validate_github_template_url(
        "https://github.com/owner/repo/tree/feat/branch-name/templates/dev",
    )
    .unwrap();
    // Canonical form preserves the full ref+path since we can't resolve
    // the ref/path boundary without an API call.
    assert_eq!(result, "owner/repo/tree/feat/branch-name/templates/dev");
}
