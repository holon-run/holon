package prompt

import (
	"fmt"
	"strings"
	"testing"
	"testing/fstest"
)

func TestCompileSystemPrompt(t *testing.T) {
	// Mock Assets - Simplified structure (no roles)
	mockFS := fstest.MapFS{
		"manifest.yaml": {Data: []byte(`
version: 1.0.0
defaults:
  mode: run
`)},
		"contracts/common.md": {Data: []byte("Common: {{ .WorkingDir }}")},
	}

	compiler := NewCompilerFromFS(mockFS)

	t.Run("Default Mode", func(t *testing.T) {
		prompt, err := compiler.CompileSystemPrompt(Config{
			WorkingDir: "/test/ws",
		})
		if err != nil {
			t.Fatalf("Compile failed: %v", err)
		}

		if !strings.Contains(prompt, "Common: /test/ws") {
			t.Errorf("Prompt missing common contract data: %s", prompt)
		}
	})

	t.Run("Explicit Mode with contract", func(t *testing.T) {
		mockFS["modes/serve/contract.md"] = &fstest.MapFile{Data: []byte("Serve Mode Contract")}

		prompt, err := compiler.CompileSystemPrompt(Config{
			Mode:       "serve",
			WorkingDir: "/test/ws",
		})
		if err != nil {
			t.Fatalf("Compile failed: %v", err)
		}

		if !strings.Contains(prompt, "Common: /test/ws") {
			t.Errorf("Prompt missing common contract data: %s", prompt)
		}
		if !strings.Contains(prompt, "Serve Mode Contract") {
			t.Errorf("Prompt missing mode contract data: %s", prompt)
		}
	})
}

func TestEmbeddedAssets(t *testing.T) {
	// This test uses the REAL embedded assets
	compiler := NewCompiler("")

	prompt, err := compiler.CompileSystemPrompt(Config{
		WorkingDir: "/real/ws",
	})

	if err != nil {
		t.Fatalf("Embedded assets compilation failed: %v", err)
	}

	// Verify some known content from our real assets
	if !strings.Contains(prompt, "HOLON CONTRACT") {
		t.Errorf("Expected 'HOLON CONTRACT' in compiled prompt, got: %s", prompt[:100])
	}
	if !strings.Contains(prompt, "Holon does not inline persona file contents") {
		t.Errorf("Expected persona self-read contract in prompt, got: %s", prompt)
	}
	if !strings.Contains(prompt, "Sandbox Environment") {
		t.Errorf("Expected 'Sandbox Environment' section in prompt, got: %s", prompt)
	}
	if !strings.Contains(prompt, "Agent-Home Protocol") {
		t.Errorf("Expected 'Agent-Home Protocol' section in prompt, got: %s", prompt)
	}
	// Verify no role layer appears
	if strings.Contains(prompt, "### ROLE:") {
		t.Errorf("Prompt should not contain role layer, but it did: %s", prompt)
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
				"contracts/common.md": {Data: []byte("Common content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read manifest",
		},
		{
			name: "Invalid YAML in manifest",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("invalid: yaml: content: [")},
				"contracts/common.md": {Data: []byte("Contract content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to parse manifest",
		},
		{
			name: "Missing common contract file",
			mockFS: fstest.MapFS{
				"manifest.yaml": {Data: []byte("version: 1.0.0\ndefaults:\n  mode: run\n")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read common contract",
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
			name:         "Goal only",
			goal:         "Implement a new feature",
			contextFiles: []string{},
			expected:     "### TASK GOAL\nImplement a new feature\n",
		},
		{
			name:         "Goal with single context file",
			goal:         "Fix the bug in main.go",
			contextFiles: []string{"main.go"},
			expected:     "### TASK GOAL\nFix the bug in main.go\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at ${HOLON_INPUT_DIR}/context/:\n- main.go\n",
		},
		{
			name:         "Goal with multiple context files",
			goal:         "Refactor the module",
			contextFiles: []string{"file1.go", "file2.go", "config.yaml"},
			expected:     "### TASK GOAL\nRefactor the module\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at ${HOLON_INPUT_DIR}/context/:\n- file1.go\n- file2.go\n- config.yaml\n",
		},
		{
			name:         "Empty goal with context files",
			goal:         "",
			contextFiles: []string{"test.go"},
			expected:     "### TASK GOAL\n\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at ${HOLON_INPUT_DIR}/context/:\n- test.go\n",
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

func TestCompileModeUserPrompt(t *testing.T) {
	t.Run("Compile serve mode user prompt", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: run\n")},
			"modes/serve/user.md": {Data: []byte("Serve user: mode={{ .Mode }} wd={{ .WorkingDir }}")},
			"contracts/common.md": {Data: []byte("Common")},
		}
		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileModeUserPrompt(Config{
			Mode:       "serve",
			WorkingDir: "/root/workspace",
		})
		if err != nil {
			t.Fatalf("CompileModeUserPrompt() error: %v", err)
		}
		if !strings.Contains(prompt, "mode=serve") {
			t.Fatalf("expected serve mode in prompt, got: %q", prompt)
		}
		if !strings.Contains(prompt, "wd=/root/workspace") {
			t.Fatalf("expected working dir in prompt, got: %q", prompt)
		}
	})

	t.Run("Missing mode user prompt returns error", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: run\n")},
			"contracts/common.md": {Data: []byte("Common")},
		}
		compiler := NewCompilerFromFS(mockFS)
		_, err := compiler.CompileModeUserPrompt(Config{Mode: "serve"})
		if err == nil {
			t.Fatal("expected error when mode user prompt is missing")
		}
		if !strings.Contains(err.Error(), "failed to read mode user prompt modes/serve/user.md") {
			t.Fatalf("unexpected error: %v", err)
		}
	})
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
	if !strings.Contains(result, "${HOLON_INPUT_DIR}/context/") {
		t.Error("Missing '${HOLON_INPUT_DIR}/context/' path wording")
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
	t.Run("Fallback to default mode when manifest has no mode", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n")},
			"contracts/common.md": {Data: []byte("Common: {{ .WorkingDir }}")},
		}
		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileSystemPrompt(Config{WorkingDir: "/test"})
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}
		// Should compile successfully with default mode (run) and no mode-specific layers
		if !strings.Contains(prompt, "Common: /test") {
			t.Errorf("Prompt missing common contract data: %s", prompt)
		}
	})
}

// TestModeOverlayLoading tests mode-specific contract loading
func TestModeOverlayLoading(t *testing.T) {
	tests := []struct {
		name                string
		mockFS              fstest.MapFS
		cfg                 Config
		expectedInPrompt    []string
		notExpectedInPrompt []string
	}{
		{
			name: "Serve mode loads serve contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: run\n")},
				"contracts/common.md":     {Data: []byte("Common Contract")},
				"modes/serve/contract.md": {Data: []byte("Serve Mode Contract")},
			},
			cfg:              Config{Mode: "serve", WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Serve Mode Contract"},
		},
		{
			name: "Run mode with no mode contract is fine",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: run\n")},
				"contracts/common.md": {Data: []byte("Common Contract")},
			},
			cfg:              Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract"},
		},
		{
			name: "Mode overlay is layered after mode contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: serve\n")},
				"contracts/common.md":     {Data: []byte("Common Contract")},
				"modes/serve/contract.md": {Data: []byte("Serve Mode Contract")},
				"modes/serve/overlay.md":  {Data: []byte("Serve Mode Overlay")},
			},
			cfg:              Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Serve Mode Contract", "Serve Mode Overlay"},
		},
		{
			name: "Missing mode contract is handled gracefully",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: run\n")},
				"contracts/common.md": {Data: []byte("Common Contract")},
			},
			cfg:              Config{Mode: "missing-mode", WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract"},
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

func TestModeContextLoading(t *testing.T) {
	t.Run("mode context loads and renders entries", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: serve\n")},
			"contracts/common.md":     {Data: []byte("Common")},
			"modes/serve/contract.md": {Data: []byte("Serve Contract")},
			"modes/serve/overlay.md":  {Data: []byte("Serve Overlay")},
			"modes/serve/context.md":  {Data: []byte("Context Files: {{range .ContextEntries}}{{.Path}} {{.Description}};{{end}}")},
		}
		compiler := NewCompilerFromFS(mockFS)
		cfg := Config{
			WorkingDir: "/test",
			ContextEntries: []ContextEntry{
				{Path: "github/issue.json", Description: "Issue metadata"},
			},
		}
		prompt, err := compiler.CompileSystemPrompt(cfg)
		if err != nil {
			t.Fatalf("compile failed: %v", err)
		}
		if !strings.Contains(prompt, "Context Files: github/issue.json Issue metadata;") {
			t.Fatalf("expected context entries rendered, got prompt: %s", prompt)
		}
		overlayIdx := strings.Index(prompt, "Serve Overlay")
		contextIdx := strings.Index(prompt, "Context Files")
		if overlayIdx == -1 || contextIdx == -1 || overlayIdx > contextIdx {
			t.Fatalf("expected overlay to appear before context block; prompt: %s", prompt)
		}
	})

	t.Run("missing mode context handled gracefully", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: serve\n")},
			"contracts/common.md":     {Data: []byte("Common")},
			"modes/serve/contract.md": {Data: []byte("Serve Contract")},
		}
		compiler := NewCompilerFromFS(mockFS)
		if _, err := compiler.CompileSystemPrompt(Config{WorkingDir: "/test"}); err != nil {
			t.Fatalf("compile should succeed without context file: %v", err)
		}
	})
}

// TestBackwardCompatibility tests that basic compilation still works
func TestBackwardCompatibility(t *testing.T) {
	t.Run("No mode specified uses default from manifest", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: run\n")},
			"contracts/common.md": {Data: []byte("Common")},
		}

		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileSystemPrompt(Config{WorkingDir: "/test"})
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}

		if !strings.Contains(prompt, "Common") {
			t.Error("Missing common contract")
		}
	})
}

// TestLegacyContractPathsIgnored verifies that the compiler uses contracts/common.md
func TestLegacyContractPathsIgnored(t *testing.T) {
	t.Run("Compiler uses contracts/common.md not contract/v1.md", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: run\n")},
			"contracts/common.md": {Data: []byte("NEW Common Contract Content")},
			"contract/v1.md":      {Data: []byte("OLD Legacy Contract Content - SHOULD NOT APPEAR")},
		}

		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileSystemPrompt(Config{WorkingDir: "/test"})
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}

		if !strings.Contains(prompt, "NEW Common Contract Content") {
			t.Errorf("Expected prompt to contain NEW common contract content, got: %s", prompt)
		}

		if strings.Contains(prompt, "OLD Legacy Contract Content") {
			t.Errorf("Prompt should NOT contain legacy contract/v1.md content, but it did: %s", prompt)
		}
	})
}
