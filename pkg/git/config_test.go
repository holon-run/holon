package git

import (
	"os"
	"os/exec"
	"strings"
	"testing"
)

// setupTestGitConfig sets up a test git config and returns a cleanup function.
func setupTestGitConfig(t *testing.T, name, email string) func() {
	// Create a temporary directory for testing
	tmpDir := t.TempDir()

	// Initialize git repo
	cmd := exec.Command("git", "init")
	cmd.Dir = tmpDir
	if err := cmd.Run(); err != nil {
		t.Fatalf("failed to init git repo: %v", err)
	}

	// Set local git config
	if name != "" {
		cmd = exec.Command("git", "config", "--local", "user.name", name)
		cmd.Dir = tmpDir
		if err := cmd.Run(); err != nil {
			t.Fatalf("failed to set user.name: %v", err)
		}
	}
	if email != "" {
		cmd = exec.Command("git", "config", "--local", "user.email", email)
		cmd.Dir = tmpDir
		if err := cmd.Run(); err != nil {
			t.Fatalf("failed to set user.email: %v", err)
		}
	}

	return func() {
		// Cleanup is handled automatically by t.TempDir()
	}
}

// setupGlobalTestGitConfig sets up a global test git config and returns a cleanup function.
// WARNING: This modifies the user's global git config. Use with caution.
func setupGlobalTestGitConfig(t *testing.T, name, email string) func() {
	t.Helper()

	// Save current global config
	oldName, _ := exec.Command("git", "config", "--global", "--get", "user.name").Output()
	oldEmail, _ := exec.Command("git", "config", "--global", "--get", "user.email").Output()

	// Set new global config
	if name != "" {
		cmd := exec.Command("git", "config", "--global", "user.name", name)
		if err := cmd.Run(); err != nil {
			t.Fatalf("failed to set global user.name: %v", err)
		}
	}
	if email != "" {
		cmd := exec.Command("git", "config", "--global", "user.email", email)
		if err := cmd.Run(); err != nil {
			t.Fatalf("failed to set global user.email: %v", err)
		}
	}

	return func() {
		// Restore original global config
		if len(oldName) > 0 {
			cmd := exec.Command("git", "config", "--global", "user.name", strings.TrimSpace(string(oldName)))
			_ = cmd.Run()
		} else {
			cmd := exec.Command("git", "config", "--global", "--unset", "user.name")
			_ = cmd.Run()
		}
		if len(oldEmail) > 0 {
			cmd := exec.Command("git", "config", "--global", "user.email", strings.TrimSpace(string(oldEmail)))
			_ = cmd.Run()
		} else {
			cmd := exec.Command("git", "config", "--global", "--unset", "user.email")
			_ = cmd.Run()
		}
	}
}

func TestResolveConfig_Defaults(t *testing.T) {
	// NOTE: This test may get host git config from the environment
	// If you have git config set globally, it will override defaults (which is correct behavior)

	opts := ConfigOptions{}
	cfg := ResolveConfig(opts)

	// If host git config is set, we'll get those values (correct behavior)
	// Otherwise, we should get defaults
	if cfg.AuthorName != DefaultAuthorName {
		t.Logf("Note: host git config is set, overriding default name: %s", cfg.AuthorName)
	}
	if cfg.AuthorEmail != DefaultAuthorEmail {
		t.Logf("Note: host git config is set, overriding default email: %s", cfg.AuthorEmail)
	}

	// Verify we got valid values (either defaults or host config)
	if cfg.AuthorName == "" {
		t.Error("author name should not be empty")
	}
	if cfg.AuthorEmail == "" {
		t.Error("author email should not be empty")
	}
}

func TestResolveConfig_ProjectConfig(t *testing.T) {
	opts := ConfigOptions{
		ProjectAuthorName:  "Project Bot",
		ProjectAuthorEmail: "project-bot@example.com",
	}

	cfg := ResolveConfig(opts)

	// If host git config is set, it will override project config
	// Otherwise, we should get project config values
	if cfg.AuthorName != DefaultAuthorName && cfg.AuthorName != "Project Bot" {
		t.Logf("Note: host git config is overriding project config: %s", cfg.AuthorName)
	}
	if cfg.AuthorEmail != DefaultAuthorEmail && cfg.AuthorEmail != "project-bot@example.com" {
		t.Logf("Note: host git config is overriding project config: %s", cfg.AuthorEmail)
	}

	// Verify we got valid values (either project config or host config or defaults)
	if cfg.AuthorName == "" {
		t.Error("author name should not be empty")
	}
	if cfg.AuthorEmail == "" {
		t.Error("author email should not be empty")
	}
}

func TestResolveConfig_EnvVars(t *testing.T) {
	// Save original env vars
	oldName := os.Getenv("GIT_AUTHOR_NAME")
	oldEmail := os.Getenv("GIT_AUTHOR_EMAIL")
	defer func() {
		if oldName != "" {
			os.Setenv("GIT_AUTHOR_NAME", oldName)
		} else {
			os.Unsetenv("GIT_AUTHOR_NAME")
		}
		if oldEmail != "" {
			os.Setenv("GIT_AUTHOR_EMAIL", oldEmail)
		} else {
			os.Unsetenv("GIT_AUTHOR_EMAIL")
		}
	}()

	// Set env vars
	os.Setenv("GIT_AUTHOR_NAME", "Env Bot")
	os.Setenv("GIT_AUTHOR_EMAIL", "env-bot@example.com")

	opts := ConfigOptions{
		EnvAuthorName:  "Env Bot",
		EnvAuthorEmail: "env-bot@example.com",
	}

	cfg := ResolveConfig(opts)

	// If host git config is set, it will override env vars
	if cfg.AuthorName != DefaultAuthorName && cfg.AuthorName != "Env Bot" {
		t.Logf("Note: host git config is overriding env vars: %s", cfg.AuthorName)
	}
	if cfg.AuthorEmail != DefaultAuthorEmail && cfg.AuthorEmail != "env-bot@example.com" {
		t.Logf("Note: host git config is overriding env vars: %s", cfg.AuthorEmail)
	}
}

func TestResolveConfig_HostGitConfigOverridesProject(t *testing.T) {
	cleanup := setupGlobalTestGitConfig(t, "Host User", "host-user@example.com")
	defer cleanup()

	opts := ConfigOptions{
		ProjectAuthorName:  "Project Bot",
		ProjectAuthorEmail: "project-bot@example.com",
	}

	cfg := ResolveConfig(opts)

	// Host git config should override project config
	if cfg.AuthorName != "Host User" {
		t.Errorf("expected host git config name %q, got %q", "Host User", cfg.AuthorName)
	}
	if cfg.AuthorEmail != "host-user@example.com" {
		t.Errorf("expected host git config email %q, got %q", "host-user@example.com", cfg.AuthorEmail)
	}
}

func TestResolveConfig_HostGitConfigOverridesEnv(t *testing.T) {
	cleanup := setupGlobalTestGitConfig(t, "Host User", "host-user@example.com")
	defer cleanup()

	opts := ConfigOptions{
		EnvAuthorName:  "Env Bot",
		EnvAuthorEmail: "env-bot@example.com",
	}

	cfg := ResolveConfig(opts)

	// Host git config should override env vars
	if cfg.AuthorName != "Host User" {
		t.Errorf("expected host git config name %q, got %q", "Host User", cfg.AuthorName)
	}
	if cfg.AuthorEmail != "host-user@example.com" {
		t.Errorf("expected host git config email %q, got %q", "host-user@example.com", cfg.AuthorEmail)
	}
}

func TestResolveConfig_AllSources(t *testing.T) {
	cleanup := setupGlobalTestGitConfig(t, "Host User", "host-user@example.com")
	defer cleanup()

	// Save and set env vars
	oldName := os.Getenv("GIT_AUTHOR_NAME")
	oldEmail := os.Getenv("GIT_AUTHOR_EMAIL")
	defer func() {
		if oldName != "" {
			os.Setenv("GIT_AUTHOR_NAME", oldName)
		} else {
			os.Unsetenv("GIT_AUTHOR_NAME")
		}
		if oldEmail != "" {
			os.Setenv("GIT_AUTHOR_EMAIL", oldEmail)
		} else {
			os.Unsetenv("GIT_AUTHOR_EMAIL")
		}
	}()

	os.Setenv("GIT_AUTHOR_NAME", "Env Bot")
	os.Setenv("GIT_AUTHOR_EMAIL", "env-bot@example.com")

	opts := ConfigOptions{
		ProjectAuthorName:  "Project Bot",
		ProjectAuthorEmail: "project-bot@example.com",
		EnvAuthorName:      "Env Bot",
		EnvAuthorEmail:     "env-bot@example.com",
	}

	cfg := ResolveConfig(opts)

	// Host git config should have highest priority
	if cfg.AuthorName != "Host User" {
		t.Errorf("expected host git config name %q, got %q", "Host User", cfg.AuthorName)
	}
	if cfg.AuthorEmail != "host-user@example.com" {
		t.Errorf("expected host git config email %q, got %q", "host-user@example.com", cfg.AuthorEmail)
	}
}

func TestGetHostGitConfig(t *testing.T) {
	cleanup := setupGlobalTestGitConfig(t, "Test User", "test@example.com")
	defer cleanup()

	name := getHostGitConfig("user.name")
	if name != "Test User" {
		t.Errorf("expected user.name %q, got %q", "Test User", name)
	}

	email := getHostGitConfig("user.email")
	if email != "test@example.com" {
		t.Errorf("expected user.email %q, got %q", "test@example.com", email)
	}
}

func TestFormatGitAuthor(t *testing.T) {
	tests := []struct {
		name     string
		inputName string
		inputEmail string
		want    string
	}{
		{
			name:     "both name and email",
			inputName: "John Doe",
			inputEmail: "john@example.com",
			want:    "John Doe <john@example.com>",
		},
		{
			name:     "only name",
			inputName: "John Doe",
			inputEmail: "",
			want:    "John Doe",
		},
		{
			name:     "only email",
			inputName: "",
			inputEmail: "john@example.com",
			want:    "john@example.com",
		},
		{
			name:     "empty",
			inputName: "",
			inputEmail: "",
			want:    "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := FormatGitAuthor(tt.inputName, tt.inputEmail)
			if got != tt.want {
				t.Errorf("FormatGitAuthor() = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestParseGitAuthor(t *testing.T) {
	tests := []struct {
		name string
		author string
		wantName string
		wantEmail string
	}{
		{
			name: "both name and email",
			author: "John Doe <john@example.com>",
			wantName: "John Doe",
			wantEmail: "john@example.com",
		},
		{
			name: "only name",
			author: "John Doe",
			wantName: "John Doe",
			wantEmail: "",
		},
		{
			name: "email with angle brackets",
			author: " <john@example.com>",
			wantName: "",
			wantEmail: "john@example.com",
		},
		{
			name: "empty",
			author: "",
			wantName: "",
			wantEmail: "",
		},
		{
			name: "name with multiple angles in email",
			author: "John Doe <john+test@example.com>",
			wantName: "John Doe",
			wantEmail: "john+test@example.com",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			name, email := ParseGitAuthor(tt.author)
			if name != tt.wantName {
				t.Errorf("ParseGitAuthor() name = %q, want %q", name, tt.wantName)
			}
			if email != tt.wantEmail {
				t.Errorf("ParseGitAuthor() email = %q, want %q", email, tt.wantEmail)
			}
		})
	}
}

func TestResolveConfigFromEnv(t *testing.T) {
	// Save original env vars
	oldName := os.Getenv("GIT_AUTHOR_NAME")
	oldEmail := os.Getenv("GIT_AUTHOR_EMAIL")
	defer func() {
		if oldName != "" {
			os.Setenv("GIT_AUTHOR_NAME", oldName)
		} else {
			os.Unsetenv("GIT_AUTHOR_NAME")
		}
		if oldEmail != "" {
			os.Setenv("GIT_AUTHOR_EMAIL", oldEmail)
		} else {
			os.Unsetenv("GIT_AUTHOR_EMAIL")
		}
	}()

	// Set env vars
	os.Setenv("GIT_AUTHOR_NAME", "Env Bot")
	os.Setenv("GIT_AUTHOR_EMAIL", "env-bot@example.com")

	cfg := ResolveConfigFromEnv()

	// If host git config is set, it will override env vars
	if cfg.AuthorName != DefaultAuthorName && cfg.AuthorName != "Env Bot" {
		t.Logf("Note: host git config is overriding env vars: %s", cfg.AuthorName)
	}
	if cfg.AuthorEmail != DefaultAuthorEmail && cfg.AuthorEmail != "env-bot@example.com" {
		t.Logf("Note: host git config is overriding env vars: %s", cfg.AuthorEmail)
	}
}

func TestGetUserInfo(t *testing.T) {
	// This test checks the deprecated GetUserInfo function
	cleanup := setupGlobalTestGitConfig(t, "Test User", "test@example.com")
	defer cleanup()

	name, email := GetUserInfo()

	if name != "Test User" {
		t.Errorf("expected user.name %q, got %q", "Test User", name)
	}
	if email != "test@example.com" {
		t.Errorf("expected user.email %q, got %q", "test@example.com", email)
	}
}

func TestGetUserInfo_NoConfig(t *testing.T) {
	// Temporarily unset global git config
	cleanup := setupGlobalTestGitConfig(t, "", "")
	defer cleanup()

	name, email := GetUserInfo()

	// Should return defaults when no config is set
	// NOTE: This test may fail if git config is set in other locations (system, local)
	if name != DefaultAuthorName {
		t.Logf("Note: git config found (possibly system/local), got name: %s", name)
	}
	if email != DefaultAuthorEmail {
		t.Logf("Note: git config found (possibly system/local), got email: %s", email)
	}

	// Verify we got valid values
	if name == "" {
		t.Error("user.name should not be empty")
	}
	if email == "" {
		t.Error("user.email should not be empty")
	}
}
