package main

import (
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
