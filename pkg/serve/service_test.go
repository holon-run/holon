package serve

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

type fakeExecutor struct {
	actions []ActionIntent
}

func (f *fakeExecutor) Execute(_ context.Context, action ActionIntent) error {
	f.actions = append(f.actions, action)
	return nil
}

func TestNormalizeGitHubEvent_IssueComment(t *testing.T) {
	now := func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }
	raw := []byte(`{"event":"issue_comment","action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":527},"comment":{"body":"@holonbot hi"}}`)
	env, err := normalizeGitHubEvent(raw, "", now)
	if err != nil {
		t.Fatalf("normalizeGitHubEvent failed: %v", err)
	}
	if env.Type != "github.issue.comment.created" {
		t.Fatalf("unexpected type: %s", env.Type)
	}
	if env.Scope.Repo != "holon-run/holon" {
		t.Fatalf("unexpected repo: %s", env.Scope.Repo)
	}
	if env.Subject.Kind != "issue" || env.Subject.ID != "527" {
		t.Fatalf("unexpected subject: %+v", env.Subject)
	}
}

func TestNormalizeGitHubEvent_PRReview(t *testing.T) {
	now := func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }
	raw := []byte(`{"event":"pull_request_review","action":"submitted","repository":{"full_name":"holon-run/holon"},"number":549,"review":{"state":"changes_requested"}}`)
	env, err := normalizeGitHubEvent(raw, "", now)
	if err != nil {
		t.Fatalf("normalizeGitHubEvent failed: %v", err)
	}
	if env.Type != "github.pull_request_review.submitted" {
		t.Fatalf("unexpected type: %s", env.Type)
	}
	if env.Subject.Kind != "pull_request" || env.Subject.ID != "549" {
		t.Fatalf("unexpected subject: %+v", env.Subject)
	}
}

func TestService_Run_Dedupe(t *testing.T) {
	td := t.TempDir()
	fake := &fakeExecutor{}
	svc, err := New(Config{
		RepoHint: "holon-run/holon",
		StateDir: td,
		Exec:     fake,
	})
	if err != nil {
		t.Fatalf("New failed: %v", err)
	}
	defer svc.Close()
	svc.now = func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }

	line := `{"id":"evt_1","source":"github","type":"github.issue.opened","scope":{"repo":"holon-run/holon"},"subject":{"kind":"issue","id":"527"},"dedupe_key":"same"}`
	input := strings.NewReader(line + "\n" + line + "\n")
	if err := svc.Run(context.Background(), input, 0); err != nil {
		t.Fatalf("Run failed: %v", err)
	}
	if len(fake.actions) != 1 {
		t.Fatalf("expected 1 executed action after dedupe, got %d", len(fake.actions))
	}
}

func TestService_Run_WritesState(t *testing.T) {
	td := t.TempDir()
	fake := &fakeExecutor{}
	svc, err := New(Config{
		RepoHint: "holon-run/holon",
		StateDir: td,
		Exec:     fake,
	})
	if err != nil {
		t.Fatalf("New failed: %v", err)
	}
	defer svc.Close()
	svc.now = func() time.Time { return time.Date(2026, 2, 8, 1, 2, 3, 0, time.UTC) }

	input := strings.NewReader(`{"id":"evt_2","source":"github","type":"github.pull_request.opened","scope":{"repo":"holon-run/holon"},"subject":{"kind":"pull_request","id":"550"},"dedupe_key":"k2"}` + "\n")
	if err := svc.Run(context.Background(), input, 0); err != nil {
		t.Fatalf("Run failed: %v", err)
	}
	stateData, err := os.ReadFile(filepath.Join(td, "serve-state.json"))
	if err != nil {
		t.Fatalf("failed to read state file: %v", err)
	}
	if !strings.Contains(string(stateData), `"last_event_id": "evt_2"`) {
		t.Fatalf("unexpected state content: %s", string(stateData))
	}
}
