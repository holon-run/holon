package serve

import (
	"context"
	"errors"
	"strings"
	"testing"
)

func TestSelectWebhookForwardTargetFlag(t *testing.T) {
	tests := []struct {
		name       string
		helpOutput string
		want       string
		wantErr    bool
	}{
		{
			name:       "prefers url when both exist",
			helpOutput: "Flags:\n  -U, --url string\n  -p, --port int\n",
			want:       webhookTargetFlagURL,
		},
		{
			name:       "accepts legacy port",
			helpOutput: "Flags:\n  -p, --port int\n",
			want:       webhookTargetFlagPort,
		},
		{
			name:       "errors when neither exists",
			helpOutput: "Flags:\n  -h, --help\n",
			wantErr:    true,
		},
	}

	for _, tc := range tests {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			got, err := selectWebhookForwardTargetFlag(tc.helpOutput)
			if tc.wantErr {
				if err == nil {
					t.Fatalf("expected error, got nil")
				}
				return
			}
			if err != nil {
				t.Fatalf("selectWebhookForwardTargetFlag() error = %v", err)
			}
			if got != tc.want {
				t.Fatalf("selectWebhookForwardTargetFlag() = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestWebhookURLsEquivalent(t *testing.T) {
	if !webhookURLsEquivalent("http://127.0.0.1:8080/ingress/github/webhook", "http://localhost:8080/ingress/github/webhook/") {
		t.Fatalf("expected localhost and 127.0.0.1 webhook URLs to be equivalent")
	}
	if webhookURLsEquivalent("http://127.0.0.1:8080/ingress/github/webhook", "http://127.0.0.1:8081/ingress/github/webhook") {
		t.Fatalf("expected different ports to be non-equivalent")
	}
}

func TestBuildExistingHookRemediation_MatchedHook(t *testing.T) {
	orig := listGitHubRepoHooks
	t.Cleanup(func() { listGitHubRepoHooks = orig })

	listGitHubRepoHooks = func(_ context.Context, repo string) ([]githubRepoHook, error) {
		if repo != "holon-run/holon" {
			t.Fatalf("unexpected repo: %s", repo)
		}
		hook := githubRepoHook{ID: 12345}
		hook.Config.URL = "http://localhost:8080/ingress/github/webhook"
		return []githubRepoHook{hook}, nil
	}

	hint, err := buildExistingHookRemediation(
		context.Background(),
		[]string{"holon-run/holon"},
		"http://127.0.0.1:8080/ingress/github/webhook",
	)
	if err != nil {
		t.Fatalf("buildExistingHookRemediation() error = %v", err)
	}
	if !strings.Contains(hint, "existing hook id 12345") {
		t.Fatalf("expected hint to include hook id, got: %s", hint)
	}
	if !strings.Contains(hint, "gh api -X DELETE repos/holon-run/holon/hooks/12345") {
		t.Fatalf("expected hint to include delete command, got: %s", hint)
	}
}

func TestForwarderBuildStartupFailureError_ConflictWithLookupError(t *testing.T) {
	orig := listGitHubRepoHooks
	t.Cleanup(func() { listGitHubRepoHooks = orig })

	listGitHubRepoHooks = func(_ context.Context, _ string) ([]githubRepoHook, error) {
		return nil, errors.New("boom")
	}

	f := &Forwarder{
		repos: []string{"holon-run/holon"},
		url:   "http://127.0.0.1:8080/ingress/github/webhook",
	}
	err := f.buildStartupFailureError(context.Background(), errors.New("exit status 1"), "Error: "+existingHookConflictMarker)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
	got := err.Error()
	if !strings.Contains(got, "detected existing webhook conflict but failed to inspect hooks") {
		t.Fatalf("expected lookup failure hint, got: %s", got)
	}
}
