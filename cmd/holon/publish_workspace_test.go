package main

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"

	"github.com/holon-run/holon/pkg/git"
	"github.com/holon-run/holon/pkg/workspace"
)

func TestPreparePublishWorkspace_GitWorktree(t *testing.T) {
	tmpDir := t.TempDir()
	sourceDir := filepath.Join(tmpDir, "repo")
	if err := os.MkdirAll(sourceDir, 0o755); err != nil {
		t.Fatalf("mkdir source: %v", err)
	}

	// Initialize a git repository with one commit.
	if err := runGit(sourceDir, "init"); err != nil {
		t.Fatalf("git init: %v", err)
	}
	if err := runGit(sourceDir, "config", "user.name", "Test User"); err != nil {
		t.Fatalf("git config user.name: %v", err)
	}
	if err := runGit(sourceDir, "config", "user.email", "test@example.com"); err != nil {
		t.Fatalf("git config user.email: %v", err)
	}
	filePath := filepath.Join(sourceDir, "hello.txt")
	if err := os.WriteFile(filePath, []byte("hello\n"), 0o644); err != nil {
		t.Fatalf("write file: %v", err)
	}
	if err := runGit(sourceDir, "add", "."); err != nil {
		t.Fatalf("git add: %v", err)
	}
	if err := runGit(sourceDir, "commit", "-m", "init"); err != nil {
		t.Fatalf("git commit: %v", err)
	}

	client := git.NewClient(sourceDir)
	head, err := client.GetHeadSHA(context.Background())
	if err != nil {
		t.Fatalf("get head: %v", err)
	}

	// Write manifest to output dir.
	outDir := filepath.Join(tmpDir, "out")
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		t.Fatalf("mkdir out: %v", err)
	}
	result := workspace.PrepareResult{
		Strategy:   "git-clone",
		Source:     sourceDir,
		Ref:        "HEAD",
		HeadSHA:    head,
		CreatedAt:  time.Now(),
		HasHistory: true,
	}
	if err := workspace.WriteManifest(outDir, result); err != nil {
		t.Fatalf("write manifest: %v", err)
	}

	pubWS, err := preparePublishWorkspace(context.Background(), outDir)
	if err != nil {
		t.Fatalf("preparePublishWorkspace returned error: %v", err)
	}
	defer pubWS.cleanup()

	if pubWS.path == "" || pubWS.path == sourceDir {
		t.Fatalf("unexpected publish workspace path: %q", pubWS.path)
	}

	// Verify the worktree is a git repo and contains the committed file.
	worktreeClient := git.NewClient(pubWS.path)
	if !worktreeClient.IsRepo(context.Background()) {
		t.Fatalf("publish workspace is not a git repo: %s", pubWS.path)
	}
	content, err := os.ReadFile(filepath.Join(pubWS.path, "hello.txt"))
	if err != nil {
		t.Fatalf("read worktree file: %v", err)
	}
	if string(content) != "hello\n" {
		t.Fatalf("worktree file content = %q, want %q", string(content), "hello\n")
	}
}

func TestPreparePublishWorkspace_NonGit(t *testing.T) {
	tmpDir := t.TempDir()
	sourceDir := filepath.Join(tmpDir, "notgit")
	if err := os.MkdirAll(sourceDir, 0o755); err != nil {
		t.Fatalf("mkdir source: %v", err)
	}

	outDir := filepath.Join(tmpDir, "out")
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		t.Fatalf("mkdir out: %v", err)
	}

	result := workspace.PrepareResult{
		Strategy:  "snapshot",
		Source:    sourceDir,
		CreatedAt: time.Now(),
	}
	if err := workspace.WriteManifest(outDir, result); err != nil {
		t.Fatalf("write manifest: %v", err)
	}

	if _, err := preparePublishWorkspace(context.Background(), outDir); err == nil {
		t.Fatalf("expected error for non-git source, got nil")
	}
}

func TestPreparePublishWorkspace_GitURLClone(t *testing.T) {
	tmpDir := t.TempDir()
	sourceDir := filepath.Join(tmpDir, "repo")
	if err := os.MkdirAll(sourceDir, 0o755); err != nil {
		t.Fatalf("mkdir source: %v", err)
	}

	// Init git repo with one commit
	if err := runGit(sourceDir, "init"); err != nil {
		t.Fatalf("git init: %v", err)
	}
	if err := runGit(sourceDir, "config", "user.name", "Test User"); err != nil {
		t.Fatalf("git config user.name: %v", err)
	}
	if err := runGit(sourceDir, "config", "user.email", "test@example.com"); err != nil {
		t.Fatalf("git config user.email: %v", err)
	}
	filePath := filepath.Join(sourceDir, "a.txt")
	if err := os.WriteFile(filePath, []byte("data\n"), 0o644); err != nil {
		t.Fatalf("write file: %v", err)
	}
	if err := runGit(sourceDir, "add", "."); err != nil {
		t.Fatalf("git add: %v", err)
	}
	if err := runGit(sourceDir, "commit", "-m", "init"); err != nil {
		t.Fatalf("git commit: %v", err)
	}

	client := git.NewClient(sourceDir)
	head, err := client.GetHeadSHA(context.Background())
	if err != nil {
		t.Fatalf("get head: %v", err)
	}

	outDir := filepath.Join(tmpDir, "out")
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		t.Fatalf("mkdir out: %v", err)
	}
	result := workspace.PrepareResult{
		Strategy:   "git-clone",
		Source:     "file://" + sourceDir,
		Ref:        "HEAD",
		HeadSHA:    head,
		CreatedAt:  time.Now(),
		HasHistory: true,
		IsShallow:  true,
	}
	if err := workspace.WriteManifest(outDir, result); err != nil {
		t.Fatalf("write manifest: %v", err)
	}

	pubWS, err := preparePublishWorkspace(context.Background(), outDir)
	if err != nil {
		t.Fatalf("preparePublishWorkspace returned error: %v", err)
	}
	defer pubWS.cleanup()

	if pubWS.path == "" || pubWS.path == sourceDir {
		t.Fatalf("unexpected publish workspace path: %q", pubWS.path)
	}

	content, err := os.ReadFile(filepath.Join(pubWS.path, "a.txt"))
	if err != nil {
		t.Fatalf("read cloned file: %v", err)
	}
	if string(content) != "data\n" {
		t.Fatalf("cloned file content = %q, want %q", string(content), "data\n")
	}
}

func runGit(dir string, args ...string) error {
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("git %v failed: %w: %s", args, err, string(out))
	}
	return nil
}
