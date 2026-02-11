package ghpath

import (
	"archive/zip"
	"bytes"
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
)

type failingGitRunner struct{}

func (r *failingGitRunner) Run(_ context.Context, _ ...string) error {
	return fmt.Errorf("git unavailable")
}

func TestParseRef(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		want    *Ref
		wantErr bool
	}{
		{
			name:  "ghpath format",
			input: "ghpath:holon-run/holon/skills/github-review@main",
			want: &Ref{
				Owner: "holon-run",
				Repo:  "holon",
				Ref:   "main",
				Path:  "skills/github-review",
			},
		},
		{
			name:  "github scheme",
			input: "github://holon-run/holon/main/skills/github-review",
			want: &Ref{
				Owner: "holon-run",
				Repo:  "holon",
				Ref:   "main",
				Path:  "skills/github-review",
			},
		},
		{
			name:  "tree url",
			input: "https://github.com/holon-run/holon/tree/main/skills/github-review",
			want: &Ref{
				Owner: "holon-run",
				Repo:  "holon",
				Ref:   "main",
				Path:  "skills/github-review",
			},
		},
		{
			name:    "invalid path traversal",
			input:   "ghpath:holon-run/holon/../skills/github-review@main",
			wantErr: true,
		},
		{
			name:    "not github path ref",
			input:   "https://example.com/skill.zip",
			wantErr: true,
		},
		{
			name:    "github archive url is not ghpath ref",
			input:   "https://github.com/holon-run/holon/archive/refs/heads/main.zip",
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ref, err := ParseRef(tt.input)
			if tt.wantErr {
				if err == nil {
					t.Fatalf("expected error, got nil")
				}
				if (tt.name == "not github path ref" || tt.name == "github archive url is not ghpath ref") && err != ErrNotGitHubPathRef {
					t.Fatalf("expected ErrNotGitHubPathRef, got: %v", err)
				}
				return
			}
			if err != nil {
				t.Fatalf("ParseRef failed: %v", err)
			}

			if *ref != *tt.want {
				t.Fatalf("ParseRef() = %#v, want %#v", ref, tt.want)
			}
		})
	}
}

func TestResolver_Resolve_UsesZipFallbackAndCache(t *testing.T) {
	cacheDir, err := os.MkdirTemp("", "holon-ghpath-cache-*")
	if err != nil {
		t.Fatalf("failed to create cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	zipData := buildZip(t, map[string]string{
		"holon-main/skills/github-review/SKILL.md":  "# Review Skill\n",
		"holon-main/skills/github-review/README.md": "docs\n",
	})

	var requestCount int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		atomic.AddInt32(&requestCount, 1)
		if r.URL.Path != "/holon-run/holon/zip/refs/heads/main" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "application/zip")
		w.WriteHeader(http.StatusOK)
		_, writeErr := w.Write(zipData)
		if writeErr != nil {
			t.Fatalf("failed to write zip response: %v", writeErr)
		}
	}))
	defer server.Close()

	resolver := NewResolver(
		cacheDir,
		WithGitRunner(&failingGitRunner{}),
		WithCodeloadBaseURL(server.URL),
		WithGitHubAPIBaseURL(server.URL),
	)

	ref, err := ParseRef("ghpath:holon-run/holon/skills/github-review@main")
	if err != nil {
		t.Fatalf("ParseRef failed: %v", err)
	}

	firstPath, err := resolver.Resolve(context.Background(), ref)
	if err != nil {
		t.Fatalf("Resolve failed: %v", err)
	}
	if _, err := os.Stat(filepath.Join(firstPath, "SKILL.md")); err != nil {
		t.Fatalf("resolved skill missing SKILL.md: %v", err)
	}

	secondPath, err := resolver.Resolve(context.Background(), ref)
	if err != nil {
		t.Fatalf("second Resolve failed: %v", err)
	}
	if secondPath != firstPath {
		t.Fatalf("expected cached path %q, got %q", firstPath, secondPath)
	}

	if got := atomic.LoadInt32(&requestCount); got != 1 {
		t.Fatalf("expected 1 download request due to cache hit, got %d", got)
	}
}

func TestResolver_Resolve_PrivateRepoFallbackUsesGitHubAPIToken(t *testing.T) {
	cacheDir, err := os.MkdirTemp("", "holon-ghpath-cache-*")
	if err != nil {
		t.Fatalf("failed to create cache dir: %v", err)
	}
	defer os.RemoveAll(cacheDir)

	zipData := buildZip(t, map[string]string{
		"holon-main/skills/github-review/SKILL.md": "# Review Skill\n",
	})

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case strings.HasPrefix(r.URL.Path, "/holon-run/holon/zip/"):
			http.NotFound(w, r)
			return
		case r.URL.Path == "/repos/holon-run/holon/zipball/main":
			if got := r.Header.Get("Authorization"); got != "Bearer test-token" {
				w.WriteHeader(http.StatusUnauthorized)
				_, _ = w.Write([]byte("missing token"))
				return
			}
			w.Header().Set("Content-Type", "application/zip")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write(zipData)
			return
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()

	resolver := NewResolver(
		cacheDir,
		WithGitRunner(&failingGitRunner{}),
		WithCodeloadBaseURL(server.URL),
		WithGitHubAPIBaseURL(server.URL),
		WithToken("test-token"),
	)

	ref, err := ParseRef("ghpath:holon-run/holon/skills/github-review@main")
	if err != nil {
		t.Fatalf("ParseRef failed: %v", err)
	}

	resolvedPath, err := resolver.Resolve(context.Background(), ref)
	if err != nil {
		t.Fatalf("Resolve failed: %v", err)
	}
	if _, err := os.Stat(filepath.Join(resolvedPath, "SKILL.md")); err != nil {
		t.Fatalf("resolved skill missing SKILL.md: %v", err)
	}
}

func buildZip(t *testing.T, files map[string]string) []byte {
	t.Helper()

	buf := new(bytes.Buffer)
	zw := zip.NewWriter(buf)

	for name, content := range files {
		w, err := zw.Create(name)
		if err != nil {
			t.Fatalf("failed to create zip file %s: %v", name, err)
		}
		_, err = w.Write([]byte(content))
		if err != nil {
			t.Fatalf("failed to write zip file %s: %v", name, err)
		}
	}

	if err := zw.Close(); err != nil {
		t.Fatalf("failed to close zip writer: %v", err)
	}

	return buf.Bytes()
}
