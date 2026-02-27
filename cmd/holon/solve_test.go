package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	pkggithub "github.com/holon-run/holon/pkg/github"
)

func TestBuildPRGoal_ReviewIntent(t *testing.T) {
	goal := buildPRGoal("https://github.com/holon-run/holon/pull/564", "review")

	if !strings.Contains(goal, "Review the PR") {
		t.Fatalf("expected review goal, got: %s", goal)
	}
	if strings.Contains(strings.ToLower(goal), "fix the pr") {
		t.Fatalf("expected review goal to avoid fix wording, got: %s", goal)
	}
}

func TestInferRefTypeFromURL(t *testing.T) {
	tests := []struct {
		name     string
		refStr   string
		solveRef *pkggithub.SolveRef
		wantType string
		wantOK   bool
	}{
		{
			name:   "issue url",
			refStr: "https://github.com/holon-run/holon/issues/123",
			solveRef: &pkggithub.SolveRef{
				Owner:  "holon-run",
				Repo:   "holon",
				Number: 123,
				Type:   pkggithub.SolveRefTypeIssue,
			},
			wantType: "issue",
			wantOK:   true,
		},
		{
			name:   "pr url",
			refStr: "https://github.com/holon-run/holon/pull/456",
			solveRef: &pkggithub.SolveRef{
				Owner:  "holon-run",
				Repo:   "holon",
				Number: 456,
				Type:   pkggithub.SolveRefTypePR,
			},
			wantType: "pr",
			wantOK:   true,
		},
		{
			name:   "url with leading and trailing spaces",
			refStr: "  https://github.com/holon-run/holon/issues/789  ",
			solveRef: &pkggithub.SolveRef{
				Owner:  "holon-run",
				Repo:   "holon",
				Number: 789,
				Type:   pkggithub.SolveRefTypeIssue,
			},
			wantType: "issue",
			wantOK:   true,
		},
		{
			name:   "short ref is ambiguous",
			refStr: "holon-run/holon#456",
			solveRef: &pkggithub.SolveRef{
				Owner:  "holon-run",
				Repo:   "holon",
				Number: 456,
				Type:   pkggithub.SolveRefTypePR,
			},
			wantType: "",
			wantOK:   false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gotType, gotOK := inferRefTypeFromURL(tt.refStr, tt.solveRef)
			if gotOK != tt.wantOK {
				t.Fatalf("inferRefTypeFromURL() ok = %v, want %v", gotOK, tt.wantOK)
			}
			if gotType != tt.wantType {
				t.Fatalf("inferRefTypeFromURL() type = %q, want %q", gotType, tt.wantType)
			}
		})
	}
}

func TestBuildPRGoal_FixIntent(t *testing.T) {
	goal := buildPRGoal("https://github.com/holon-run/holon/pull/564", "fix")

	if !strings.Contains(strings.ToLower(goal), "fix the pr") {
		t.Fatalf("expected fix goal, got: %s", goal)
	}
}

func TestBuildIssueGoal(t *testing.T) {
	goal := buildIssueGoal("https://github.com/holon-run/holon/issues/527")

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

	prep, err := prepareWorkspaceForSolve(context.Background(), ref, "", t.TempDir())
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

func TestPublishResults_SkillFirstMode_MissingManifestErrors(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Test with missing manifest.json - should fail in manifest-first mode
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err == nil {
		t.Fatal("expected error when manifest.json is missing, got nil")
	}
	if !strings.Contains(err.Error(), "manifest.json is required but missing") {
		t.Fatalf("expected missing manifest error, got: %v", err)
	}
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
