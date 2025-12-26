package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoad_NoConfigFile(t *testing.T) {
	// Create temp directory with no config file
	tmpDir := t.TempDir()

	cfg, err := Load(tmpDir)
	if err != nil {
		t.Fatalf("Load() returned error: %v", err)
	}

	// Should return zero config
	if cfg.BaseImage != "" {
		t.Errorf("BaseImage should be empty, got %q", cfg.BaseImage)
	}
	if cfg.Agent != "" {
		t.Errorf("Agent should be empty, got %q", cfg.Agent)
	}
	if cfg.LogLevel != "" {
		t.Errorf("LogLevel should be empty, got %q", cfg.LogLevel)
	}
}

func TestLoad_ValidConfigFile(t *testing.T) {
	// Create temp directory with config file
	tmpDir := t.TempDir()
	holonDir := filepath.Join(tmpDir, ".holon")
	if err := os.MkdirAll(holonDir, 0755); err != nil {
		t.Fatal(err)
	}

	configContent := `
base_image: "python:3.11"
agent: "default"
log_level: "debug"
git:
  author_name: "Test Bot"
  author_email: "test@example.com"
`
	configPath := filepath.Join(holonDir, "config.yaml")
	if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := Load(tmpDir)
	if err != nil {
		t.Fatalf("Load() returned error: %v", err)
	}

	// Check parsed values
	if cfg.BaseImage != "python:3.11" {
		t.Errorf("BaseImage = %q, want %q", cfg.BaseImage, "python:3.11")
	}
	if cfg.Agent != "default" {
		t.Errorf("Agent = %q, want %q", cfg.Agent, "default")
	}
	if cfg.LogLevel != "debug" {
		t.Errorf("LogLevel = %q, want %q", cfg.LogLevel, "debug")
	}
	if cfg.Git.AuthorName != "Test Bot" {
		t.Errorf("Git.AuthorName = %q, want %q", cfg.Git.AuthorName, "Test Bot")
	}
	if cfg.Git.AuthorEmail != "test@example.com" {
		t.Errorf("Git.AuthorEmail = %q, want %q", cfg.Git.AuthorEmail, "test@example.com")
	}
}

func TestLoad_SearchParentDirectories(t *testing.T) {
	// Create temp directory structure:
	// tmpDir/
	//   .holon/config.yaml
	//   subdir/
	//     nested/
	tmpDir := t.TempDir()
	holonDir := filepath.Join(tmpDir, ".holon")
	if err := os.MkdirAll(holonDir, 0755); err != nil {
		t.Fatal(err)
	}

	configContent := `base_image: "node:20"`
	configPath := filepath.Join(holonDir, "config.yaml")
	if err := os.WriteFile(configPath, []byte(configContent), 0644); err != nil {
		t.Fatal(err)
	}

	// Create nested subdirectories
	subdir := filepath.Join(tmpDir, "subdir", "nested")
	if err := os.MkdirAll(subdir, 0755); err != nil {
		t.Fatal(err)
	}

	// Load from nested directory - should find config in parent
	cfg, err := Load(subdir)
	if err != nil {
		t.Fatalf("Load() returned error: %v", err)
	}

	if cfg.BaseImage != "node:20" {
		t.Errorf("BaseImage = %q, want %q", cfg.BaseImage, "node:20")
	}
}

func TestLoad_InvalidYAML(t *testing.T) {
	tmpDir := t.TempDir()
	holonDir := filepath.Join(tmpDir, ".holon")
	if err := os.MkdirAll(holonDir, 0755); err != nil {
		t.Fatal(err)
	}

	// Write invalid YAML
	configPath := filepath.Join(holonDir, "config.yaml")
	if err := os.WriteFile(configPath, []byte("invalid: yaml: content:["), 0644); err != nil {
		t.Fatal(err)
	}

	_, err := Load(tmpDir)
	if err == nil {
		t.Error("Load() should return error for invalid YAML")
	}
}

func TestResolveString(t *testing.T) {
	cfg := &ProjectConfig{
		BaseImage: "python:3.11",
		Agent:     "default",
		LogLevel:  "debug",
	}

	tests := []struct {
		name         string
		cliValue     string
		configValue  string
		defaultValue string
		wantValue    string
		wantSource   string
	}{
		{
			name:         "CLI takes precedence",
			cliValue:     "cli-value",
			configValue:  "config-value",
			defaultValue: "default-value",
			wantValue:    "cli-value",
			wantSource:   "cli",
		},
		{
			name:         "Config takes precedence over default",
			cliValue:     "",
			configValue:  "config-value",
			defaultValue: "default-value",
			wantValue:    "config-value",
			wantSource:   "config",
		},
		{
			name:         "Default when no CLI or config",
			cliValue:     "",
			configValue:  "",
			defaultValue: "default-value",
			wantValue:    "default-value",
			wantSource:   "default",
		},
		{
			name:         "Empty default when CLI and config empty",
			cliValue:     "",
			configValue:  "",
			defaultValue: "",
			wantValue:    "",
			wantSource:   "default",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gotValue, gotSource := cfg.ResolveString(tt.cliValue, tt.configValue, tt.defaultValue)
			if gotValue != tt.wantValue {
				t.Errorf("value = %q, want %q", gotValue, tt.wantValue)
			}
			if gotSource != tt.wantSource {
				t.Errorf("source = %q, want %q", gotSource, tt.wantSource)
			}
		})
	}
}

func TestResolveBaseImage(t *testing.T) {
	tests := []struct {
		name         string
		baseImage    string
		cliValue     string
		defaultValue string
		wantValue    string
		wantSource   string
	}{
		{
			name:         "CLI overrides config",
			baseImage:    "python:3.11",
			cliValue:     "node:20",
			defaultValue: "golang:1.22",
			wantValue:    "node:20",
			wantSource:   "cli",
		},
		{
			name:         "Config overrides default",
			baseImage:    "python:3.11",
			cliValue:     "",
			defaultValue: "golang:1.22",
			wantValue:    "python:3.11",
			wantSource:   "config",
		},
		{
			name:         "Default when no CLI or config",
			baseImage:    "",
			cliValue:     "",
			defaultValue: "golang:1.22",
			wantValue:    "golang:1.22",
			wantSource:   "default",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := &ProjectConfig{BaseImage: tt.baseImage}
			gotValue, gotSource := cfg.ResolveBaseImage(tt.cliValue, tt.defaultValue)
			if gotValue != tt.wantValue {
				t.Errorf("value = %q, want %q", gotValue, tt.wantValue)
			}
			if gotSource != tt.wantSource {
				t.Errorf("source = %q, want %q", gotSource, tt.wantSource)
			}
		})
	}
}

func TestResolveAgent(t *testing.T) {
	tests := []struct {
		name       string
		agent      string
		cliValue   string
		wantValue  string
		wantSource string
	}{
		{
			name:       "CLI overrides config",
			agent:      "default",
			cliValue:   "custom-agent",
			wantValue:  "custom-agent",
			wantSource: "cli",
		},
		{
			name:       "Config used when no CLI",
			agent:      "default",
			cliValue:   "",
			wantValue:  "default",
			wantSource: "config",
		},
		{
			name:       "Empty when no CLI or config",
			agent:      "",
			cliValue:   "",
			wantValue:  "",
			wantSource: "default",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := &ProjectConfig{Agent: tt.agent}
			gotValue, gotSource := cfg.ResolveAgent(tt.cliValue)
			if gotValue != tt.wantValue {
				t.Errorf("value = %q, want %q", gotValue, tt.wantValue)
			}
			if gotSource != tt.wantSource {
				t.Errorf("source = %q, want %q", gotSource, tt.wantSource)
			}
		})
	}
}

func TestResolveLogLevel(t *testing.T) {
	tests := []struct {
		name         string
		logLevel     string
		cliValue     string
		defaultValue string
		wantValue    string
		wantSource   string
	}{
		{
			name:         "CLI overrides config",
			logLevel:     "debug",
			cliValue:     "info",
			defaultValue: "progress",
			wantValue:    "info",
			wantSource:   "cli",
		},
		{
			name:         "Config overrides default",
			logLevel:     "debug",
			cliValue:     "",
			defaultValue: "progress",
			wantValue:    "debug",
			wantSource:   "config",
		},
		{
			name:         "Default when no CLI or config",
			logLevel:     "",
			cliValue:     "",
			defaultValue: "progress",
			wantValue:    "progress",
			wantSource:   "default",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := &ProjectConfig{LogLevel: tt.logLevel}
			gotValue, gotSource := cfg.ResolveLogLevel(tt.cliValue, tt.defaultValue)
			if gotValue != tt.wantValue {
				t.Errorf("value = %q, want %q", gotValue, tt.wantValue)
			}
			if gotSource != tt.wantSource {
				t.Errorf("source = %q, want %q", gotSource, tt.wantSource)
			}
		})
	}
}

func TestGetGitAuthorName(t *testing.T) {
	cfg := &ProjectConfig{
		Git: GitConfig{AuthorName: "Test Bot"},
	}
	if got := cfg.GetGitAuthorName(); got != "Test Bot" {
		t.Errorf("GetGitAuthorName() = %q, want %q", got, "Test Bot")
	}
}

func TestGetGitAuthorEmail(t *testing.T) {
	cfg := &ProjectConfig{
		Git: GitConfig{AuthorEmail: "test@example.com"},
	}
	if got := cfg.GetGitAuthorEmail(); got != "test@example.com" {
		t.Errorf("GetGitAuthorEmail() = %q, want %q", got, "test@example.com")
	}
}

func TestHasGitConfig(t *testing.T) {
	tests := []struct {
		name     string
		cfg      ProjectConfig
		expected bool
	}{
		{
			name:     "No git config",
			cfg:      ProjectConfig{},
			expected: false,
		},
		{
			name: "Only author name",
			cfg: ProjectConfig{
				Git: GitConfig{AuthorName: "Bot"},
			},
			expected: true,
		},
		{
			name: "Only author email",
			cfg: ProjectConfig{
				Git: GitConfig{AuthorEmail: "bot@example.com"},
			},
			expected: true,
		},
		{
			name: "Both name and email",
			cfg: ProjectConfig{
				Git: GitConfig{
					AuthorName:  "Bot",
					AuthorEmail: "bot@example.com",
				},
			},
			expected: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.cfg.HasGitConfig(); got != tt.expected {
				t.Errorf("HasGitConfig() = %v, want %v", got, tt.expected)
			}
		})
	}
}
