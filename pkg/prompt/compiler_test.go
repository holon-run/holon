package prompt

import (
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
