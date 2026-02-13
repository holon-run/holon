package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	pkggithub "github.com/holon-run/holon/pkg/github"
)

func TestBuildGoal_SkillModePRReviewUsesGithubReviewSkill(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 564}
	goal := buildGoal("", ref, "pr", "", "github-review")

	if !strings.Contains(goal, "Use the github-review skill") {
		t.Fatalf("expected github-review goal, got: %s", goal)
	}
	if strings.Contains(goal, "github-pr-fix") {
		t.Fatalf("expected goal to avoid github-pr-fix for review skill, got: %s", goal)
	}
}

func TestBuildGoal_SkillModePRFixUsesGithubPrFixSkill(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 564}
	goal := buildGoal("", ref, "pr", "", "github-pr-fix")

	if !strings.Contains(goal, "Use the github-pr-fix skill") {
		t.Fatalf("expected github-pr-fix goal, got: %s", goal)
	}
}

func TestBuildGoal_SkillModeIssueUsesGithubIssueSolveSkill(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}
	goal := buildGoal("", ref, "issue", "", "github-issue-solve")

	if !strings.Contains(goal, "Use the github-issue-solve skill") {
		t.Fatalf("expected github-issue-solve goal, got: %s", goal)
	}
	// Verify it uses generic manifest contract, not publish-intent.json
	if strings.Contains(goal, "publish-intent.json") {
		t.Fatalf("goal should not mention publish-intent.json, got: %s", goal)
	}
	// Verify it mentions manifest status/outcome validation
	if !strings.Contains(goal, "status='completed'") || !strings.Contains(goal, "outcome='success'") {
		t.Fatalf("goal should mention manifest status/outcome validation, got: %s", goal)
	}
}

func TestPrepareWorkspaceForSolve_WithWorkspace_UsesDirectWorkspace(t *testing.T) {
	userWorkspace := t.TempDir()
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 627}

	oldWorkspace := solveWorkspace
	oldWorkspaceRef := solveWorkspaceRef
	oldWorkspaceHistory := solveWorkspaceHistory
	oldFetchRemote := solveFetchRemote
	defer func() {
		solveWorkspace = oldWorkspace
		solveWorkspaceRef = oldWorkspaceRef
		solveWorkspaceHistory = oldWorkspaceHistory
		solveFetchRemote = oldFetchRemote
	}()

	solveWorkspace = userWorkspace
	solveWorkspaceRef = ""
	solveWorkspaceHistory = ""
	solveFetchRemote = false

	prep, err := prepareWorkspaceForSolve(context.Background(), ref, "")
	if err != nil {
		t.Fatalf("prepareWorkspaceForSolve() error = %v", err)
	}
	if prep.path != userWorkspace {
		t.Fatalf("workspace path = %q, want %q", prep.path, userWorkspace)
	}
	if prep.cleanupNeeded {
		t.Fatalf("cleanupNeeded = true, want false for user-provided workspace")
	}
	if !prep.useDirect {
		t.Fatalf("useDirect = false, want true for user-provided workspace")
	}
}

func TestPublishResults_SkillFirstMode_MissingManifestWarning(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Test with missing manifest.json - should succeed with warning
	// In skill-first mode, missing manifest.json is not a blocking error
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err != nil {
		t.Fatalf("expected success when manifest.json is missing (skill-first mode), got: %v", err)
	}
	// The function should complete without error, logging a warning instead
}

func TestPublishResults_SkillFirstMode_ValidatesManifestStatus(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Create manifest.json with failed status
	manifestPath := filepath.Join(outDir, "manifest.json")
	manifestContent := `{"status": "completed", "outcome": "failure", "duration": "1s", "artifacts": []}`
	if err := os.WriteFile(manifestPath, []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to create manifest.json: %v", err)
	}

	// Test with failed outcome - should error
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err == nil {
		t.Fatal("expected error when manifest outcome is failure, got nil")
	}
	if !strings.Contains(err.Error(), "outcome") {
		t.Fatalf("expected error to mention outcome, got: %v", err)
	}
}

func TestPublishResults_SkillFirstMode_SuccessWithManifest(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Create manifest.json with success status
	manifestPath := filepath.Join(outDir, "manifest.json")
	manifestContent := `{"status": "completed", "outcome": "success", "duration": "1s", "artifacts": []}`
	if err := os.WriteFile(manifestPath, []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to create manifest.json: %v", err)
	}

	// Test with valid manifest - should succeed (even without publish evidence, this just warns)
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err != nil {
		t.Fatalf("expected success when manifest is valid, got error: %v", err)
	}
}

func TestPublishResults_SkillFirstMode_ReviewSkillNoPublishIntentRequired(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 123}

	// Create a temp output directory
	outDir := t.TempDir()

	// Create manifest.json for github-review skill (successful, no publish-intent needed)
	manifestPath := filepath.Join(outDir, "manifest.json")
	manifestContent := `{
		"status": "completed",
		"outcome": "success",
		"duration": "5s",
		"artifacts": ["review.md", "review.json", "summary.md"],
		"metadata": {
			"provider": "github-review",
			"pr_ref": "holon-run/holon#123",
			"findings_count": 3
		}
	}`
	if err := os.WriteFile(manifestPath, []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to create manifest.json: %v", err)
	}

	// Test with PR ref type - should succeed without publish-intent.json
	// github-review posts reviews, not PRs, so no PR publish evidence expected
	err := publishResults(nil, ref, "pr", "", outDir, "auto", true)
	if err != nil {
		t.Fatalf("expected success for review skill without publish-intent.json, got error: %v", err)
	}
}

func TestPublishResults_SkillFirstMode_IssueSolveWithPublishEvidence(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Create manifest.json with PR publish evidence
	manifestPath := filepath.Join(outDir, "manifest.json")
	manifestContent := `{
		"status": "completed",
		"outcome": "success",
		"duration": "10s",
		"artifacts": ["diff.patch", "summary.md"],
		"metadata": {
			"provider": "github-issue-solve",
			"issue_ref": "holon-run/holon#527",
			"pr_number": 123,
			"pr_url": "https://github.com/holon-run/holon/pull/123"
		}
	}`
	if err := os.WriteFile(manifestPath, []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to create manifest.json: %v", err)
	}

	// Test with issue ref type and publish evidence - should succeed
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err != nil {
		t.Fatalf("expected success with publish evidence in manifest, got error: %v", err)
	}
}

func TestPublishResults_SkillFirstMode_PublishEvidenceInSummary(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Create manifest.json (without PR metadata)
	manifestPath := filepath.Join(outDir, "manifest.json")
	manifestContent := `{
		"status": "completed",
		"outcome": "success",
		"duration": "10s",
		"artifacts": ["diff.patch", "summary.md"]
	}`
	if err := os.WriteFile(manifestPath, []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to create manifest.json: %v", err)
	}

	// Create summary.md with PR evidence (backward compatibility)
	summaryPath := filepath.Join(outDir, "summary.md")
	summaryContent := "## Summary\n\nCreated PR #123\n\nPR URL: https://github.com/holon-run/holon/pull/123\npr_number: 123\npr_url: https://github.com/holon-run/holon/pull/123"
	if err := os.WriteFile(summaryPath, []byte(summaryContent), 0644); err != nil {
		t.Fatalf("failed to create summary.md: %v", err)
	}

	// Test with publish evidence in summary - should succeed
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err != nil {
		t.Fatalf("expected success with publish evidence in summary, got error: %v", err)
	}
}

func TestPublishResults_SkillFirstMode_MalformedManifestErrors(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Create a malformed manifest.json (invalid JSON)
	manifestPath := filepath.Join(outDir, "manifest.json")
	manifestContent := `{invalid json this is not valid`
	if err := os.WriteFile(manifestPath, []byte(manifestContent), 0644); err != nil {
		t.Fatalf("failed to create manifest.json: %v", err)
	}

	// Test with malformed manifest - should error (unlike missing manifest)
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err == nil {
		t.Fatal("expected error when manifest.json is malformed, got nil")
	}
	if !strings.Contains(err.Error(), "malformed") {
		t.Fatalf("expected error to mention malformed manifest, got: %v", err)
	}
}
