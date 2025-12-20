package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/jolestar/holon/pkg/runtime/docker"
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
			// Verify that the spec file exists
			if _, err := os.Stat(cfg.SpecPath); os.IsNotExist(err) {
				t.Errorf("Expected spec file to exist at %s", cfg.SpecPath)
			}
			return nil
		},
	}
	runner := NewRunner(mockRuntime)

	_, workspaceDir, outDir := setupTestEnv(t)

	cfg := RunnerConfig{
		GoalStr:       "Test goal",
		TaskName:      "test-task",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
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

	cfg := RunnerConfig{
		SpecPath:      specPath,
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
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

	// Set environment variables for auto-injection
	t.Setenv("ANTHROPIC_API_KEY", "test-key")
	t.Setenv("GITHUB_TOKEN", "test-token")

	cfg := RunnerConfig{
		SpecPath:      specPath,
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
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

	cfg := RunnerConfig{
		SpecPath:      specPath, // No goal string provided, should extract from spec
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
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

	_, workspaceDir, outDir := setupTestEnv(t)

	cfg := RunnerConfig{
		GoalStr:       "Test goal for debug prompts",
		TaskName:      "test-debug",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
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

	_, workspaceDir, outDir := setupTestEnv(t)

	// Test without explicit log level
	cfg1 := RunnerConfig{
		GoalStr:       "Test goal",
		TaskName:      "test-log-1",
		WorkspacePath: workspaceDir,
		OutDir:        outDir,
		BaseImage:     "test-image",
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
	sysPrompt, userPrompt, promptTempDir, err := runner.compilePrompts(cfg, "")
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
			if cfg.AdapterImage != "holon-adapter-claude" {
				t.Errorf("Expected AdapterImage to be 'holon-adapter-claude', got %q", cfg.AdapterImage)
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

	cfg := RunnerConfig{
		SpecPath:      specPath,
		WorkspacePath: workspaceDir,
		ContextPath:   contextDir,
		OutDir:        outDir,
		BaseImage:     "golang:1.22",
		RoleName:      "coder",
		EnvVarsList:   []string{"CLI_VAR=cli-value"},
		LogLevel:      "debug",
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

	// Verify debug prompts were created
	if _, err := os.Stat(filepath.Join(outDir, "prompt.compiled.system.md")); os.IsNotExist(err) {
		t.Error("Expected system prompt debug file")
	}
	if _, err := os.Stat(filepath.Join(outDir, "prompt.compiled.user.md")); os.IsNotExist(err) {
		t.Error("Expected user prompt debug file")
	}
}
