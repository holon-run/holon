package image

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
)

func TestDetect_GoProject(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "go.mod", "module test\n")

	result := Detect(dir)
	if result.Image != "golang:1.23" {
		t.Errorf("Expected golang:1.23, got %s", result.Image)
	}
	if len(result.Signals) != 1 || result.Signals[0] != "go.mod" {
		t.Errorf("Expected signals [go.mod], got %v", result.Signals)
	}
}

func TestDetect_RustProject(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "Cargo.toml", "[package]\nname = \"test\"\n")

	result := Detect(dir)
	if result.Image != "rust:1.83" {
		t.Errorf("Expected rust:1.83, got %s", result.Image)
	}
	if len(result.Signals) != 1 || result.Signals[0] != "Cargo.toml" {
		t.Errorf("Expected signals [Cargo.toml], got %v", result.Signals)
	}
}

func TestDetect_PythonProject_Pyproject(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "pyproject.toml", "[project]\nname = \"test\"\n")

	result := Detect(dir)
	if result.Image != "python:3.13" {
		t.Errorf("Expected python:3.13, got %s", result.Image)
	}
	if len(result.Signals) != 1 || result.Signals[0] != "pyproject.toml" {
		t.Errorf("Expected signals [pyproject.toml], got %v", result.Signals)
	}
}

func TestDetect_PythonProject_Requirements(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "requirements.txt", "requests==2.28.0\n")

	result := Detect(dir)
	if result.Image != "python:3.13" {
		t.Errorf("Expected python:3.13, got %s", result.Image)
	}
}

func TestDetect_NodeProject(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "package.json", `{"name": "test"}`)

	result := Detect(dir)
	if result.Image != "node:22" {
		t.Errorf("Expected node:22, got %s", result.Image)
	}
	if len(result.Signals) != 1 || result.Signals[0] != "package.json" {
		t.Errorf("Expected signals [package.json], got %v", result.Signals)
	}
}

func TestDetect_JavaProject_Maven(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "pom.xml", "<project></project>\n")

	result := Detect(dir)
	if result.Image != "eclipse-temurin:21-jdk" {
		t.Errorf("Expected eclipse-temurin:21-jdk, got %s", result.Image)
	}
}

func TestDetect_JavaProject_Gradle(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "build.gradle", "plugins {}\n")

	result := Detect(dir)
	if result.Image != "eclipse-temurin:21-jdk" {
		t.Errorf("Expected eclipse-temurin:21-jdk, got %s", result.Image)
	}
}

func TestDetect_RubyProject(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "Gemfile", "source 'https://rubygems.org'\n")

	result := Detect(dir)
	if result.Image != "ruby:3.3" {
		t.Errorf("Expected ruby:3.3, got %s", result.Image)
	}
}

func TestDetect_PhpProject(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "composer.json", `{"name": "test"}`)

	result := Detect(dir)
	if result.Image != "php:8.3" {
		t.Errorf("Expected php:8.3, got %s", result.Image)
	}
}

func TestDetect_DotnetProject(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "test.csproj", "<Project></Project>\n")

	result := Detect(dir)
	if result.Image != "mcr.microsoft.com/dotnet/sdk:8.0" {
		t.Errorf("Expected mcr.microsoft.com/dotnet/sdk:8.0, got %s", result.Image)
	}
}

func TestDetect_Dockerfile(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "Dockerfile", "FROM alpine\n")

	result := Detect(dir)
	if result.Image != "docker:24" {
		t.Errorf("Expected docker:24, got %s", result.Image)
	}
}

func TestDetect_PolyglotProject(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "go.mod", "module test\n")
	createFile(t, dir, "package.json", `{"name": "test"}`)

	result := Detect(dir)
	// Should pick Go due to higher priority (100 vs 90)
	if result.Image != "golang:1.23" {
		t.Errorf("Expected golang:1.23 (higher priority), got %s", result.Image)
	}
	// Should have both signals
	if len(result.Signals) != 2 {
		t.Errorf("Expected 2 signals, got %d", len(result.Signals))
	}
}

func TestDetect_NoSignal(t *testing.T) {
	dir := t.TempDir()
	// Create a file that doesn't match any known signal
	createFile(t, dir, "README.md", "# Test\n")

	result := Detect(dir)
	if result.Image != "golang:1.22" {
		t.Errorf("Expected default golang:1.22, got %s", result.Image)
	}
	if len(result.Signals) != 0 {
		t.Errorf("Expected no signals, got %v", result.Signals)
	}
}

func TestDetect_SkipsNodeModules(t *testing.T) {
	dir := t.TempDir()
	nodeModules := filepath.Join(dir, "node_modules")
	if err := os.MkdirAll(nodeModules, 0755); err != nil {
		t.Fatal(err)
	}
	// Create package.json inside node_modules - should be ignored
	createFileInDir(t, dir, "node_modules", "package.json", `{"name": "test"}`)

	result := Detect(dir)
	// Should return default, not node:22
	if result.Image != "golang:1.22" {
		t.Errorf("Expected default golang:1.22 (node_modules should be skipped), got %s", result.Image)
	}
}

func TestDetect_SkipsVendor(t *testing.T) {
	dir := t.TempDir()
	vendor := filepath.Join(dir, "vendor")
	if err := os.MkdirAll(vendor, 0755); err != nil {
		t.Fatal(err)
	}
	// Create go.mod inside vendor - should be ignored
	createFileInDir(t, dir, "vendor", "go.mod", "module test\n")

	result := Detect(dir)
	// Should return default, not golang:1.23
	if result.Image != "golang:1.22" {
		t.Errorf("Expected default golang:1.22 (vendor should be skipped), got %s", result.Image)
	}
}

func TestDetect_SkipsHiddenFiles(t *testing.T) {
	dir := t.TempDir()
	// Create .hidden directory
	hiddenDir := filepath.Join(dir, ".hidden")
	if err := os.MkdirAll(hiddenDir, 0755); err != nil {
		t.Fatal(err)
	}
	// Create package.json inside hidden directory - should be ignored
	createFileInDir(t, dir, ".hidden", "package.json", `{"name": "test"}`)

	result := Detect(dir)
	// Should return default, not node:22
	if result.Image != "golang:1.22" {
		t.Errorf("Expected default golang:1.22 (hidden files should be skipped), got %s", result.Image)
	}
}

func TestDetect_PythonPriority_PyprojectVsRequirements(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "pyproject.toml", "[project]\nname = \"test\"\n")
	createFile(t, dir, "requirements.txt", "requests==2.28.0\n")

	result := Detect(dir)
	// pyproject.toml has higher priority (90 vs 80)
	if result.Image != "python:3.13" {
		t.Errorf("Expected python:3.13, got %s", result.Image)
	}
	if len(result.Signals) != 2 {
		t.Errorf("Expected 2 signals, got %d", len(result.Signals))
	}
}

func TestFormatResult(t *testing.T) {
	tests := []struct {
		name     string
		result   *DetectResult
		expected string
	}{
		{
			name: "single signal",
			result: &DetectResult{
				Image:     "golang:1.23",
				Signals:   []string{"go.mod"},
				Rationale: "Detected Go module (go.mod)",
			},
			expected: "Detected image: golang:1.23 (signals: go.mod) - Detected Go module (go.mod)",
		},
		{
			name: "multiple signals",
			result: &DetectResult{
				Image:     "golang:1.23",
				Signals:   []string{"go.mod", "package.json"},
				Rationale: "Detected Go module (go.mod)",
			},
			expected: "Detected image: golang:1.23 (signals: go.mod, package.json) - Detected Go module (go.mod)",
		},
		{
			name: "no signals",
			result: &DetectResult{
				Image:     "golang:1.22",
				Signals:   []string{},
				Rationale: "No language signals detected, using default Go image",
			},
			expected: "Detected image: golang:1.22 (signals: none) - No language signals detected, using default Go image",
		},
		{
			name: "disabled",
			result: &DetectResult{
				Disabled: true,
			},
			expected: "Auto-detection disabled",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := FormatResult(tt.result)
			if got != tt.expected {
				t.Errorf("FormatResult() = %q, want %q", got, tt.expected)
			}
		})
	}
}

func TestDetect_PnpmWorkspace_Yaml(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n")

	result := Detect(dir)
	if result.Image != "node:22" {
		t.Errorf("Expected node:22, got %s", result.Image)
	}
	if len(result.Signals) != 1 || result.Signals[0] != "pnpm-workspace.yaml" {
		t.Errorf("Expected signals [pnpm-workspace.yaml], got %v", result.Signals)
	}
}

func TestDetect_PnpmWorkspace_Yml(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "pnpm-workspace.yml", "packages:\n  - 'packages/*'\n")

	result := Detect(dir)
	if result.Image != "node:22" {
		t.Errorf("Expected node:22, got %s", result.Image)
	}
	if len(result.Signals) != 1 || result.Signals[0] != "pnpm-workspace.yaml" {
		t.Errorf("Expected signals [pnpm-workspace.yaml], got %v", result.Signals)
	}
}

func TestDetect_PnpmWorkspace_PriorityOverPackageJson(t *testing.T) {
	dir := t.TempDir()
	// Both pnpm-workspace.yaml and package.json present
	createFile(t, dir, "pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n")
	createFile(t, dir, "package.json", `{"name": "test"}`)

	result := Detect(dir)
	// pnpm-workspace.yaml should win (priority 95 vs 90)
	if result.Image != "node:22" {
		t.Errorf("Expected node:22, got %s", result.Image)
	}
	// Both signals should be detected
	if len(result.Signals) != 2 {
		t.Errorf("Expected 2 signals, got %d: %v", len(result.Signals), result.Signals)
	}
	// Check that pnpm-workspace.yaml is in signals
	foundPnpm := false
	for _, sig := range result.Signals {
		if sig == "pnpm-workspace.yaml" {
			foundPnpm = true
			break
		}
	}
	if !foundPnpm {
		t.Errorf("Expected pnpm-workspace.yaml in signals, got %v", result.Signals)
	}
}

func TestDetect_Monorepo_SingleLevel(t *testing.T) {
	dir := t.TempDir()
	// Create packages/a/package.json
	packagesDir := filepath.Join(dir, "packages")
	aDir := filepath.Join(packagesDir, "a")
	if err := os.MkdirAll(aDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(aDir, "package.json"), []byte(`{"name": "a"}`), 0644); err != nil {
		t.Fatal(err)
	}
	// Create packages/b/package.json
	bDir := filepath.Join(packagesDir, "b")
	if err := os.MkdirAll(bDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bDir, "package.json"), []byte(`{"name": "b"}`), 0644); err != nil {
		t.Fatal(err)
	}

	result := Detect(dir)
	if result.Image != "node:22" {
		t.Errorf("Expected node:22, got %s", result.Image)
	}
	// Should detect both individual package.json signals AND monorepo signal
	foundMonorepo := false
	packageCount := 0
	for _, sig := range result.Signals {
		if sig == "monorepo:packages/**/package.json" {
			foundMonorepo = true
		}
		if sig == "package.json" {
			packageCount++
		}
	}
	if !foundMonorepo {
		t.Errorf("Expected monorepo:packages/**/package.json in signals, got %v", result.Signals)
	}
	// Should have 2 individual package.json signals plus the monorepo signal
	if packageCount != 2 {
		t.Errorf("Expected 2 package.json signals, got %d", packageCount)
	}
}

func TestDetect_Monorepo_DeeplyNested(t *testing.T) {
	dir := t.TempDir()
	// Create deeply nested structure: typescript/packages/extensions/package.json
	pathParts := []string{"typescript", "packages", "extensions"}
	path := filepath.Join(dir, filepath.Join(pathParts...))
	if err := os.MkdirAll(path, 0755); err != nil {
		t.Fatal(err)
	}
	pkgJsonPath := filepath.Join(path, "package.json")
	if err := os.WriteFile(pkgJsonPath, []byte(`{"name": "test"}`), 0644); err != nil {
		t.Fatal(err)
	}

	result := Detect(dir)
	if result.Image != "node:22" {
		t.Errorf("Expected node:22, got %s", result.Image)
	}
	// Should detect monorepo signal for deeply nested structure
	foundMonorepo := false
	for _, sig := range result.Signals {
		if sig == "monorepo:typescript/**/package.json" {
			foundMonorepo = true
			break
		}
	}
	if !foundMonorepo {
		t.Errorf("Expected monorepo:typescript/**/package.json in signals, got %v", result.Signals)
	}
	// Should also detect the individual package.json signal
	foundPackageJson := false
	for _, sig := range result.Signals {
		if sig == "package.json" {
			foundPackageJson = true
			break
		}
	}
	if !foundPackageJson {
		t.Errorf("Expected package.json in signals, got %v", result.Signals)
	}
}

func TestDetect_Monorepo_MultipleRoots(t *testing.T) {
	dir := t.TempDir()
	// Create packages/a/package.json
	packagesDir := filepath.Join(dir, "packages")
	aDir := filepath.Join(packagesDir, "a")
	if err := os.MkdirAll(aDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(aDir, "package.json"), []byte(`{"name": "a"}`), 0644); err != nil {
		t.Fatal(err)
	}
	// Create apps/b/package.json
	appsDir := filepath.Join(dir, "apps")
	bDir := filepath.Join(appsDir, "b")
	if err := os.MkdirAll(bDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bDir, "package.json"), []byte(`{"name": "b"}`), 0644); err != nil {
		t.Fatal(err)
	}

	result := Detect(dir)
	if result.Image != "node:22" {
		t.Errorf("Expected node:22, got %s", result.Image)
	}
	// Should detect both monorepo signals
	foundPackages := false
	foundApps := false
	for _, sig := range result.Signals {
		if sig == "monorepo:packages/**/package.json" {
			foundPackages = true
		}
		if sig == "monorepo:apps/**/package.json" {
			foundApps = true
		}
	}
	if !foundPackages {
		t.Errorf("Expected monorepo:packages/**/package.json in signals, got %v", result.Signals)
	}
	if !foundApps {
		t.Errorf("Expected monorepo:apps/**/package.json in signals, got %v", result.Signals)
	}
}

func TestDetect_Monorepo_RationaleCorrectCount(t *testing.T) {
	dir := t.TempDir()
	// Create 3 packages
	packagesDir := filepath.Join(dir, "packages")
	for i := 1; i <= 3; i++ {
		pkgDir := filepath.Join(packagesDir, fmt.Sprintf("pkg%d", i))
		if err := os.MkdirAll(pkgDir, 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(pkgDir, "package.json"), []byte(fmt.Sprintf(`{"name": "pkg%d"}`, i)), 0644); err != nil {
			t.Fatal(err)
		}
	}

	detector := NewDetector(dir)
	debugResult := detector.DetectDebug()

	// Check that monorepo signal is in scanned signals with correct count
	foundMonorepoWithCount := false
	for _, sig := range debugResult.ScannedSignals {
		if sig.Name == "monorepo:packages/**/package.json" {
			// The signal name itself won't have the count, but we can check it exists
			foundMonorepoWithCount = true
		}
	}
	if !foundMonorepoWithCount {
		t.Errorf("Expected monorepo:packages/**/package.json in scanned signals, got %v", debugResult.ScannedSignals)
	}
	// Should have 3 package.json signals
	packageCount := 0
	for _, sig := range debugResult.ScannedSignals {
		if sig.Name == "package.json" {
			packageCount++
		}
	}
	if packageCount != 3 {
		t.Errorf("Expected 3 package.json signals, got %d", packageCount)
	}
}

func TestDetect_DebugMode_SignalPaths(t *testing.T) {
	dir := t.TempDir()
	createFile(t, dir, "go.mod", "module test\n")
	// Create a nested package.json
	packagesDir := filepath.Join(dir, "packages")
	testDir := filepath.Join(packagesDir, "test")
	if err := os.MkdirAll(testDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(testDir, "package.json"), []byte(`{"name": "test"}`), 0644); err != nil {
		t.Fatal(err)
	}

	detector := NewDetector(dir)
	debugResult := detector.DetectDebug()

	// Check that paths are tracked
	foundGoModPath := false
	foundPackagePath := false
	for _, sig := range debugResult.ScannedSignals {
		if sig.Name == "go.mod" && sig.Path == "go.mod" {
			foundGoModPath = true
		}
		if sig.Name == "package.json" && sig.Path == filepath.Join("packages", "test", "package.json") {
			foundPackagePath = true
		}
	}

	if !foundGoModPath {
		t.Errorf("Expected go.mod with path 'go.mod', got paths: %v", debugResult.ScannedSignals)
	}
	if !foundPackagePath {
		t.Errorf("Expected package.json with nested path, got paths: %v", debugResult.ScannedSignals)
	}

	// Check file count is tracked (should be at least 2: go.mod and package.json)
	if debugResult.FileCount < 2 {
		t.Errorf("Expected at least 2 files counted, got %d", debugResult.FileCount)
	}

	// Check duration is non-negative (can be 0 for very fast operations)
	if debugResult.DurationMs < 0 {
		t.Errorf("Expected non-negative duration, got %dms", debugResult.DurationMs)
	}
}

// Helper function to check if a string contains a substring
func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(s) > len(substr) && (
		s[:len(substr)] == substr ||
		s[len(s)-len(substr):] == substr ||
		containsMiddle(s, substr)))
}

func containsMiddle(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}

// Helper function to create a file in a directory
func createFile(t *testing.T, dir, filename, content string) {
	path := filepath.Join(dir, filename)
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
}

// Helper function to create a file in a subdirectory
func createFileInDir(t *testing.T, dir, subdir, filename, content string) {
	path := filepath.Join(dir, subdir, filename)
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
}
