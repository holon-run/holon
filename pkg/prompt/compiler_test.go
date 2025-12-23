package prompt

import (
	"fmt"
	"strings"
	"testing"
	"testing/fstest"
)

func TestCompileSystemPrompt(t *testing.T) {
	// Mock Assets - New layered structure
	mockFS := fstest.MapFS{
		"manifest.yaml": {Data: []byte(`
version: 1.0.0
defaults:
  mode: execute
  role: coder
  contract: v1
`)},
		"contracts/common.md":      {Data: []byte("Common: {{ .WorkingDir }}")},
		"modes/execute/contract.md": {Data: []byte("Execute Mode Contract")},
		"roles/coder.md":            {Data: []byte("Role: Coder")},
	}

	compiler := NewCompilerFromFS(mockFS)

	t.Run("Default Role and Mode", func(t *testing.T) {
		prompt, err := compiler.CompileSystemPrompt(Config{
			WorkingDir: "/test/ws",
		})
		if err != nil {
			t.Fatalf("Compile failed: %v", err)
		}

		if !strings.Contains(prompt, "Common: /test/ws") {
			t.Errorf("Prompt missing common contract data: %s", prompt)
		}
		if !strings.Contains(prompt, "Execute Mode Contract") {
			t.Errorf("Prompt missing mode contract data: %s", prompt)
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

	t.Run("Developer role alias maps to coder", func(t *testing.T) {
		prompt, err := compiler.CompileSystemPrompt(Config{
			Role:       "developer",
			WorkingDir: "/test/ws",
		})
		if err != nil {
			t.Fatalf("Compile failed: %v", err)
		}

		if !strings.Contains(prompt, "Role: Coder") {
			t.Errorf("Developer role should map to Coder: %s", prompt)
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
				"contracts/common.md":      {Data: []byte("Common content")},
				"modes/execute/contract.md": {Data: []byte("Execute mode")},
				"roles/coder.md":            {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read manifest",
		},
		{
			name: "Invalid YAML in manifest",
			mockFS: fstest.MapFS{
				"manifest.yaml":             {Data: []byte("invalid: yaml: content: [")},
				"contracts/common.md":       {Data: []byte("Contract content")},
				"modes/execute/contract.md": {Data: []byte("Execute mode")},
				"roles/coder.md":            {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to parse manifest",
		},
		{
			name: "Missing common contract file",
			mockFS: fstest.MapFS{
				"manifest.yaml":             {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: coder\n")},
				"modes/execute/contract.md": {Data: []byte("Execute mode")},
				"roles/coder.md":            {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read common contract",
		},
		{
			name: "Missing role file",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: architect\n")},
				"contracts/common.md": {Data: []byte("Common content")},
				"roles/coder.md":      {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read role architect",
		},
		{
			name: "Missing role file with explicit role",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: coder\n")},
				"contracts/common.md": {Data: []byte("Common content")},
				"roles/coder.md":      {Data: []byte("Role content")},
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
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n")},
				"contracts/common.md": {Data: []byte("Common: {{ .WorkingDir }}")},
				"roles/coder.md":      {Data: []byte("Default Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			check: func(prompt string) bool {
				return strings.Contains(prompt, "Default Role")
			},
		},
		{
			name: "Fallback to default mode when manifest has no mode",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  role: coder\n")},
				"contracts/common.md": {Data: []byte("Common: {{ .WorkingDir }}")},
				"roles/coder.md":      {Data: []byte("Role content")},
			},
			cfg: Config{WorkingDir: "/test"},
			check: func(prompt string) bool {
				return strings.Contains(prompt, "Role content")
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

// TestModeOverlayLoading tests mode-specific contract loading
func TestModeOverlayLoading(t *testing.T) {
	tests := []struct {
		name              string
		mockFS            fstest.MapFS
		cfg               Config
		expectedInPrompt  []string
		notExpectedInPrompt []string
	}{
		{
			name: "Execute mode loads execute contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: coder\n")},
				"contracts/common.md":     {Data: []byte("Common Contract")},
				"modes/execute/contract.md": {Data: []byte("Execute Mode Overlay")},
				"roles/coder.md":           {Data: []byte("Coder Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Execute Mode Overlay", "Coder Role"},
		},
		{
			name: "Review-fix mode loads review-fix contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":             {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: coder\n")},
				"contracts/common.md":       {Data: []byte("Common Contract")},
				"modes/review-fix/contract.md": {Data: []byte("Review-Fix Mode Overlay")},
				"modes/execute/contract.md":  {Data: []byte("Execute Mode Overlay")},
				"roles/coder.md":            {Data: []byte("Coder Role")},
			},
			cfg: Config{Mode: "review-fix", WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Review-Fix Mode Overlay", "Coder Role"},
			notExpectedInPrompt: []string{"Execute Mode Overlay"},
		},
		{
			name: "Missing mode contract is handled gracefully",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: coder\n")},
				"contracts/common.md": {Data: []byte("Common Contract")},
				"roles/coder.md":      {Data: []byte("Coder Role")},
			},
			cfg: Config{Mode: "missing-mode", WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Coder Role"},
		},
		{
			name: "Mode-specific role overlay takes precedence",
			mockFS: fstest.MapFS{
				"manifest.yaml":              {Data: []byte("version: 1.0.0\ndefaults:\n  mode: review-fix\n  role: coder\n")},
				"contracts/common.md":        {Data: []byte("Common Contract")},
				"modes/review-fix/contract.md": {Data: []byte("Review-Fix Mode")},
				"modes/review-fix/roles/coder.md": {Data: []byte("Review-Fix Specific Coder")},
				"roles/coder.md":             {Data: []byte("Generic Coder")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Review-Fix Mode", "Review-Fix Specific Coder"},
			notExpectedInPrompt: []string{"Generic Coder"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			compiler := NewCompilerFromFS(tt.mockFS)
			prompt, err := compiler.CompileSystemPrompt(tt.cfg)
			if err != nil {
				t.Fatalf("Unexpected error: %v", err)
			}

			for _, expected := range tt.expectedInPrompt {
				if !strings.Contains(prompt, expected) {
					t.Errorf("Expected prompt to contain %q, but it was missing. Prompt: %s", expected, prompt)
				}
			}

			for _, notExpected := range tt.notExpectedInPrompt {
				if strings.Contains(prompt, notExpected) {
					t.Errorf("Expected prompt NOT to contain %q, but it was present. Prompt: %s", notExpected, prompt)
				}
			}
		})
	}
}

// TestBackwardCompatibility tests that old behavior still works
func TestBackwardCompatibility(t *testing.T) {
	t.Run("No mode specified uses default from manifest", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: coder\n")},
			"contracts/common.md":     {Data: []byte("Common")},
			"modes/execute/contract.md": {Data: []byte("Execute")},
			"roles/coder.md":          {Data: []byte("Coder")},
		}

		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileSystemPrompt(Config{WorkingDir: "/test"})
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}

		// Should include all three layers
		if !strings.Contains(prompt, "Common") {
			t.Error("Missing common contract")
		}
		if !strings.Contains(prompt, "Execute") {
			t.Error("Missing execute mode contract")
		}
		if !strings.Contains(prompt, "Coder") {
			t.Error("Missing coder role")
		}
	})

	t.Run("Developer role alias maps to coder", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: coder\n")},
			"contracts/common.md": {Data: []byte("Common")},
			"roles/coder.md":      {Data: []byte("Coder Role Content")},
		}

		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileSystemPrompt(Config{Role: "developer", WorkingDir: "/test"})
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}

		if !strings.Contains(prompt, "Coder Role Content") {
			t.Error("Developer role should map to coder role content")
		}
	})
}
