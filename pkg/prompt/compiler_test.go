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
  role: developer
  contract: v1
`)},
		"contracts/common.md":      {Data: []byte("Common: {{ .WorkingDir }}")},
		"modes/execute/contract.md": {Data: []byte("Execute Mode Contract")},
		"roles/developer.md":        {Data: []byte("Role: Developer")},
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
				"contracts/common.md":      {Data: []byte("Common content")},
				"modes/execute/contract.md": {Data: []byte("Execute mode")},
				"roles/developer.md":        {Data: []byte("Role content")},
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
				"roles/developer.md":        {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to parse manifest",
		},
		{
			name: "Missing common contract file",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
				"modes/execute/contract.md": {Data: []byte("Execute mode")},
				"roles/developer.md":        {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read common contract",
		},
		{
			name: "Missing role file",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: architect\n")},
				"contracts/common.md": {Data: []byte("Common content")},
				"roles/developer.md":  {Data: []byte("Role content")},
			},
			cfg:           Config{WorkingDir: "/test"},
			expectedError: "failed to read base role architect",
		},
		{
			name: "Missing role file with explicit role",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
				"contracts/common.md": {Data: []byte("Common content")},
				"roles/developer.md":  {Data: []byte("Role content")},
			},
			cfg: Config{
				Role:       "missing-role",
				WorkingDir: "/test",
			},
			expectedError: "failed to read base role missing-role",
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
			expected:     "### TASK GOAL\nFix the bug in main.go\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at /holon/input/context/:\n- main.go\n",
		},
		{
			name:         "Goal with multiple context files",
			goal:         "Refactor the module",
			contextFiles: []string{"file1.go", "file2.go", "config.yaml"},
			expected:     "### TASK GOAL\nRefactor the module\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at /holon/input/context/:\n- file1.go\n- file2.go\n- config.yaml\n",
		},
		{
			name:         "Empty goal with context files",
			goal:         "",
			contextFiles: []string{"test.go"},
			expected:     "### TASK GOAL\n\n\n\n### ADDITIONAL CONTEXT FILES\nThe following files provide additional context and are available at /holon/input/context/:\n- test.go\n",
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

// TestPromptLayering tests the correct layering order per #148
func TestPromptLayering(t *testing.T) {
	tests := []struct {
		name                 string
		mockFS               fstest.MapFS
		cfg                  Config
		expectedInPrompt     []string
		notExpectedInPrompt  []string
		description          string
	}{
		{
			name: "Default mode+role loads common + role + mode contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
				"contracts/common.md":     {Data: []byte("LAYER: Common Contract")},
				"roles/developer.md":      {Data: []byte("LAYER: Developer Role")},
				"modes/execute/contract.md": {Data: []byte("LAYER: Execute Mode")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{
				"LAYER: Common Contract",
				"LAYER: Developer Role",
				"LAYER: Execute Mode",
			},
			description: "Base case: common + role + mode contract all present",
		},
		{
			name: "Base role is always included even when mode overlays exist",
			mockFS: fstest.MapFS{
				"manifest.yaml":                    {Data: []byte("version: 1.0.0\ndefaults:\n  mode: review-fix\n  role: developer\n")},
				"contracts/common.md":              {Data: []byte("LAYER: Common")},
				"roles/developer.md":               {Data: []byte("LAYER: Base Developer Role")},
				"modes/review-fix/contract.md":     {Data: []byte("LAYER: Review-Fix Contract")},
				"modes/review-fix/overlays/developer.md": {Data: []byte("LAYER: Review-Fix Developer Overlay")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{
				"LAYER: Common",
				"LAYER: Base Developer Role",
				"LAYER: Review-Fix Contract",
				"LAYER: Review-Fix Developer Overlay",
			},
			description: "Base role should be layered before mode-specific overlay",
		},
		{
			name: "modes/<mode>/overlays/<role>.md is loaded only for the selected role",
			mockFS: fstest.MapFS{
				"manifest.yaml":                    {Data: []byte("version: 1.0.0\ndefaults:\n  mode: review-fix\n  role: developer\n")},
				"contracts/common.md":              {Data: []byte("LAYER: Common")},
				"roles/developer.md":               {Data: []byte("LAYER: Base Developer")},
				"roles/architect.md":               {Data: []byte("LAYER: Base Architect")},
				"modes/review-fix/contract.md":     {Data: []byte("LAYER: Review-Fix Mode")},
				"modes/review-fix/overlays/developer.md": {Data: []byte("LAYER: Developer Overlay")},
				"modes/review-fix/overlays/architect.md": {Data: []byte("LAYER: Architect Overlay")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{
				"LAYER: Common",
				"LAYER: Base Developer",
				"LAYER: Review-Fix Mode",
				"LAYER: Developer Overlay",
			},
			notExpectedInPrompt: []string{
				"LAYER: Architect Overlay",
				"LAYER: Base Architect",
			},
			description: "Only developer overlay loaded, not architect overlay",
		},
		{
			name: "role=coder maps to developer (compat)",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
				"contracts/common.md": {Data: []byte("LAYER: Common")},
				"roles/developer.md":  {Data: []byte("LAYER: Developer Role")},
				"roles/coder.md":      {Data: []byte("LAYER: Coder Role (alias)")},
			},
			cfg: Config{Role: "coder", WorkingDir: "/test"},
			expectedInPrompt: []string{
				"LAYER: Common",
				"LAYER: Developer Role",
			},
			notExpectedInPrompt: []string{
				"LAYER: Coder Role (alias)",
			},
			description: "Coder alias maps to developer role",
		},
		{
			name: "Mode contract missing is tolerated, but role/common missing fails",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: missing-mode\n  role: developer\n")},
				"contracts/common.md": {Data: []byte("LAYER: Common")},
				"roles/developer.md":  {Data: []byte("LAYER: Developer Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{
				"LAYER: Common",
				"LAYER: Developer Role",
			},
			description: "Missing mode contract is optional and tolerated",
		},
		{
			name: "All five layers present when optional files exist",
			mockFS: fstest.MapFS{
				"manifest.yaml":                    {Data: []byte("version: 1.0.0\ndefaults:\n  mode: review-fix\n  role: developer\n")},
				"contracts/common.md":              {Data: []byte("LAYER1: Common")},
				"roles/developer.md":               {Data: []byte("LAYER2: Developer Role")},
				"modes/review-fix/contract.md":     {Data: []byte("LAYER3: Mode Contract")},
				"modes/review-fix/overlay.md":      {Data: []byte("LAYER4: Mode Overlay")},
				"modes/review-fix/overlays/developer.md": {Data: []byte("LAYER5: Role Overlay")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{
				"LAYER1: Common",
				"LAYER2: Developer Role",
				"LAYER3: Mode Contract",
				"LAYER4: Mode Overlay",
				"LAYER5: Role Overlay",
			},
			description: "All five layers loaded in correct order",
		},
		{
			name: "Layering order: common -> role -> mode contract -> mode overlay -> role overlay",
			mockFS: fstest.MapFS{
				"manifest.yaml":                    {Data: []byte("version: 1.0.0\ndefaults:\n  mode: custom\n  role: developer\n")},
				"contracts/common.md":              {Data: []byte("FIRST")},
				"roles/developer.md":               {Data: []byte("SECOND")},
				"modes/custom/contract.md":         {Data: []byte("THIRD")},
				"modes/custom/overlay.md":          {Data: []byte("FOURTH")},
				"modes/custom/overlays/developer.md": {Data: []byte("FIFTH")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{
				"FIRST",
				"SECOND",
				"THIRD",
				"FOURTH",
				"FIFTH",
			},
			description: "Verifies strict layering order per #148 spec",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			compiler := NewCompilerFromFS(tt.mockFS)
			prompt, err := compiler.CompileSystemPrompt(tt.cfg)
			if err != nil {
				t.Fatalf("Unexpected error: %v (description: %s)", err, tt.description)
			}

			for _, expected := range tt.expectedInPrompt {
				if !strings.Contains(prompt, expected) {
					t.Errorf("Expected prompt to contain %q (description: %s).\nPrompt:\n%s", expected, tt.description, prompt)
				}
			}

			for _, notExpected := range tt.notExpectedInPrompt {
				if strings.Contains(prompt, notExpected) {
					t.Errorf("Expected prompt NOT to contain %q (description: %s).\nPrompt:\n%s", notExpected, tt.description, prompt)
				}
			}

			// Verify layer order by checking positions
			if len(tt.expectedInPrompt) > 1 {
				positions := make(map[string]int)
				for _, expected := range tt.expectedInPrompt {
					positions[expected] = strings.Index(prompt, expected)
				}
				for i := 0; i < len(tt.expectedInPrompt)-1; i++ {
					current := tt.expectedInPrompt[i]
					next := tt.expectedInPrompt[i+1]
					if positions[current] > positions[next] {
						t.Errorf("Layer order violation: %q should come before %q (description: %s)", current, next, tt.description)
					}
				}
			}
		})
	}
}

// TestModeOverlayLoading tests mode-specific contract loading
func TestModeOverlayLoading(t *testing.T) {
	tests := []struct {
		name             string
		mockFS           fstest.MapFS
		cfg              Config
		expectedInPrompt []string
		notExpectedInPrompt []string
	}{
		{
			name: "Execute mode loads execute contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
				"contracts/common.md":     {Data: []byte("Common Contract")},
				"modes/execute/contract.md": {Data: []byte("Execute Mode Overlay")},
				"roles/developer.md":      {Data: []byte("Developer Role")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Execute Mode Overlay", "Developer Role"},
		},
		{
			name: "Review-fix mode loads review-fix contract",
			mockFS: fstest.MapFS{
				"manifest.yaml":             {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
				"contracts/common.md":       {Data: []byte("Common Contract")},
				"modes/review-fix/contract.md": {Data: []byte("Review-Fix Mode Overlay")},
				"modes/execute/contract.md":  {Data: []byte("Execute Mode Overlay")},
				"roles/developer.md":        {Data: []byte("Developer Role")},
			},
			cfg:               Config{Mode: "review-fix", WorkingDir: "/test"},
			expectedInPrompt:   []string{"Common Contract", "Review-Fix Mode Overlay", "Developer Role"},
			notExpectedInPrompt: []string{"Execute Mode Overlay"},
		},
		{
			name: "Missing mode contract is handled gracefully",
			mockFS: fstest.MapFS{
				"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
				"contracts/common.md": {Data: []byte("Common Contract")},
				"roles/developer.md":  {Data: []byte("Developer Role")},
			},
			cfg: Config{Mode: "missing-mode", WorkingDir: "/test"},
			expectedInPrompt: []string{"Common Contract", "Developer Role"},
		},
		{
			name: "Mode-specific role overlay adds to base role (does not replace)",
			mockFS: fstest.MapFS{
				"manifest.yaml":                    {Data: []byte("version: 1.0.0\ndefaults:\n  mode: review-fix\n  role: developer\n")},
				"contracts/common.md":              {Data: []byte("Common Contract")},
				"modes/review-fix/contract.md":     {Data: []byte("Review-Fix Mode")},
				"roles/developer.md":               {Data: []byte("Base Developer Role")},
				"modes/review-fix/overlays/developer.md": {Data: []byte("Review-Fix Developer Overlay")},
			},
			cfg: Config{WorkingDir: "/test"},
			expectedInPrompt: []string{
				"Common Contract",
				"Base Developer Role",
				"Review-Fix Mode",
				"Review-Fix Developer Overlay",
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
			"manifest.yaml":           {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
			"contracts/common.md":     {Data: []byte("Common")},
			"modes/execute/contract.md": {Data: []byte("Execute")},
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
		if !strings.Contains(prompt, "Execute") {
			t.Error("Missing execute mode contract")
		}
		if !strings.Contains(prompt, "Developer") {
			t.Error("Missing developer role")
		}
	})

	t.Run("Coder role alias maps to developer", func(t *testing.T) {
		mockFS := fstest.MapFS{
			"manifest.yaml":       {Data: []byte("version: 1.0.0\ndefaults:\n  mode: execute\n  role: developer\n")},
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
