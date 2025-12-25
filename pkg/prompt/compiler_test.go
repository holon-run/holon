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
  mode: solve
  role: developer
  contract: v1
`)},
		"contracts/common.md":     {Data: []byte("Common: {{ .WorkingDir }}")},
		"modes/solve/contract.md": {Data: []byte("Solve Mode Contract")},
		"roles/developer.md":      {Data: []byte("Role: Developer")},
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
		if !strings.Contains(prompt, "Solve Mode Contract") {
			t.Errorf("Prompt missing mode contract data: %s", prompt)
		}
		if !strings.Contains(prompt, "Role: Developer") {
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

	t.Run("Coder role alias maps to developer", func(t *testing.T) {
		prompt, err := compiler.CompileSystemPrompt(Config{
			Role:       "coder",
			WorkingDir: "/test/ws",
		})
		if err != nil {
			t.Fatalf("Compile failed: %v", err)
		}

		if !strings.Contains(prompt, "Role: Developer") {
			t.Errorf("Coder role should map to Developer: %s", prompt)
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
				"contracts/common.md":     {Data: []byte("Common content")},
				"modes/solve/contract.md": {Data: []byte("Solve mode")},
				"roles/developer.md":      {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read manifest",
		},
		{
			name: "Invalid YAML in manifest",
			mockFS: fstest.MapFS{
				"manifest.yaml":           {Data: []byte("invalid: yaml: content: [")},
				"contracts/common.md":     {Data: []byte("Contract content")},
				"modes/solve/contract.md": {Data: []byte("Solve mode")},
				"roles/developer.md":      {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to parse manifest",
		},
		{
			name: "Missing common contract file",
			mockFS: fstest.MapFS{
				"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
				"modes/solve/contract.md": {Data: []byte("Solve mode")},
				"roles/developer.md":        {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read common contract",
		},
		{
			name: "Missing role file",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: architect\n")},
				"contracts/common.md": {Data: []byte("Common content")},
				"roles/developer.md":  {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read role architect",
		},
		{
			name: "Missing role file with explicit role",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
				"contracts/common.md": {Data: []byte("Common content")},
				"roles/developer.md":  {Data: []byte("Role content")},
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
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n")},
				"contracts/common.md": {Data: []byte("Common: {{ .WorkingDir }}")},
				"roles/developer.md":  {Data: []byte("Default Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			check: func(prompt string) bool {
				return strings.Contains(prompt, "Default Role")
			},
		},
		{
			name: "Fallback to default mode when manifest has no mode",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  role: developer\n")},
				"contracts/common.md": {Data: []byte("Common: {{ .WorkingDir }}")},
				"roles/developer.md":  {Data: []byte("Role content")},
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
			name: "Solve mode loads solve contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
				"contracts/common.md":     {Data: []byte("Common Contract")},
				"modes/solve/contract.md": {Data: []byte("Solve Mode Overlay")},
				"roles/developer.md":      {Data: []byte("Developer Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Solve Mode Overlay", "Developer Role"},
		},
		{
			name: "PR-fix mode loads pr-fix contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":              {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
				"contracts/common.md":        {Data: []byte("Common Contract")},
				"modes/pr-fix/contract.md":   {Data: []byte("PR-Fix Mode Overlay")},
				"modes/solve/contract.md":    {Data: []byte("Solve Mode Overlay")},
				"roles/developer.md":         {Data: []byte("Developer Role")},
			},
			cfg: Config{Mode: "pr-fix", WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "PR-Fix Mode Overlay", "Developer Role"},
			notExpectedInPrompt: []string{"Solve Mode Overlay"},
		},
		{
			name: "Mode overlay is layered after mode contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":             {Data: []byte("version: 1.0.0\ndefaults:\n  mode: pr-fix\n  role: developer\n")},
				"contracts/common.md":       {Data: []byte("Common Contract")},
				"modes/pr-fix/contract.md":  {Data: []byte("PR-Fix Mode Contract")},
				"modes/pr-fix/overlay.md":   {Data: []byte("PR-Fix Mode Overlay")},
				"roles/developer.md":        {Data: []byte("Developer Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Developer Role", "PR-Fix Mode Contract", "PR-Fix Mode Overlay"},
		},
		{
			name: "Missing mode contract is handled gracefully",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
				"contracts/common.md": {Data: []byte("Common Contract")},
				"roles/developer.md":  {Data: []byte("Developer Role")},
			},
			cfg: Config{Mode: "missing-mode", WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Developer Role"},
		},
		{
			name: "Role overlay layers on top of base role",
			mockFS: fstest.MapFS{
				"manifest.yaml":             {Data: []byte("version: 1.0.0\ndefaults:\n  mode: pr-fix\n  role: developer\n")},
				"contracts/common.md":       {Data: []byte("Common Contract")},
				"modes/pr-fix/contract.md":  {Data: []byte("PR-Fix Mode")},
				"modes/pr-fix/overlays/developer.md": {Data: []byte("PR-Fix Developer Overlay")},
				"roles/developer.md":        {Data: []byte("Base Developer Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Base Developer Role", "PR-Fix Mode", "PR-Fix Developer Overlay"},
		},
		{
			name: "Role overlay is only loaded for selected role",
			mockFS: fstest.MapFS{
				"manifest.yaml":             {Data: []byte("version: 1.0.0\ndefaults:\n  mode: pr-fix\n  role: developer\n")},
				"contracts/common.md":       {Data: []byte("Common Contract")},
				"modes/pr-fix/overlays/developer.md": {Data: []byte("Developer Overlay")},
				"modes/pr-fix/overlays/architect.md": {Data: []byte("Architect Overlay")},
				"roles/developer.md":        {Data: []byte("Base Developer Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Base Developer Role", "Developer Overlay"},
			notExpectedInPrompt: []string{"Architect Overlay"},
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
			"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
			"contracts/common.md":     {Data: []byte("Common")},
			"modes/solve/contract.md": {Data: []byte("Solve")},
			"roles/developer.md":      {Data: []byte("Developer")},
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
		if !strings.Contains(prompt, "Solve") {
			t.Error("Missing solve mode contract")
		}
		if !strings.Contains(prompt, "Developer") {
			t.Error("Missing developer role")
		}
	})

	t.Run("Coder role alias maps to developer", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
			"contracts/common.md": {Data: []byte("Common")},
			"roles/developer.md":  {Data: []byte("Developer Role Content")},
		}

		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileSystemPrompt(Config{Role: "coder", WorkingDir: "/test"})
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}

		if !strings.Contains(prompt, "Developer Role Content") {
			t.Error("Coder role should map to developer role content")
		}
	})
}

// TestLegacyContractPathsIgnored verifies that the compiler ignores legacy contract paths
// and only uses the new contracts/common.md path.
func TestLegacyContractPathsIgnored(t *testing.T) {
	t.Run("Compiler uses contracts/common.md not contract/v1.md", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
			"contracts/common.md":     {Data: []byte("NEW Common Contract Content")},
			"contract/v1.md":          {Data: []byte("OLD Legacy Contract Content - SHOULD NOT APPEAR")},
			"modes/solve/contract.md": {Data: []byte("Solve Mode")},
			"roles/developer.md":      {Data: []byte("Developer Role")},
		}

		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileSystemPrompt(Config{WorkingDir: "/test"})
		if err != nil {
			t.Fatalf("Unexpected error: %v", err)
		}

		// Verify the NEW contract is used
		if !strings.Contains(prompt, "NEW Common Contract Content") {
			t.Errorf("Expected prompt to contain NEW common contract content, got: %s", prompt)
		}

		// Verify the OLD legacy contract is NOT used
		if strings.Contains(prompt, "OLD Legacy Contract Content") {
			t.Errorf("Prompt should NOT contain legacy contract/v1.md content, but it did: %s", prompt)
		}

		// Verify it still includes the other expected layers
		if !strings.Contains(prompt, "Solve Mode") {
			t.Errorf("Expected prompt to contain solve mode contract, got: %s", prompt)
		}
		if !strings.Contains(prompt, "Developer Role") {
			t.Errorf("Expected prompt to contain developer role, got: %s", prompt)
		}
	})

	t.Run("Compiler works without legacy contract path entirely", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: solve\n  role: developer\n")},
			"contracts/common.md":     {Data: []byte("Active Common Contract")},
			"modes/solve/contract.md": {Data: []byte("Solve Mode")},
			"roles/developer.md":      {Data: []byte("Developer Role")},
			// NOTE: No contract/v1.md present at all
		}

		compiler := NewCompilerFromFS(mockFS)
		prompt, err := compiler.CompileSystemPrompt(Config{WorkingDir: "/test"})
		if err != nil {
			t.Fatalf("Compiler should work without legacy contract path, got error: %v", err)
		}

		if !strings.Contains(prompt, "Active Common Contract") {
			t.Errorf("Expected prompt to contain common contract, got: %s", prompt)
		}
	})
}

