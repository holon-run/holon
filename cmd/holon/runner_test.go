package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/holon-run/holon/pkg/runtime/docker"
)

// MockRuntime is a mock implementation of the Runtime interface for testing
type MockRuntime struct {
	RunHolonFunc func(ctx context.Context, cfg *docker.ContainerConfig) error
	calls        []struct {
		ctx context.Context
		cfg *docker.ContainerConfig
	}
}

func (m *MockRuntime) RunHolon(ctx context.Context, cfg *docker.ContainerConfig) error {
	// Always record the call
	m.calls = append(m.calls, struct {
		ctx context.Context
		cfg *docker.ContainerConfig
	}{ctx: ctx, cfg: cfg})

	if m.RunHolonFunc != nil {
		return m.RunHolonFunc(ctx, cfg)
	}
	return nil
}

func (m *MockRuntime) GetCalls() []struct {
	ctx context.Context
	cfg *docker.ContainerConfig
} {
	return m.calls
}

func (m *MockRuntime) Reset() {
	m.calls = nil
}

// setupTestEnv creates a temporary directory structure for testing
func setupTestEnv(t *testing.T) (tempDir, workspaceDir, outDir string) {
	t.Helper()

	tempDir = t.TempDir()
	workspaceDir = filepath.Join(tempDir, "workspace")
	outDir = filepath.Join(tempDir, "output")

	if err := os.MkdirAll(workspaceDir, 0755); err != nil {
		t.Fatalf("Failed to create workspace dir: %v", err)
	}
	if err := os.MkdirAll(outDir, 0755); err != nil {
		t.Fatalf("Failed to create output dir: %v", err)
	}

	return tempDir, workspaceDir, outDir
}

// createTestSpec creates a test spec file with the given parameters
func createTestSpec(t *testing.T, path, name, goal string, env map[string]string) {
	t.Helper()

	specContent := fmt.Sprintf(`version: "v1"
kind: Holon
metadata:
  name: %q
goal:
  description: %q
context:
  env:`, name, goal)

	for k, v := range env {
		specContent += fmt.Sprintf("\n    %q: %q", k, v)
	}

	if err := os.WriteFile(path, []byte(specContent), 0644); err != nil {
		t.Fatalf("Failed to write test spec: %v", err)
	}
}

func createDummyBundle(t *testing.T, dir string) string {
	t.Helper()

	bundlePath := filepath.Join(dir, "agent-bundle.tar.gz")
	if err := os.WriteFile(bundlePath, []byte("bundle"), 0644); err != nil {
		t.Fatalf("Failed to create bundle file: %v", err)
	}
	return bundlePath
}

func TestRunner_Run_RequiresSpecOrGoal(t *testing.T) {
	mockRuntime := &MockRuntime{}
	runner := NewRunner(mockRuntime)

	_, workspaceDir, outDir := setupTestEnv(t)

	cfg := RunnerConfig{
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
	}

	err := runner.Run(context.Background(), cfg)

	if err == nil {
		t.Error("Expected error when neither spec nor goal is provided")
	}

	expectedError := "either --spec or --goal is required"
	if !strings.Contains(err.Error(), expectedError) {
		t.Errorf("Expected error containing %q, got %q", expectedError, err.Error())
	}
}

func TestRunner_Run_WithGoalOnly(t *testing.T) {
	mockRuntime := &MockRuntime{
		RunHolonFunc: func(ctx context.Context, cfg *docker.ContainerConfig) error {
			// Verify that the spec file exists in input directory
			specPath := filepath.Join(cfg.InputPath, "spec.yaml")
			if _, err := os.Stat(specPath); os.IsNotExist(err) {
				t.Errorf("Expected spec file to exist at %s", specPath)
			}
			return nil
		},
	}
	runner := NewRunner(mockRuntime)

	tempDir, workspaceDir, outDir := setupTestEnv(t)
	bundlePath := createDummyBundle(t, tempDir)

	cfg := RunnerConfig{
		GoalStr:       "Test goal",
		TaskName:      "test-task",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
	}

	err := runner.Run(context.Background(), cfg)

	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	// Verify that RunHolon was called
	calls := mockRuntime.GetCalls()
	if len(calls) != 1 {
		t.Errorf("Expected 1 call to RunHolon, got %d", len(calls))
	}
}

func TestRunner_Run_WithSpecOnly(t *testing.T) {
	mockRuntime := &MockRuntime{}
	runner := NewRunner(mockRuntime)

	tempDir, workspaceDir, outDir := setupTestEnv(t)
	specPath := filepath.Join(tempDir, "spec.yaml")
	createTestSpec(t, specPath, "test-spec", "Test goal from spec", map[string]string{
		"SPEC_ENV": "spec-value",
	})
	bundlePath := createDummyBundle(t, tempDir)

	cfg := RunnerConfig{
		SpecPath:      specPath,
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
	}

	err := runner.Run(context.Background(), cfg)

	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	calls := mockRuntime.GetCalls()
	if len(calls) != 1 {
		t.Errorf("Expected 1 call to RunHolon, got %d", len(calls))
	}

	// Verify that the goal was extracted from the spec
	// This is verified implicitly by the successful execution
}

func TestRunner_Run_EnvVariablePrecedence(t *testing.T) {
	// Test that spec context.env < auto injection < --env
	mockRuntime := &MockRuntime{}
	runner := NewRunner(mockRuntime)

	tempDir, workspaceDir, outDir := setupTestEnv(t)
	specPath := filepath.Join(tempDir, "spec.yaml")
	createTestSpec(t, specPath, "test-spec", "Test goal", map[string]string{
		"SPEC_ENV": "spec-value",
	})
	bundlePath := createDummyBundle(t, tempDir)

	// Set environment variables for auto-injection
	t.Setenv("ANTHROPIC_API_KEY", "test-key")
	t.Setenv("GITHUB_TOKEN", "test-token")

	cfg := RunnerConfig{
		SpecPath:      specPath,
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
		EnvVarsList:   []string{"TEST_VAR=from-cli", "CLI_VAR=cli-value"},
	}

	err := runner.Run(context.Background(), cfg)

	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	calls := mockRuntime.GetCalls()
	if len(calls) != 1 {
		t.Fatalf("Expected 1 call to RunHolon, got %d", len(calls))
	}

	env := calls[0].cfg.Env

	// Verify precedence
	if env["TEST_VAR"] != "from-cli" {
		t.Errorf("Expected TEST_VAR to be 'from-cli' (highest priority), got %q", env["TEST_VAR"])
	}

	if env["SPEC_ENV"] != "spec-value" {
		t.Errorf("Expected SPEC_ENV to be 'spec-value', got %q", env["SPEC_ENV"])
	}

	if env["ANTHROPIC_API_KEY"] != "test-key" {
		t.Errorf("Expected ANTHROPIC_API_KEY to be 'test-key', got %q", env["ANTHROPIC_API_KEY"])
	}

	if env["GITHUB_TOKEN"] != "test-token" {
		t.Errorf("Expected GITHUB_TOKEN to be 'test-token', got %q", env["GITHUB_TOKEN"])
	}

	if env["CLI_VAR"] != "cli-value" {
		t.Errorf("Expected CLI_VAR to be 'cli-value', got %q", env["CLI_VAR"])
	}
}

func TestRunner_Run_GoalExtractionFromSpec(t *testing.T) {
	mockRuntime := &MockRuntime{}
	runner := NewRunner(mockRuntime)

	tempDir, workspaceDir, outDir := setupTestEnv(t)
	specPath := filepath.Join(tempDir, "spec.yaml")
	createTestSpec(t, specPath, "test-spec", "Goal from spec file", nil)
	bundlePath := createDummyBundle(t, tempDir)

	cfg := RunnerConfig{
		SpecPath:      specPath, // No goal string provided, should extract from spec
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
	}

	err := runner.Run(context.Background(), cfg)

	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	calls := mockRuntime.GetCalls()
	if len(calls) != 1 {
		t.Errorf("Expected 1 call to RunHolon, got %d", len(calls))
	}

	// The goal extraction is verified by the successful execution
	// If goal extraction failed, validation would catch it
}

func TestRunner_Run_DebugPromptOutputs(t *testing.T) {
	mockRuntime := &MockRuntime{}
	runner := NewRunner(mockRuntime)

	tempDir, workspaceDir, outDir := setupTestEnv(t)
	bundlePath := createDummyBundle(t, tempDir)

	cfg := RunnerConfig{
		GoalStr:       "Test goal for debug prompts",
		TaskName:      "test-debug",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
		RoleName:      "coder",
	}

	err := runner.Run(context.Background(), cfg)

	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	// Check that debug prompt files were created
	sysPromptPath := filepath.Join(outDir, "prompt.compiled.system.md")
	userPromptPath := filepath.Join(outDir, "prompt.compiled.user.md")

	if _, err := os.Stat(sysPromptPath); os.IsNotExist(err) {
		t.Error("Expected system prompt debug file to be created")
	}

	if _, err := os.Stat(userPromptPath); os.IsNotExist(err) {
		t.Error("Expected user prompt debug file to be created")
	}

	// Verify content is not empty
	sysContent, err := os.ReadFile(sysPromptPath)
	if err != nil {
		t.Errorf("Failed to read system prompt file: %v", err)
	}
	if len(sysContent) == 0 {
		t.Error("System prompt file is empty")
	}

	userContent, err := os.ReadFile(userPromptPath)
	if err != nil {
		t.Errorf("Failed to read user prompt file: %v", err)
	}
	if len(userContent) == 0 {
		t.Error("User prompt file is empty")
	}

	// Verify user prompt contains the goal
	if !strings.Contains(string(userContent), "Test goal for debug prompts") {
		t.Error("User prompt doesn't contain the expected goal")
	}
}

func TestRunner_Run_LogLevelDefaults(t *testing.T) {
	mockRuntime := &MockRuntime{}
	runner := NewRunner(mockRuntime)

	tempDir, workspaceDir, outDir := setupTestEnv(t)
	bundlePath := createDummyBundle(t, tempDir)

	// Test without explicit log level
	cfg1 := RunnerConfig{
		GoalStr:       "Test goal",
		TaskName:      "test-log-1",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
	}

	err := runner.Run(context.Background(), cfg1)
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	calls1 := mockRuntime.GetCalls()
	if len(calls1) == 0 {
		t.Error("Expected at least 1 call to RunHolon")
	} else if calls1[0].cfg.Env["LOG_LEVEL"] != "progress" {
		t.Errorf("Expected default LOG_LEVEL to be 'progress', got %q", calls1[0].cfg.Env["LOG_LEVEL"])
	}

	mockRuntime.Reset()

	// Test with explicit log level
	cfg2 := RunnerConfig{
		GoalStr:       "Test goal",
		TaskName:      "test-log-2",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
		LogLevel:      "debug",
	}

	err = runner.Run(context.Background(), cfg2)
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	calls2 := mockRuntime.GetCalls()
	if len(calls2) == 0 {
		t.Error("Expected at least 1 call to RunHolon")
	} else if calls2[0].cfg.Env["LOG_LEVEL"] != "debug" {
		t.Errorf("Expected LOG_LEVEL to be 'debug', got %q", calls2[0].cfg.Env["LOG_LEVEL"])
	}
}

func TestRunner_Run_ModeDefaults(t *testing.T) {
	mockRuntime := &MockRuntime{}
	runner := NewRunner(mockRuntime)

	_, workspaceDir, outDir := setupTestEnv(t)
	bundlePath := createDummyBundle(t, t.TempDir())

	// Test without explicit mode (should default to "solve")
	cfg1 := RunnerConfig{
		GoalStr:       "Test goal",
		TaskName:      "test-mode-1",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
	}

	err := runner.Run(context.Background(), cfg1)
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	calls1 := mockRuntime.GetCalls()
	if len(calls1) == 0 {
		t.Error("Expected at least 1 call to RunHolon")
	} else if calls1[0].cfg.Env["HOLON_MODE"] != "solve" {
		t.Errorf("Expected default HOLON_MODE to be 'solve', got %q", calls1[0].cfg.Env["HOLON_MODE"])
	}

	mockRuntime.Reset()

	// Test with explicit mode
	cfg2 := RunnerConfig{
		GoalStr:       "Test goal",
		TaskName:      "test-mode-2",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
		AgentBundle:   bundlePath,
		Mode:          "plan",
	}

	err = runner.Run(context.Background(), cfg2)
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	calls2 := mockRuntime.GetCalls()
	if len(calls2) == 0 {
		t.Error("Expected at least 1 call to RunHolon")
	} else if calls2[0].cfg.Env["HOLON_MODE"] != "plan" {
		t.Errorf("Expected HOLON_MODE to be 'plan', got %q", calls2[0].cfg.Env["HOLON_MODE"])
	}
}

func TestRunner_collectEnvVars_SpecEnvParsing(t *testing.T) {
	runner := NewRunner(&MockRuntime{})

	tempDir := t.TempDir()
	specPath := filepath.Join(tempDir, "spec.yaml")
	createTestSpec(t, specPath, "test-spec", "Test goal", map[string]string{
		"VAR1": "value1",
		"VAR2": "value2",
	})

	cfg := RunnerConfig{
		SpecPath: specPath,
	}

	envVars, err := runner.collectEnvVars(cfg, specPath)
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	if envVars["VAR1"] != "value1" {
		t.Errorf("Expected VAR1 to be 'value1', got %q", envVars["VAR1"])
	}

	if envVars["VAR2"] != "value2" {
		t.Errorf("Expected VAR2 to be 'value2', got %q", envVars["VAR2"])
	}
}

func TestRunner_extractGoalFromSpec(t *testing.T) {
	runner := NewRunner(&MockRuntime{})

	tempDir := t.TempDir()
	specPath := filepath.Join(tempDir, "spec.yaml")
	createTestSpec(t, specPath, "test-spec", "Extracted goal", nil)

	goal, err := runner.extractGoalFromSpec(specPath)
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	if goal != "Extracted goal" {
		t.Errorf("Expected goal to be 'Extracted goal', got %q", goal)
	}
}

func TestRunner_extractGoalFromSpec_InvalidFile(t *testing.T) {
	runner := NewRunner(&MockRuntime{})

	_, err := runner.extractGoalFromSpec("/nonexistent/file.yaml")
	if err == nil {
		t.Error("Expected error for nonexistent file")
	}
}

func TestRunner_compilePrompts(t *testing.T) {
	runner := NewRunner(&MockRuntime{})

	// Test without context directory
	cfg := RunnerConfig{
		GoalStr:  "Test goal",
		RoleName: "coder",
	}

	// This will use the real embedded assets, but still tests the logic
	envVars := make(map[string]string)
	sysPrompt, userPrompt, promptTempDir, err := runner.compilePrompts(cfg, "", envVars)
	if err != nil {
		t.Errorf("Unexpected error compiling prompts: %v", err)
	}

	defer os.RemoveAll(promptTempDir)

	if len(sysPrompt) == 0 {
		t.Error("System prompt is empty")
	}

	if len(userPrompt) == 0 {
		t.Error("User prompt is empty")
	}

	if !strings.Contains(userPrompt, "Test goal") {
		t.Error("User prompt doesn't contain the goal")
	}
}

func TestRunner_writeDebugPrompts(t *testing.T) {
	runner := NewRunner(&MockRuntime{})

	outDir := t.TempDir()
	sysPrompt := "# System Prompt\nThis is a system prompt."
	userPrompt := "# User Prompt\nThis is a user prompt."

	err := runner.writeDebugPrompts(outDir, sysPrompt, userPrompt)
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	// Verify files were created
	sysPath := filepath.Join(outDir, "prompt.compiled.system.md")
	userPath := filepath.Join(outDir, "prompt.compiled.user.md")

	sysContent, err := os.ReadFile(sysPath)
	if err != nil {
		t.Errorf("Failed to read system prompt file: %v", err)
	}

	userContent, err := os.ReadFile(userPath)
	if err != nil {
		t.Errorf("Failed to read user prompt file: %v", err)
	}

	if string(sysContent) != sysPrompt {
		t.Error("System prompt content doesn't match")
	}

	if string(userContent) != userPrompt {
		t.Error("User prompt content doesn't match")
	}
}

// Integration test to verify the complete flow
func TestRunner_Integration(t *testing.T) {
	mockRuntime := &MockRuntime{
		RunHolonFunc: func(ctx context.Context, cfg *docker.ContainerConfig) error {
			// Verify all expected values are in the config
			if cfg.BaseImage != "golang:1.22" {
				t.Errorf("Expected BaseImage to be 'golang:1.22', got %q", cfg.BaseImage)
			}
			if cfg.AgentBundle == "" {
				t.Errorf("Expected AgentBundle to be set")
			}
			// WorkingDir is hardcoded to "/holon/workspace" in the docker runtime
			return nil
		},
	}

	runner := NewRunner(mockRuntime)

	tempDir, workspaceDir, outDir := setupTestEnv(t)
	specPath := filepath.Join(tempDir, "spec.yaml")
	createTestSpec(t, specPath, "integration-test", "Integration test goal", map[string]string{
		"TEST_ENV": "test-value",
	})

	// Create a context directory with some files
	contextDir := filepath.Join(tempDir, "context")
	if err := os.MkdirAll(contextDir, 0755); err != nil {
		t.Fatalf("Failed to create context dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(contextDir, "test.txt"), []byte("test content"), 0644); err != nil {
		t.Fatalf("Failed to create test context file: %v", err)
	}

	bundlePath := filepath.Join(tempDir, "agent-bundle.tar.gz")
	if err := os.WriteFile(bundlePath, []byte("bundle"), 0644); err != nil {
		t.Fatalf("Failed to create bundle file: %v", err)
	}

	cfg := RunnerConfig{
		SpecPath:      specPath,
		WorkspacePath: workspaceDir,
		ContextPath:   contextDir,
		OutDir:        outDir,
		BaseImage:     "golang:1.22",
		AgentBundle:   bundlePath,
		RoleName:      "coder",
		EnvVarsList:   []string{"CLI_VAR=cli-value"},
		LogLevel:      "debug",
		Mode:          "solve",
	}

	err := runner.Run(context.Background(), cfg)
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	// Verify calls
	calls := mockRuntime.GetCalls()
	if len(calls) != 1 {
		t.Errorf("Expected 1 call to RunHolon, got %d", len(calls))
	}

	// Verify environment variables
	env := calls[0].cfg.Env
	if env["TEST_ENV"] != "test-value" {
		t.Errorf("Expected TEST_ENV to be 'test-value', got %q", env["TEST_ENV"])
	}
	if env["CLI_VAR"] != "cli-value" {
		t.Errorf("Expected CLI_VAR to be 'cli-value', got %q", env["CLI_VAR"])
	}
	if env["LOG_LEVEL"] != "debug" {
		t.Errorf("Expected LOG_LEVEL to be 'debug', got %q", env["LOG_LEVEL"])
	}
	if env["HOLON_MODE"] != "solve" {
		t.Errorf("Expected HOLON_MODE to be 'solve', got %q", env["HOLON_MODE"])
	}

	// Verify debug prompts were created
	if _, err := os.Stat(filepath.Join(outDir, "prompt.compiled.system.md")); os.IsNotExist(err) {
		t.Error("Expected system prompt debug file")
	}
	if _, err := os.Stat(filepath.Join(outDir, "prompt.compiled.user.md")); os.IsNotExist(err) {
		t.Error("Expected user prompt debug file")
	}
}

// Test for findLatestBundle function
func Test_findLatestBundle(t *testing.T) {
	t.Run("Directory with multiple bundles", func(t *testing.T) {
		tempDir := t.TempDir()

		// Create test bundles with different modification times
		bundle1 := filepath.Join(tempDir, "bundle1.tar.gz")
		bundle2 := filepath.Join(tempDir, "bundle2.tar.gz")
		bundle3 := filepath.Join(tempDir, "bundle3.tar.gz")

		if err := os.WriteFile(bundle1, []byte("bundle1"), 0644); err != nil {
			t.Fatalf("Failed to create bundle1: %v", err)
		}

		if err := os.WriteFile(bundle2, []byte("bundle2"), 0644); err != nil {
			t.Fatalf("Failed to create bundle2: %v", err)
		}

		if err := os.WriteFile(bundle3, []byte("bundle3"), 0644); err != nil {
			t.Fatalf("Failed to create bundle3: %v", err)
		}

		// Explicitly set different modification times to avoid relying on time.Sleep
		now := time.Now()
		if err := os.Chtimes(bundle1, now.Add(-2*time.Second), now.Add(-2*time.Second)); err != nil {
			t.Fatalf("Failed to set mtime for bundle1: %v", err)
		}
		if err := os.Chtimes(bundle2, now.Add(-1*time.Second), now.Add(-1*time.Second)); err != nil {
			t.Fatalf("Failed to set mtime for bundle2: %v", err)
		}
		if err := os.Chtimes(bundle3, now, now); err != nil {
			t.Fatalf("Failed to set mtime for bundle3: %v", err)
		}
		// Test finding the latest bundle
		latest, err := findLatestBundle(tempDir)
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}
		expected := bundle3

		if latest != expected {
			t.Errorf("Expected latest bundle to be %q, got %q", expected, latest)
		}
	})

	t.Run("Directory with no bundles", func(t *testing.T) {
		tempDir := t.TempDir()

		// Create some non-bundle files
		if err := os.WriteFile(filepath.Join(tempDir, "readme.txt"), []byte("readme"), 0644); err != nil {
			t.Fatalf("Failed to create readme: %v", err)
		}
		if err := os.WriteFile(filepath.Join(tempDir, "config.json"), []byte("{}"), 0644); err != nil {
			t.Fatalf("Failed to create config: %v", err)
		}

		latest, err := findLatestBundle(tempDir)
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}
		if latest != "" {
			t.Errorf("Expected empty string when no bundles found, got %q", latest)
		}
	})

	t.Run("Non-existent directory", func(t *testing.T) {
		latest, err := findLatestBundle("/nonexistent/directory")
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}
		if latest != "" {
			t.Errorf("Expected empty string for non-existent directory, got %q", latest)
		}
	})

	t.Run("Directory with mixed files", func(t *testing.T) {
		tempDir := t.TempDir()

		// Create mix of files
		if err := os.WriteFile(filepath.Join(tempDir, "bundle.tar.gz"), []byte("bundle"), 0644); err != nil {
			t.Fatalf("Failed to create bundle: %v", err)
		}
		if err := os.WriteFile(filepath.Join(tempDir, "readme.txt"), []byte("readme"), 0644); err != nil {
			t.Fatalf("Failed to create readme: %v", err)
		}
		if err := os.WriteFile(filepath.Join(tempDir, "data.tar"), []byte("data"), 0644); err != nil {
			t.Fatalf("Failed to create data: %v", err)
		}
		if err := os.WriteFile(filepath.Join(tempDir, "backup.tar.gz.bak"), []byte("backup"), 0644); err != nil {
			t.Fatalf("Failed to create backup: %v", err)
		}

		latest, err := findLatestBundle(tempDir)
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}
		expected := filepath.Join(tempDir, "bundle.tar.gz")

		if latest != expected {
			t.Errorf("Expected latest bundle to be %q, got %q", expected, latest)
		}
	})

	t.Run("Directory with only .tar.gz files", func(t *testing.T) {
		tempDir := t.TempDir()

		// Create only valid bundle files
		bundles := []string{"app.tar.gz", "service.tar.gz", "tool.tar.gz"}
		baseTime := time.Now()
		for i, name := range bundles {
			bundlePath := filepath.Join(tempDir, name)
			if err := os.WriteFile(bundlePath, []byte(name), 0644); err != nil {
				t.Fatalf("Failed to create %s: %v", name, err)
			}
			// Explicitly set different modification times for deterministic behavior
			ts := baseTime.Add(time.Duration(i) * time.Second)
			if err := os.Chtimes(bundlePath, ts, ts); err != nil {
				t.Fatalf("Failed to set times for %s: %v", name, err)
			}
		}

		latest, err := findLatestBundle(tempDir)
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}
		expected := filepath.Join(tempDir, "tool.tar.gz") // Last created

		if latest != expected {
			t.Errorf("Expected latest bundle to be %q, got %q", expected, latest)
		}
	})
}

// Test for buildAgentBundle function
func Test_buildAgentBundle(t *testing.T) {
	t.Run("Successful build", func(t *testing.T) {
		tempDir := t.TempDir()

		// Create a mock build script that succeeds
		scriptPath := filepath.Join(tempDir, "build-bundle.sh")
		scriptContent := "#!/bin/bash\necho 'Building bundle...'\nexit 0"
		if err := os.WriteFile(scriptPath, []byte(scriptContent), 0755); err != nil {
			t.Fatalf("Failed to create build script: %v", err)
		}

		// Test building the bundle
		err := buildAgentBundle(scriptPath, tempDir)
		if err != nil {
			t.Errorf("Unexpected error building bundle: %v", err)
		}
	})

	t.Run("Build script failure", func(t *testing.T) {
		tempDir := t.TempDir()

		// Create a mock build script that fails
		scriptPath := filepath.Join(tempDir, "build-bundle.sh")
		scriptContent := "#!/bin/bash\necho 'Build failed!' >&2\nexit 1"
		if err := os.WriteFile(scriptPath, []byte(scriptContent), 0755); err != nil {
			t.Fatalf("Failed to create build script: %v", err)
		}

		// Test building the bundle
		err := buildAgentBundle(scriptPath, tempDir)
		if err == nil {
			t.Error("Expected error when build script fails")
		}

		// Verify error message contains command output
		if !strings.Contains(err.Error(), "Build failed!") {
			t.Errorf("Expected error message to contain script output, got: %v", err)
		}
	})

	t.Run("Non-existent build script", func(t *testing.T) {
		tempDir := t.TempDir()
		scriptPath := filepath.Join(tempDir, "nonexistent.sh")

		// Test building with non-existent script
		err := buildAgentBundle(scriptPath, tempDir)
		if err == nil {
			t.Error("Expected error when build script doesn't exist")
		}

		// Verify error mentions the script execution
		errorMsg := err.Error()
		if !strings.Contains(errorMsg, "bash") && !strings.Contains(errorMsg, scriptPath) {
			t.Errorf("Expected error to mention bash or script path, got: %s", errorMsg)
		}
	})
}

// Test for resolveAgentBundle function
func TestRunner_resolveAgentBundle(t *testing.T) {
	runner := NewRunner(&MockRuntime{})
	t.Setenv("HOLON_AGENT", "")
	t.Setenv("HOLON_AGENT_BUNDLE", "")

	t.Run("Direct bundle path provided", func(t *testing.T) {
		tempDir := t.TempDir()

		// Create a bundle file
		bundlePath := filepath.Join(tempDir, "custom-bundle.tar.gz")
		if err := os.WriteFile(bundlePath, []byte("bundle content"), 0644); err != nil {
			t.Fatalf("Failed to create bundle: %v", err)
		}

		cfg := RunnerConfig{
			AgentBundle:   bundlePath,
			WorkspacePath: tempDir,
		}

		resolvedPath, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err != nil {
			t.Errorf("Unexpected error: %v", err)
		}

		if resolvedPath != bundlePath {
			t.Errorf("Expected resolved path to be %q, got %q", bundlePath, resolvedPath)
		}
	})

	t.Run("Agent bundle resolved from HOLON_AGENT", func(t *testing.T) {
		tempDir := t.TempDir()
		bundlePath := filepath.Join(tempDir, "env-bundle.tar.gz")
		if err := os.WriteFile(bundlePath, []byte("bundle content"), 0644); err != nil {
			t.Fatalf("Failed to create bundle: %v", err)
		}

		t.Setenv("HOLON_AGENT", bundlePath)

		cfg := RunnerConfig{
			WorkspacePath: tempDir,
		}

		resolvedPath, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err != nil {
			t.Errorf("Unexpected error: %v", err)
		}

		if resolvedPath != bundlePath {
			t.Errorf("Expected resolved path to be %q, got %q", bundlePath, resolvedPath)
		}
	})

	t.Run("Agent bundle resolved from HOLON_AGENT_BUNDLE (legacy)", func(t *testing.T) {
		tempDir := t.TempDir()
		bundlePath := filepath.Join(tempDir, "legacy-bundle.tar.gz")
		if err := os.WriteFile(bundlePath, []byte("bundle content"), 0644); err != nil {
			t.Fatalf("Failed to create bundle: %v", err)
		}

		t.Setenv("HOLON_AGENT", "")
		t.Setenv("HOLON_AGENT_BUNDLE", bundlePath)

		cfg := RunnerConfig{
			WorkspacePath: tempDir,
		}

		resolvedPath, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err != nil {
			t.Errorf("Unexpected error: %v", err)
		}

		if resolvedPath != bundlePath {
			t.Errorf("Expected resolved path to be %q, got %q", bundlePath, resolvedPath)
		}
	})

	t.Run("Direct bundle path does not exist", func(t *testing.T) {
		cfg := RunnerConfig{
			AgentBundle: "/nonexistent/bundle.tar.gz",
		}

		_, err := runner.resolveAgentBundle(context.Background(), cfg, "")
		if err == nil {
			t.Error("Expected error for non-existent bundle path")
		}

		expectedError := "agent bundle not found"
		if !strings.Contains(err.Error(), expectedError) {
			t.Errorf("Expected error containing %q, got %q", expectedError, err.Error())
		}
	})

	t.Run("Direct bundle path is a directory", func(t *testing.T) {
		tempDir := t.TempDir()

		cfg := RunnerConfig{
			AgentBundle: tempDir,
		}

		_, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err == nil {
			t.Error("Expected error for bundle path directory")
		}
	})

	t.Run("Local bundle found in expected location", func(t *testing.T) {
		// Disable auto-install to test local bundle fallback behavior
		t.Setenv("HOLON_NO_AUTO_INSTALL", "1")

		tempDir := t.TempDir()

		// Create bundle directory structure
		bundleDir := filepath.Join(tempDir, "agents", "claude", "dist", "agent-bundles")
		if err := os.MkdirAll(bundleDir, 0755); err != nil {
			t.Fatalf("Failed to create bundle directory: %v", err)
		}

		// Create build script directory (required for bundle discovery)
		scriptDir := filepath.Join(tempDir, "agents", "claude", "scripts")
		if err := os.MkdirAll(scriptDir, 0755); err != nil {
			t.Fatalf("Failed to create script directory: %v", err)
		}
		scriptPath := filepath.Join(scriptDir, "build-bundle.sh")
		if err := os.WriteFile(scriptPath, []byte("#!/bin/bash\nexit 0"), 0755); err != nil {
			t.Fatalf("Failed to create build script: %v", err)
		}

		// Create a bundle file
		bundlePath := filepath.Join(bundleDir, "local-bundle.tar.gz")
		if err := os.WriteFile(bundlePath, []byte("local bundle"), 0644); err != nil {
			t.Fatalf("Failed to create bundle: %v", err)
		}

		cfg := RunnerConfig{
			WorkspacePath: tempDir,
		}

		resolvedPath, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err != nil {
			t.Errorf("Unexpected error: %v", err)
		}

		if resolvedPath != bundlePath {
			t.Errorf("Expected resolved path to be %q, got %q", bundlePath, resolvedPath)
		}
	})

	t.Run("Build bundle when none exists", func(t *testing.T) {
		// Disable auto-install to test local bundle fallback behavior
		t.Setenv("HOLON_NO_AUTO_INSTALL", "1")

		tempDir := t.TempDir()

		// Create bundle directory structure but no bundle file
		bundleDir := filepath.Join(tempDir, "agents", "claude", "dist", "agent-bundles")
		if err := os.MkdirAll(bundleDir, 0755); err != nil {
			t.Fatalf("Failed to create bundle directory: %v", err)
		}

		// Create build script directory and script
		scriptDir := filepath.Join(tempDir, "agents", "claude", "scripts")
		if err := os.MkdirAll(scriptDir, 0755); err != nil {
			t.Fatalf("Failed to create script directory: %v", err)
		}
		scriptPath := filepath.Join(scriptDir, "build-bundle.sh")
		scriptContent := "#!/bin/bash\n# Create a bundle file\nmkdir -p agents/claude/dist/agent-bundles\ntouch agents/claude/dist/agent-bundles/auto-built.tar.gz\nexit 0"
		if err := os.WriteFile(scriptPath, []byte(scriptContent), 0755); err != nil {
			t.Fatalf("Failed to create build script: %v", err)
		}

		cfg := RunnerConfig{
			WorkspacePath: tempDir,
		}

		resolvedPath, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err != nil {
			t.Errorf("Unexpected error: %v", err)
		}

		expectedBundle := filepath.Join(bundleDir, "auto-built.tar.gz")
		if resolvedPath != expectedBundle {
			t.Errorf("Expected resolved path to be %q, got %q", expectedBundle, resolvedPath)
		}

		// Verify bundle was actually created
		if _, err := os.Stat(expectedBundle); os.IsNotExist(err) {
			t.Errorf("Expected bundle file to be created at %s", expectedBundle)
		}
	})

	t.Run("Build bundle script fails", func(t *testing.T) {
		// Disable auto-install to test local bundle fallback behavior
		t.Setenv("HOLON_NO_AUTO_INSTALL", "1")

		tempDir := t.TempDir()

		// Create bundle directory structure but no bundle file
		bundleDir := filepath.Join(tempDir, "agents", "claude", "dist", "agent-bundles")
		if err := os.MkdirAll(bundleDir, 0755); err != nil {
			t.Fatalf("Failed to create bundle directory: %v", err)
		}

		// Create a failing build script
		scriptDir := filepath.Join(tempDir, "agents", "claude", "scripts")
		if err := os.MkdirAll(scriptDir, 0755); err != nil {
			t.Fatalf("Failed to create script directory: %v", err)
		}
		scriptPath := filepath.Join(scriptDir, "build-bundle.sh")
		scriptContent := "#!/bin/bash\necho 'Build failed' >&2\nexit 1"
		if err := os.WriteFile(scriptPath, []byte(scriptContent), 0755); err != nil {
			t.Fatalf("Failed to create build script: %v", err)
		}

		cfg := RunnerConfig{
			WorkspacePath: tempDir,
		}

		// This should error since bundle building fails
		resolvedPath, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err == nil {
			t.Error("Expected error when build script fails")
		}

		if resolvedPath != "" {
			t.Errorf("Expected resolved path to be empty when script fails, got %q", resolvedPath)
		}

		// Verify error mentions build failure
		errorMsg := err.Error()
		if !strings.Contains(errorMsg, "failed to build agent bundle") {
			t.Errorf("Expected error to mention build failure, got: %s", errorMsg)
		}
	})

	t.Run("No bundle and no build script", func(t *testing.T) {
		// Disable auto-install to test local bundle fallback behavior
		t.Setenv("HOLON_NO_AUTO_INSTALL", "1")

		tempDir := t.TempDir()

		cfg := RunnerConfig{
			WorkspacePath: tempDir,
		}

		_, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err == nil {
			t.Error("Expected error when no bundle and no build script are present")
		}
	})

	t.Run("Multiple bundles - selects latest", func(t *testing.T) {
		// Disable auto-install to test local bundle fallback behavior
		t.Setenv("HOLON_NO_AUTO_INSTALL", "1")

		tempDir := t.TempDir()

		// Create bundle directory structure
		bundleDir := filepath.Join(tempDir, "agents", "claude", "dist", "agent-bundles")
		if err := os.MkdirAll(bundleDir, 0755); err != nil {
			t.Fatalf("Failed to create bundle directory: %v", err)
		}

		// Create build script directory (required for bundle discovery)
		scriptDir := filepath.Join(tempDir, "agents", "claude", "scripts")
		if err := os.MkdirAll(scriptDir, 0755); err != nil {
			t.Fatalf("Failed to create script directory: %v", err)
		}
		scriptPath := filepath.Join(scriptDir, "build-bundle.sh")
		if err := os.WriteFile(scriptPath, []byte("#!/bin/bash\nexit 0"), 0755); err != nil {
			t.Fatalf("Failed to create build script: %v", err)
		}

		// Create multiple bundles with different timestamps
		bundle1 := filepath.Join(bundleDir, "bundle1.tar.gz")
		bundle2 := filepath.Join(bundleDir, "bundle2.tar.gz")

		baseTime := time.Now()

		if err := os.WriteFile(bundle1, []byte("bundle1"), 0644); err != nil {
			t.Fatalf("Failed to create bundle1: %v", err)
		}
		if err := os.Chtimes(bundle1, baseTime, baseTime); err != nil {
			t.Fatalf("Failed to set times for bundle1: %v", err)
		}

		if err := os.WriteFile(bundle2, []byte("bundle2"), 0644); err != nil {
			t.Fatalf("Failed to create bundle2: %v", err)
		}
		newerTime := baseTime.Add(time.Second)
		if err := os.Chtimes(bundle2, newerTime, newerTime); err != nil {
			t.Fatalf("Failed to set times for bundle2: %v", err)
		}

		cfg := RunnerConfig{
			WorkspacePath: tempDir,
		}

		resolvedPath, err := runner.resolveAgentBundle(context.Background(), cfg, tempDir)
		if err != nil {
			t.Errorf("Unexpected error: %v", err)
		}

		if resolvedPath != bundle2 {
			t.Errorf("Expected resolved path to be latest bundle %q, got %q", bundle2, resolvedPath)
		}
	})
}

// TestRunner_GitConfigOverride tests that GitAuthorName and GitAuthorEmail fields
// are properly passed through to environment variables
func TestRunner_GitConfigOverride(t *testing.T) {
	_, workspaceDir, outDir := setupTestEnv(t)
	bundlePath := createDummyBundle(t, outDir)

	tests := []struct {
		name           string
		gitAuthorName  string
		gitAuthorEmail string
		expectedName   string
		expectedEmail  string
	}{
		{
			name:           "both git name and email set",
			gitAuthorName:  "Holon Bot",
			gitAuthorEmail: "holon@example.com",
			expectedName:   "Holon Bot",
			expectedEmail:  "holon@example.com",
		},
		{
			name:           "only git name set",
			gitAuthorName:  "Bot Name",
			gitAuthorEmail: "",
			expectedName:   "Bot Name",
			expectedEmail:  "",
		},
		{
			name:           "only git email set",
			gitAuthorName:  "",
			gitAuthorEmail: "bot@example.com",
			expectedName:   "",
			expectedEmail:  "bot@example.com",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockRuntime := &MockRuntime{}
			runner := NewRunner(mockRuntime)

			cfg := RunnerConfig{
				GoalStr:       "Test goal",
				TaskName:      "test-git-config",
				WorkspacePath: workspaceDir,
				OutDir:        outDir,
				BaseImage:     "test-image",
				AgentBundle:   bundlePath,
				GitAuthorName: tt.gitAuthorName,
				GitAuthorEmail: tt.gitAuthorEmail,
			}

			err := runner.Run(context.Background(), cfg)
			if err != nil {
				t.Errorf("Unexpected error: %v", err)
			}

			calls := mockRuntime.GetCalls()
			if len(calls) != 1 {
				t.Fatalf("Expected 1 call to RunHolon, got %d", len(calls))
			}

			env := calls[0].cfg.Env

			// Verify GIT_AUTHOR_NAME and GIT_COMMITTER_NAME
			if tt.gitAuthorName != "" {
				if env["GIT_AUTHOR_NAME"] != tt.expectedName {
					t.Errorf("Expected GIT_AUTHOR_NAME to be %q, got %q", tt.expectedName, env["GIT_AUTHOR_NAME"])
				}
				if env["GIT_COMMITTER_NAME"] != tt.expectedName {
					t.Errorf("Expected GIT_COMMITTER_NAME to be %q, got %q", tt.expectedName, env["GIT_COMMITTER_NAME"])
				}
			}

			// Verify GIT_AUTHOR_EMAIL and GIT_COMMITTER_EMAIL
			if tt.gitAuthorEmail != "" {
				if env["GIT_AUTHOR_EMAIL"] != tt.expectedEmail {
					t.Errorf("Expected GIT_AUTHOR_EMAIL to be %q, got %q", tt.expectedEmail, env["GIT_AUTHOR_EMAIL"])
				}
				if env["GIT_COMMITTER_EMAIL"] != tt.expectedEmail {
					t.Errorf("Expected GIT_COMMITTER_EMAIL to be %q, got %q", tt.expectedEmail, env["GIT_COMMITTER_EMAIL"])
				}
			}
		})
	}
}
