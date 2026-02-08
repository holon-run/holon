package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	pkggithub "github.com/holon-run/holon/pkg/github"
)

func TestBuildGoal_SkillModePRReviewUsesGithubReviewSkill(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 564}
	goal := buildGoal("", ref, "pr", "", true, "github-review")

	if !strings.Contains(goal, "Use the github-review skill") {
		t.Fatalf("expected github-review goal, got: %s", goal)
	}
	if strings.Contains(goal, "github-pr-fix") {
		t.Fatalf("expected goal to avoid github-pr-fix for review skill, got: %s", goal)
	}
}

func TestBuildGoal_SkillModePRFixUsesGithubPrFixSkill(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 564}
	goal := buildGoal("", ref, "pr", "", true, "github-pr-fix")

	if !strings.Contains(goal, "Use the github-pr-fix skill") {
		t.Fatalf("expected github-pr-fix goal, got: %s", goal)
	}
}

func TestBuildGoal_SkillModeIssueUsesGithubIssueSolveSkill(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}
	goal := buildGoal("", ref, "issue", "", true, "github-issue-solve")

	if !strings.Contains(goal, "Use the github-issue-solve skill") {
		t.Fatalf("expected github-issue-solve goal, got: %s", goal)
	}
}

func TestPublishResults_SkillFirstMode_RequiresPublishIntent(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Test with missing publish-intent.json - should return error
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err == nil {
		t.Fatal("expected error when publish-intent.json is missing, got nil")
	}
	if !strings.Contains(err.Error(), "publish-intent.json") {
		t.Fatalf("expected error to mention publish-intent.json, got: %v", err)
	}
}

func TestPublishResults_SkillFirstMode_RequiresPublishIntent_PermError(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Create publish-intent.json
	publishIntentPath := filepath.Join(outDir, "publish-intent.json")
	if err := os.WriteFile(publishIntentPath, []byte(`{}`), 0644); err != nil {
		t.Fatalf("failed to create publish-intent.json: %v", err)
	}

	// Test with present publish-intent.json - should succeed
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err != nil {
		t.Fatalf("expected success when publish-intent.json exists, got error: %v", err)
	}
}

func TestPublishResults_SkillFirstMode_SummaryValidation(t *testing.T) {
	ref := &pkggithub.SolveRef{Owner: "holon-run", Repo: "holon", Number: 527}

	// Create a temp output directory
	outDir := t.TempDir()

	// Create publish-intent.json
	publishIntentPath := filepath.Join(outDir, "publish-intent.json")
	if err := os.WriteFile(publishIntentPath, []byte(`{}`), 0644); err != nil {
		t.Fatalf("failed to create publish-intent.json: %v", err)
	}

	// Test with summary.md missing both pr_number and pr_url - should warn but succeed
	summaryPath := filepath.Join(outDir, "summary.md")
	if err := os.WriteFile(summaryPath, []byte("No PR info here"), 0644); err != nil {
		t.Fatalf("failed to create summary.md: %v", err)
	}

	// Capture stderr output
	// (Note: this test doesn't capture stderr, but validates that the function doesn't error)
	err := publishResults(nil, ref, "issue", "", outDir, "auto", true)
	if err != nil {
		t.Fatalf("expected success even with missing PR info in summary.md, got error: %v", err)
	}
}
