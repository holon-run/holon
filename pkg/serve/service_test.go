package serve

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

type fakeExecutor struct {
	events []EventEnvelope
}

func (f *fakeExecutor) HandleEvent(_ context.Context, env EventEnvelope) error {
	f.events = append(f.events, env)
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

func TestNormalizeGitHubEvent_PRReviewComment(t *testing.T) {
	now := func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }
	raw := []byte(`{"event":"pull_request_review_comment","action":"created","repository":{"full_name":"holon-run/holon"},"pull_request":{"number":549},"comment":{"id":321}}`)
	env, err := normalizeGitHubEvent(raw, "", now)
	if err != nil {
		t.Fatalf("normalizeGitHubEvent failed: %v", err)
	}
	if env.Type != "github.pull_request_review_comment.created" {
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
		Handler:  fake,
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
	if len(fake.events) != 1 {
		t.Fatalf("expected 1 forwarded event after dedupe, got %d", len(fake.events))
	}
}

func TestService_Run_WritesState(t *testing.T) {
	td := t.TempDir()
	fake := &fakeExecutor{}
	svc, err := New(Config{
		RepoHint: "holon-run/holon",
		StateDir: td,
		Handler:  fake,
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
	var state struct {
		LastEventID string `json:"last_event_id"`
	}
	if err := json.Unmarshal(stateData, &state); err != nil {
		t.Fatalf("failed to parse state json: %v", err)
	}
	if state.LastEventID != "evt_2" {
		t.Fatalf("unexpected last_event_id: %q", state.LastEventID)
	}
}

func TestNormalizeGitHubEvent_IssueCommentEdited(t *testing.T) {
	now := func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }
	raw := []byte(`{"event":"issue_comment","action":"edited","repository":{"full_name":"holon-run/holon"},"issue":{"number":527}}`)
	env, err := normalizeGitHubEvent(raw, "", now)
	if err != nil {
		t.Fatalf("normalizeGitHubEvent failed: %v", err)
	}
	if env.Type != "github.issue.comment.edited" {
		t.Fatalf("unexpected type: %s", env.Type)
	}
}

func TestNormalizeGitHubEvent_IssuesForPRUsesPRSubject(t *testing.T) {
	now := func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }
	raw := []byte(`{"event":"issues","action":"labeled","repository":{"full_name":"holon-run/holon"},"issue":{"number":579,"pull_request":{"url":"https://api.github.com/repos/holon-run/holon/pulls/579"}},"label":{"name":"needs-review"}}`)
	env, err := normalizeGitHubEvent(raw, "", now)
	if err != nil {
		t.Fatalf("normalizeGitHubEvent failed: %v", err)
	}
	if env.Subject.Kind != "pull_request" || env.Subject.ID != "579" {
		t.Fatalf("unexpected subject: %+v", env.Subject)
	}
	if env.Type != "github.pull_request.labeled" {
		t.Fatalf("unexpected type: %s", env.Type)
	}
}

func TestNormalizeGitHubEvent_HeadersFallbackAndDeliveryDedupe(t *testing.T) {
	now := func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }
	raw := []byte(`{
		"headers": {
			"X-GitHub-Event": "issue_comment",
			"X-GitHub-Delivery": "delivery-1"
		},
		"action":"created",
		"repository":{"full_name":"holon-run/holon"},
		"issue":{"number":527},
		"comment":{"id":987}
	}`)
	env, err := normalizeGitHubEvent(raw, "", now)
	if err != nil {
		t.Fatalf("normalizeGitHubEvent failed: %v", err)
	}
	if env.Type != "github.issue.comment.created" {
		t.Fatalf("unexpected type: %s", env.Type)
	}
	if env.DedupeKey != "github:delivery:delivery-1" {
		t.Fatalf("unexpected dedupe key: %s", env.DedupeKey)
	}
}

func TestService_Run_DedupeAllowsDistinctIssueComments(t *testing.T) {
	td := t.TempDir()
	fake := &fakeExecutor{}
	svc, err := New(Config{
		RepoHint: "holon-run/holon",
		StateDir: td,
		Handler:  fake,
	})
	if err != nil {
		t.Fatalf("New failed: %v", err)
	}
	defer svc.Close()
	svc.now = func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }

	line1 := `{"event":"issue_comment","action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":527},"comment":{"id":1}}`
	line2 := `{"event":"issue_comment","action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":527},"comment":{"id":2}}`
	input := strings.NewReader(line1 + "\n" + line2 + "\n")
	if err := svc.Run(context.Background(), input, 0); err != nil {
		t.Fatalf("Run failed: %v", err)
	}
	if len(fake.events) != 2 {
		t.Fatalf("expected 2 forwarded events, got %d", len(fake.events))
	}
}

func TestService_Run_DedupeSkipsDuplicateDeliveryID(t *testing.T) {
	td := t.TempDir()
	fake := &fakeExecutor{}
	svc, err := New(Config{
		RepoHint: "holon-run/holon",
		StateDir: td,
		Handler:  fake,
	})
	if err != nil {
		t.Fatalf("New failed: %v", err)
	}
	defer svc.Close()
	svc.now = func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }

	line := `{"headers":{"X-GitHub-Event":"issue_comment","X-GitHub-Delivery":"same-delivery"},"action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":527},"comment":{"id":1}}`
	input := strings.NewReader(line + "\n" + line + "\n")
	if err := svc.Run(context.Background(), input, 0); err != nil {
		t.Fatalf("Run failed: %v", err)
	}
	if len(fake.events) != 1 {
		t.Fatalf("expected 1 forwarded event, got %d", len(fake.events))
	}
}

func TestService_Run_DedupeAllowsDistinctPRIssueComments(t *testing.T) {
	td := t.TempDir()
	fake := &fakeExecutor{}
	svc, err := New(Config{
		RepoHint: "holon-run/holon",
		StateDir: td,
		Handler:  fake,
	})
	if err != nil {
		t.Fatalf("New failed: %v", err)
	}
	defer svc.Close()
	svc.now = func() time.Time { return time.Date(2026, 2, 8, 0, 0, 0, 0, time.UTC) }

	line1 := `{"event":"issue_comment","action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":579,"pull_request":{"url":"https://api.github.com/repos/holon-run/holon/pulls/579"}},"comment":{"id":11}}`
	line2 := `{"event":"issue_comment","action":"created","repository":{"full_name":"holon-run/holon"},"issue":{"number":579,"pull_request":{"url":"https://api.github.com/repos/holon-run/holon/pulls/579"}},"comment":{"id":12}}`
	input := strings.NewReader(line1 + "\n" + line2 + "\n")
	if err := svc.Run(context.Background(), input, 0); err != nil {
		t.Fatalf("Run failed: %v", err)
	}
	if len(fake.events) != 2 {
		t.Fatalf("expected 2 forwarded events, got %d", len(fake.events))
	}
}

func TestBuildGitHubDedupeKey_LabelUsesSubjectKind(t *testing.T) {
	envIssue := EventEnvelope{
		Source:  "github",
		Type:    "github.issue.labeled",
		Scope:   EventScope{Repo: "holon-run/holon"},
		Subject: EventSubject{Kind: "issue", ID: "123"},
	}
	envPR := EventEnvelope{
		Source:  "github",
		Type:    "github.pull_request.labeled",
		Scope:   EventScope{Repo: "holon-run/holon"},
		Subject: EventSubject{Kind: "pull_request", ID: "123"},
	}
	payload := map[string]interface{}{
		"label": map[string]interface{}{"name": "needs-review"},
	}

	issueKey := buildGitHubDedupeKey(envIssue, payload)
	prKey := buildGitHubDedupeKey(envPR, payload)
	if issueKey == prKey {
		t.Fatalf("expected distinct keys for issue vs pull_request label events, got %q", issueKey)
	}
}

func TestService_InjectEvent_UsesSameDedupePipeline(t *testing.T) {
	td := t.TempDir()
	fake := &fakeExecutor{}
	svc, err := New(Config{
		RepoHint: "holon-run/holon",
		StateDir: td,
		Handler:  fake,
	})
	if err != nil {
		t.Fatalf("New failed: %v", err)
	}
	defer svc.Close()
	svc.now = func() time.Time { return time.Date(2026, 2, 10, 0, 0, 0, 0, time.UTC) }

	env := EventEnvelope{
		Source: "timer",
		Type:   "timer.tick",
		Scope:  EventScope{Repo: "holon-run/holon"},
		Subject: EventSubject{
			Kind: "timer",
			ID:   "1739145600",
		},
		DedupeKey: "timer:holon-run/holon:1739145600",
	}
	if err := svc.InjectEvent(context.Background(), env); err != nil {
		t.Fatalf("InjectEvent first: %v", err)
	}
	if err := svc.InjectEvent(context.Background(), env); err != nil {
		t.Fatalf("InjectEvent second: %v", err)
	}
	if len(fake.events) != 1 {
		t.Fatalf("expected 1 forwarded event after dedupe, got %d", len(fake.events))
	}
}
