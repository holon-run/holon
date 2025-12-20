package prompt

import (
	"fmt"
	"strings"
	"testing"
	"testing/fstest"
)

func TestCompileSystemPrompt(t *testing.T) {
	// Mock Assets
	mockFS := fstest.MapFS{
		"manifest.yaml": {Data: []byte(`
version: 1.0.0
defaults:
  role: coder
  contract: v1
`)},
		"contract/v1.md": {Data: []byte("Contract: {{ .WorkingDir }}")},
		"roles/coder.md": {Data: []byte("Role: Coder")},
	}

	compiler := NewCompilerFromFS(mockFS)

	t.Run("Default Role", func(t *testing.T) {
		prompt, err := compiler.CompileSystemPrompt(Config{
			WorkingDir: "/test/ws",
		})
		if err != nil {
			t.Fatalf("Compile failed: %v", err)
		}

		if !strings.Contains(prompt, "Contract: /test/ws") {
			t.Errorf("Prompt missing contract data: %s", prompt)
		}
		if !strings.Contains(prompt, "Role: Coder") {
			t.Errorf("Prompt missing role data: %s", prompt)
		}
	})

	t.Run("Explicit Role", func(t *testing.T) {
		mockFS["roles/architect.md"] = &fstest.MapFile{Data: []byte("Role: Architect")}

		prompt, err := compiler.CompileSystemPrompt(Config{
			Role:       "architect",
			WorkingDir: "/test/ws",
		})
		if err != nil {
			t.Fatalf("Compile failed: %v", err)
		}

		if !strings.Contains(prompt, "Role: Architect") {
			t.Errorf("Prompt should have Architect role: %s", prompt)
		}
	})
}

func TestEmbeddedAssets(t *testing.T) {
	// This test uses the REAL embedded assets
	compiler := NewCompiler("")

	// We expect basic compilation to work if paths are correct (e.g. manifest.yaml vs assets/manifest.yaml)
	prompt, err := compiler.CompileSystemPrompt(Config{
		WorkingDir: "/real/ws",
	})

	if err != nil {
		t.Fatalf("Embedded assets compilation failed: %v. \nCheck if go:embed path and fs.Sub logic handles 'assets' prefix correctly.", err)
	}

	// Verify some known content from our real assets
	if !strings.Contains(prompt, "HOLON CONTRACT") {
		t.Errorf("Expected 'HOLON CONTRACT' in compiled prompt, got: %s", prompt[:100])
	}
}

// TestCompileSystemPromptErrors tests error conditions for CompileSystemPrompt
func TestCompileSystemPromptErrors(t *testing.T) {
	tests := []struct {
		name          string
		mockFS        fstest.MapFS
		cfg           Config
		expectedError string
	}{
		{
			name: "Missing manifest.yaml",
			mockFS: fstest.MapFS{
				"contract/v1.md": {Data: []byte("Contract content")},
				"roles/coder.md": {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read manifest",
		},
		{
			name: "Invalid YAML in manifest",
			mockFS: fstest.MapFS{
				"manifest.yaml": {Data: []byte("invalid: yaml: content: [")},
				"contract/v1.md": {Data: []byte("Contract content")},
				"roles/coder.md": {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to parse manifest",
		},
		{
			name: "Missing contract file",
			mockFS: fstest.MapFS{
				"manifest.yaml": {Data: []byte(`
version: 1.0.0
defaults:
  role: coder
  contract: v2
`)},
				"contract/v1.md": {Data: []byte("Contract v1")},
				"roles/coder.md": {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read contract v2",
		},
		{
			name: "Missing role file",
			mockFS: fstest.MapFS{
				"manifest.yaml": {Data: []byte(`
version: 1.0.0
defaults:
  role: architect
  contract: v1
`)},
				"contract/v1.md": {Data: []byte("Contract content")},
				"roles/coder.md": {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read role architect",
		},
		{
			name: "Missing role file with explicit role",
			mockFS: fstest.MapFS{
				"manifest.yaml": {Data: []byte(`
version: 1.0.0
defaults:
  role: coder
  contract: v1
`)},
				"contract/v1.md": {Data: []byte("Contract content")},
				"roles/coder.md": {Data: []byte("Role content")},
			},
			cfg: Config{
				Role:       "missing-role",
				WorkingDir: "/test",
			},
			expectedError: "failed to read role missing-role",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			compiler := NewCompilerFromFS(tt.mockFS)
			_, err := compiler.CompileSystemPrompt(tt.cfg)
			if err == nil {
				t.Fatalf("Expected error containing %q, got nil", tt.expectedError)
			}
			if !strings.Contains(err.Error(), tt.expectedError) {
				t.Fatalf("Expected error containing %q, got %q", tt.expectedError, err.Error())
			}
		})
	}
}

// TestCompileUserPrompt tests the CompileUserPrompt function output structure
func TestCompileUserPrompt(t *testing.T) {
	compiler := NewCompilerFromFS(fstest.MapFS{})

	tests := []struct {
		name         string
		goal         string
		contextFiles []string
		expected     string
	}{
		{
			name: "Goal only",
			goal: "Implement a new feature",
			contextFiles: []string{},
			expected: "### TASK GOAL\nImplement a new feature\n",
		},
		{
			name: "Goal with single context file",
			goal: "Fix the bug in main.go",
			contextFiles: []string{"main.go"},
			expected: "### TASK GOAL\nFix the bug in main.go\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at /holon/input/context/:\n- main.go\n",
		},
		{
			name: "Goal with multiple context files",
			goal: "Refactor the module",
			contextFiles: []string{"file1.go", "file2.go", "config.yaml"},
			expected: "### TASK GOAL\nRefactor the module\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at /holon/input/context/:\n- file1.go\n- file2.go\n- config.yaml\n",
		},
		{
			name: "Empty goal with context files",
			goal: "",
			contextFiles: []string{"test.go"},
			expected: "### TASK GOAL\n\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at /holon/input/context/:\n- test.go\n",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result, err := compiler.CompileUserPrompt(tt.goal, tt.contextFiles)
			if err != nil {
				t.Fatalf("CompileUserPrompt returned unexpected error: %v", err)
			}
			if result != tt.expected {
				t.Fatalf("Expected output:\n%q\n\nGot:\n%q", tt.expected, result)
			}
		})
	}
}

// TestCompileUserPromptStructure verifies specific structural requirements
func TestCompileUserPromptStructure(t *testing.T) {
	compiler := NewCompilerFromFS(fstest.MapFS{})

	goal := "Test goal"
	contextFiles := []string{"file1.txt", "file2.md"}

	result, err := compiler.CompileUserPrompt(goal, contextFiles)
	if err != nil {
		t.Fatalf("Unexpected error: %v", err)
	}

	// Check for goal section
	if !strings.Contains(result, "### TASK GOAL") {
		t.Error("Missing '### TASK GOAL' section")
	}

	// Check for context files section
	if !strings.Contains(result, "### ADDITIONAL CONTEXT FILES") {
		t.Error("Missing '### ADDITIONAL CONTEXT FILES' section")
	}

	// Check for the specific path wording
	if !strings.Contains(result, "/holon/input/context/") {
		t.Error("Missing '/holon/input/context/' path wording")
	}

	// Check that all context files are listed
	for _, file := range contextFiles {
		if !strings.Contains(result, fmt.Sprintf("- %s", file)) {
			t.Errorf("Context file %s not listed in output", file)
		}
	}
}

// TestCompileSystemPromptFallbacks tests fallback behavior
func TestCompileSystemPromptFallbacks(t *testing.T) {
	tests := []struct {
		name   string
		mockFS fstest.MapFS
		cfg    Config
		check  func(string) bool
	}{
		{
			name: "Fallback to default role when manifest has no role",
			mockFS: fstest.MapFS{
				"manifest.yaml": {Data: []byte(`
version: 1.0.0
defaults:
  contract: v1
`)},
				"contract/v1.md": {Data: []byte("Contract: {{ .WorkingDir }}")},
				"roles/coder.md": {Data: []byte("Default Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			check: func(prompt string) bool {
				return strings.Contains(prompt, "Default Role")
			},
		},
		{
			name: "Fallback to default contract when manifest has no contract",
			mockFS: fstest.MapFS{
				"manifest.yaml": {Data: []byte(`
version: 1.0.0
defaults:
  role: coder
`)},
				"contract/v1.md": {Data: []byte("Default Contract: {{ .WorkingDir }}")},
				"roles/coder.md": {Data: []byte("Role content")},
			},
			cfg: Config{WorkingDir: "/test"},
			check: func(prompt string) bool {
				return strings.Contains(prompt, "Default Contract: /test")
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			compiler := NewCompilerFromFS(tt.mockFS)
			prompt, err := compiler.CompileSystemPrompt(tt.cfg)
			if err != nil {
				t.Fatalf("Unexpected error: %v", err)
			}
			if !tt.check(prompt) {
				t.Errorf("Fallback behavior failed for test: %s", tt.name)
			}
		})
	}
}
