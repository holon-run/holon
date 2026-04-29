use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;

use crate::{
    config::AppConfig,
    run_once::{run_once, RunFinalStatus, RunOnceRequest, RunOnceResponse},
    types::TrustLevel,
};

pub const DEFAULT_SOLVE_AGENT_ID: &str = "github-solve";
pub const DEFAULT_SOLVE_TEMPLATE_ID: &str = "holon-github-solve";

#[derive(Debug, Clone)]
pub struct SolveRequest {
    pub target_ref: String,
    pub repo: Option<String>,
    pub base: Option<String>,
    pub goal: Option<String>,
    pub role: Option<String>,
    pub agent_id: Option<String>,
    pub template: Option<String>,
    pub max_turns: Option<u64>,
    pub trust: TrustLevel,
    pub json: bool,
    pub workspace_root: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub input_dir: Option<PathBuf>,
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitHubTarget {
    pub repo: String,
    pub number: u64,
    pub kind: GitHubTargetKind,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubTargetKind {
    IssueOrPullRequest,
    Issue,
    PullRequest,
}

#[derive(Debug, Serialize)]
struct SolveManifest<'a> {
    provider: &'static str,
    status: &'a str,
    outcome: &'a str,
    target_ref: &'a str,
    target: Option<&'a GitHubTarget>,
    run_json: Option<String>,
    summary: Option<String>,
}

pub async fn run_solve(config: AppConfig, request: SolveRequest) -> Result<RunOnceResponse> {
    let output_dir = prepare_output_dir(request.output_dir.as_deref())?;
    let input_dir = prepare_input_dir(request.input_dir.as_deref(), &output_dir)?;
    export_solve_env(&input_dir, &output_dir);

    let target = parse_github_target(&request.target_ref, request.repo.as_deref())?;
    write_input_metadata(&input_dir, &request, target.as_ref())?;

    let prompt = build_solve_prompt(&request, target.as_ref(), &output_dir);
    let agent_id = request
        .agent_id
        .clone()
        .unwrap_or_else(|| DEFAULT_SOLVE_AGENT_ID.to_string());
    let template = request
        .template
        .clone()
        .unwrap_or_else(|| DEFAULT_SOLVE_TEMPLATE_ID.to_string());

    let run_request = RunOnceRequest {
        text: prompt,
        trust: request.trust.clone(),
        agent_id: Some(agent_id.clone()),
        create_agent: true,
        template: Some(template),
        max_turns: request.max_turns,
        wait_for_tasks: true,
        workspace_root: request.workspace_root.clone(),
        cwd: request.cwd.clone(),
    };

    let response = match run_once(config.clone(), run_request.clone()).await {
        Ok(response) => response,
        Err(err) if err.to_string().contains("already exists") => {
            let retry = RunOnceRequest {
                create_agent: false,
                template: None,
                ..run_request
            };
            run_once(config, retry).await?
        }
        Err(err) => return Err(err),
    };

    write_run_artifacts(&output_dir, &request, target.as_ref(), &response)?;
    Ok(response)
}

fn prepare_output_dir(output_dir: Option<&Path>) -> Result<PathBuf> {
    let path = output_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("holon-output-{}", uuid::Uuid::new_v4()))
    });
    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create output directory {}", path.display()))?;
    Ok(path)
}

fn prepare_input_dir(input_dir: Option<&Path>, output_dir: &Path) -> Result<PathBuf> {
    let path = input_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| output_dir.join("github-context"));
    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create input directory {}", path.display()))?;
    Ok(path)
}

fn export_solve_env(input_dir: &Path, output_dir: &Path) {
    std::env::set_var("GITHUB_INPUT_DIR", input_dir);
    std::env::set_var("GITHUB_CONTEXT_DIR", input_dir);
    std::env::set_var("GITHUB_OUTPUT_DIR", output_dir);
}

pub fn parse_github_target(
    target_ref: &str,
    repo_hint: Option<&str>,
) -> Result<Option<GitHubTarget>> {
    let trimmed = target_ref.trim();
    if trimmed.is_empty() {
        bail!("solve ref must not be empty");
    }

    if let Some((repo, number, kind)) = parse_github_url(trimmed)? {
        return Ok(Some(GitHubTarget {
            repo: repo.clone(),
            number,
            kind,
            url: target_url(&repo, number, kind),
        }));
    }

    if let Some(number) = trimmed.strip_prefix('#') {
        let repo = repo_hint.ok_or_else(|| anyhow!("numeric ref requires --repo"))?;
        validate_repo(repo)?;
        let number = parse_number(number)?;
        return Ok(Some(GitHubTarget {
            repo: repo.to_string(),
            number,
            kind: GitHubTargetKind::IssueOrPullRequest,
            url: target_url(repo, number, GitHubTargetKind::IssueOrPullRequest),
        }));
    }

    if let Some((repo, number)) = trimmed.split_once('#') {
        let repo = repo.trim();
        validate_repo(repo)?;
        let number = parse_number(number)?;
        return Ok(Some(GitHubTarget {
            repo: repo.to_string(),
            number,
            kind: GitHubTargetKind::IssueOrPullRequest,
            url: target_url(repo, number, GitHubTargetKind::IssueOrPullRequest),
        }));
    }

    Ok(None)
}

fn parse_github_url(raw: &str) -> Result<Option<(String, u64, GitHubTargetKind)>> {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return Ok(None);
    };
    if url.host_str() != Some("github.com") {
        return Ok(None);
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    if segments.len() < 4 {
        return Ok(None);
    }
    let repo = format!("{}/{}", segments[0], segments[1]);
    validate_repo(&repo)?;
    let kind = match segments[2] {
        "issues" => GitHubTargetKind::Issue,
        "pull" => GitHubTargetKind::PullRequest,
        _ => return Ok(None),
    };
    Ok(Some((repo, parse_number(segments[3])?, kind)))
}

fn validate_repo(repo: &str) -> Result<()> {
    let parts = repo.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|part| part.trim().is_empty()) {
        bail!("repo must be in owner/repo format: {repo}");
    }
    Ok(())
}

fn parse_number(raw: &str) -> Result<u64> {
    raw.trim()
        .parse::<u64>()
        .with_context(|| format!("invalid GitHub issue/PR number: {raw}"))
}

fn target_url(repo: &str, number: u64, kind: GitHubTargetKind) -> String {
    let path = match kind {
        GitHubTargetKind::IssueOrPullRequest | GitHubTargetKind::Issue => "issues",
        GitHubTargetKind::PullRequest => "pull",
    };
    format!("https://github.com/{repo}/{path}/{number}")
}

pub fn build_solve_prompt(
    request: &SolveRequest,
    target: Option<&GitHubTarget>,
    output_dir: &Path,
) -> String {
    let mut sections = Vec::new();
    sections.push(
        "You are running Holon solve, a GitHub task preset built on top of holon run.".to_string(),
    );

    match target {
        Some(target) => {
            sections.push(format!(
                "Target: {} #{} ({})\nURL: {}",
                target.repo,
                target.number,
                match target.kind {
                    GitHubTargetKind::IssueOrPullRequest => "issue_or_pull_request",
                    GitHubTargetKind::Issue => "issue",
                    GitHubTargetKind::PullRequest => "pull_request",
                },
                target.url
            ));
        }
        None => sections.push(format!("Target ref: {}", request.target_ref)),
    }

    if let Some(base) = request.base.as_deref().filter(|value| !value.is_empty()) {
        sections.push(format!("Base branch hint: {base}"));
    }
    if let Some(role) = request.role.as_deref().filter(|value| !value.is_empty()) {
        sections.push(format!("Role hint: {role}"));
    }
    if let Some(goal) = request.goal.as_deref().filter(|value| !value.is_empty()) {
        sections.push(format!("User goal:\n{goal}"));
    } else {
        sections.push(default_goal(target));
    }

    sections.push(format!(
        "Output contract:\n- Write a concise human summary to {}/summary.md.\n- Write machine-readable execution status to {}/manifest.json.\n- Include any PR URL, comment URL, branch, commit SHA, verification commands, and residual blockers when available.",
        output_dir.display(),
        output_dir.display()
    ));
    sections.push(
        "GitHub operating rules:\n- Use GITHUB_TOKEN or GH_TOKEN when publishing through gh.\n- The repository checkout is already prepared by the caller; do not clone by default.\n- If code changes are required, create or reuse an appropriate branch, commit intentionally, push, and create or update a PR yourself.\n- If this is a review-only task, publish one structured review or comment only when the requested skill workflow calls for it.\n- Prefer the github-issue-solve, github-pr-fix, github-review, and ghx skills when their descriptions match the target."
            .to_string(),
    );
    sections.join("\n\n")
}

fn default_goal(target: Option<&GitHubTarget>) -> String {
    match target.map(|target| target.kind) {
        Some(GitHubTargetKind::PullRequest) => {
            "Default goal: fix or review the target pull request according to the trigger context.".to_string()
        }
        Some(GitHubTargetKind::Issue) => {
            "Default goal: solve the target issue end to end and publish the result.".to_string()
        }
        _ => {
            "Default goal: inspect the target, determine whether it is an issue or pull request, then choose the appropriate GitHub skill workflow.".to_string()
        }
    }
}

fn write_input_metadata(
    input_dir: &Path,
    request: &SolveRequest,
    target: Option<&GitHubTarget>,
) -> Result<()> {
    #[derive(Serialize)]
    struct InputMetadata<'a> {
        target_ref: &'a str,
        target: Option<&'a GitHubTarget>,
        repo_hint: Option<&'a str>,
        base: Option<&'a str>,
        role: Option<&'a str>,
        goal: Option<&'a str>,
    }

    let metadata = InputMetadata {
        target_ref: &request.target_ref,
        target,
        repo_hint: request.repo.as_deref(),
        base: request.base.as_deref(),
        role: request.role.as_deref(),
        goal: request.goal.as_deref(),
    };
    let content = serde_json::to_vec_pretty(&metadata)?;
    fs::write(input_dir.join("solve.json"), content)
        .with_context(|| format!("failed to write {}", input_dir.join("solve.json").display()))
}

fn write_run_artifacts(
    output_dir: &Path,
    request: &SolveRequest,
    target: Option<&GitHubTarget>,
    response: &RunOnceResponse,
) -> Result<()> {
    let run_json_path = output_dir.join("run.json");
    fs::write(&run_json_path, serde_json::to_vec_pretty(response)?)
        .with_context(|| format!("failed to write {}", run_json_path.display()))?;

    let summary_path = output_dir.join("summary.md");
    if !summary_path.exists() {
        fs::write(&summary_path, response.render_text())
            .with_context(|| format!("failed to write {}", summary_path.display()))?;
    }

    let manifest_path = output_dir.join("manifest.json");
    if !manifest_path.exists() {
        let status = match response.final_status {
            RunFinalStatus::Completed => "completed",
            RunFinalStatus::Waiting => "waiting",
            RunFinalStatus::Failed => "failed",
            RunFinalStatus::MaxTurnsExceeded => "max_turns_exceeded",
        };
        let outcome = if response.final_status == RunFinalStatus::Completed {
            "success"
        } else {
            "incomplete"
        };
        let manifest = SolveManifest {
            provider: "holon-solve",
            status,
            outcome,
            target_ref: &request.target_ref,
            target,
            run_json: Some(run_json_path.display().to_string()),
            summary: Some(summary_path.display().to_string()),
        };
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)
            .with_context(|| format!("failed to write {}", manifest_path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(target_ref: &str) -> SolveRequest {
        SolveRequest {
            target_ref: target_ref.to_string(),
            repo: None,
            base: Some("main".into()),
            goal: None,
            role: None,
            agent_id: None,
            template: None,
            max_turns: Some(1),
            trust: TrustLevel::TrustedOperator,
            json: true,
            workspace_root: None,
            cwd: None,
            input_dir: None,
            output_dir: None,
        }
    }

    #[test]
    fn parse_owner_repo_number_ref() {
        let target = parse_github_target("holon-run/holon#123", None)
            .unwrap()
            .unwrap();
        assert_eq!(target.repo, "holon-run/holon");
        assert_eq!(target.number, 123);
        assert_eq!(target.kind, GitHubTargetKind::IssueOrPullRequest);
    }

    #[test]
    fn parse_github_pull_url() {
        let target = parse_github_target("https://github.com/holon-run/holon/pull/753", None)
            .unwrap()
            .unwrap();
        assert_eq!(target.repo, "holon-run/holon");
        assert_eq!(target.number, 753);
        assert_eq!(target.kind, GitHubTargetKind::PullRequest);
        assert_eq!(target.url, "https://github.com/holon-run/holon/pull/753");
    }

    #[test]
    fn build_prompt_contains_github_skill_contract() {
        let request = request("holon-run/holon#123");
        let target = parse_github_target(&request.target_ref, None).unwrap();
        let prompt = build_solve_prompt(&request, target.as_ref(), Path::new("/tmp/out"));
        assert!(prompt.contains("github-issue-solve"));
        assert!(prompt.contains("github-pr-fix"));
        assert!(prompt.contains("github-review"));
        assert!(prompt.contains("/tmp/out/manifest.json"));
    }
}
