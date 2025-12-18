package tools

import (
	"path/filepath"
	"testing"
)

func TestFileTools(t *testing.T) {
	tmpDir := t.TempDir()
	testFile := filepath.Join(tmpDir, "test.txt")
	content := "hello world"

	// Test WriteFile
	err := WriteFile(testFile, content)
	if err != nil {
		t.Fatalf("WriteFile failed: %v", err)
	}

	// Test ReadFile
	readContent, err := ReadFile(testFile)
	if err != nil {
		t.Fatalf("ReadFile failed: %v", err)
	}
	if readContent != content {
		t.Errorf("Expected content %q, got %q", content, readContent)
	}

	// Test ListDir
	list, err := ListDir(tmpDir)
	if err != nil {
		t.Fatalf("ListDir failed: %v", err)
	}
	if list == "" {
		t.Error("Expected non-empty list for non-empty directory")
	}
}

func TestExecuteCommand(t *testing.T) {
	out, err := ExecuteCommand("echo 'holon'")
	if err != nil {
		t.Fatalf("ExecuteCommand failed: %v", err)
	}
	if out != "holon\n" {
		t.Errorf("Expected 'holon\n', got %q", out)
	}
}
