package image

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestParseVersion(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		wantMajor int
		wantMinor int
		wantPatch int
		wantErr  bool
	}{
		{
			name:       "simple major.minor",
			input:      "1.22",
			wantMajor:  1,
			wantMinor:  22,
			wantPatch:  0,
			wantErr:    false,
		},
		{
			name:       "with v prefix",
			input:      "v1.22",
			wantMajor:  1,
			wantMinor:  22,
			wantPatch:  0,
			wantErr:    false,
		},
		{
			name:       "full version",
			input:      "1.22.0",
			wantMajor:  1,
			wantMinor:  22,
			wantPatch:  0,
			wantErr:    false,
		},
		{
			name:       "major only",
			input:      "22",
			wantMajor:  22,
			wantMinor:  0,
			wantPatch:  0,
			wantErr:    false,
		},
		{
			name:       "v prefix major only",
			input:      "v22",
			wantMajor:  22,
			wantMinor:  0,
			wantPatch:  0,
			wantErr:    false,
		},
		{
			name:       "empty string",
			input:      "",
			wantErr:    true,
		},
		{
			name:       "invalid version",
			input:      "abc",
			wantErr:    true,
		},
		{
			name:       "too many parts",
			input:      "1.2.3.4",
			wantErr:    true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := ParseVersion(tt.input)
			if (err != nil) != tt.wantErr {
				t.Errorf("ParseVersion() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
			if !tt.wantErr {
				if got.Major != tt.wantMajor {
					t.Errorf("ParseVersion() Major = %v, want %v", got.Major, tt.wantMajor)
				}
				if got.Minor != tt.wantMinor {
					t.Errorf("ParseVersion() Minor = %v, want %v", got.Minor, tt.wantMinor)
				}
				if got.Patch != tt.wantPatch {
					t.Errorf("ParseVersion() Patch = %v, want %v", got.Patch, tt.wantPatch)
				}
			}
		})
	}
}

func TestVersionString(t *testing.T) {
	tests := []struct {
		name  string
		v     Version
		want  string
	}{
		{
			name: "major.minor",
			v:    Version{Major: 1, Minor: 22, Patch: 0},
			want: "1.22",
		},
		{
			name: "major only",
			v:    Version{Major: 22, Minor: 0, Patch: 0},
			want: "22",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.v.String(); got != tt.want {
				t.Errorf("Version.String() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestVersionImageString(t *testing.T) {
	tests := []struct {
		name           string
		v              Version
		useMajorOnly   bool
		want           string
	}{
		{
			name:         "node major only",
			v:            Version{Major: 22, Minor: 0, Patch: 0},
			useMajorOnly: true,
			want:         "22",
		},
		{
			name:         "python major.minor",
			v:            Version{Major: 3, Minor: 12, Patch: 0},
			useMajorOnly: false,
			want:         "3.12",
		},
		{
			name:         "auto major only when minor is 0",
			v:            Version{Major: 3, Minor: 0, Patch: 0},
			useMajorOnly: false,
			want:         "3",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.v.ImageString(tt.useMajorOnly); got != tt.want {
				t.Errorf("Version.ImageString() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestVersionCompare(t *testing.T) {
	tests := []struct {
		name  string
		v     Version
		other Version
		want  int
	}{
		{
			name:  "equal",
			v:     Version{Major: 1, Minor: 22, Patch: 0},
			other: Version{Major: 1, Minor: 22, Patch: 0},
			want:  0,
		},
		{
			name:  "less than major",
			v:     Version{Major: 1, Minor: 22, Patch: 0},
			other: Version{Major: 2, Minor: 0, Patch: 0},
			want:  -1,
		},
		{
			name:  "greater than major",
			v:     Version{Major: 2, Minor: 0, Patch: 0},
			other: Version{Major: 1, Minor: 22, Patch: 0},
			want:  1,
		},
		{
			name:  "less than minor",
			v:     Version{Major: 1, Minor: 21, Patch: 0},
			other: Version{Major: 1, Minor: 22, Patch: 0},
			want:  -1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := tt.v.Compare(&tt.other); got != tt.want {
				t.Errorf("Version.Compare() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestStripVersionRanges(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{"caret", "^1.2.3", "1.2.3"},
		{"tilde", "~1.2.3", "1.2.3"},
		{"greater than or equal", ">=1.2.3", "1.2.3"},
		{"exact", "=1.2.3", "1.2.3"},
		{"v prefix", "v1.2.3", "1.2.3"},
		{"wildcard x", "1.x", "1"},
		{"wildcard star", "1.*", "1"},
		{"x alone", "x", ""},
		{"star alone", "*", ""},
		{"any", "any", ""},
		{"latest", "latest", ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := stripVersionRanges(tt.input); got != tt.want {
				t.Errorf("stripVersionRanges() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNormalizeVersionRange(t *testing.T) {
	tests := []struct {
		name        string
		versionRange string
		ltsVersion  string
		want        string
	}{
		{
			name:        "exact version",
			versionRange: "1.22",
			ltsVersion:  "1.23",
			want:        "1.22",
		},
		{
			name:        "caret range",
			versionRange: "^1.22.0",
			ltsVersion:  "1.23",
			want:        "1.22",
		},
		{
			name:        "tilde range",
			versionRange: "~1.22.0",
			ltsVersion:  "1.23",
			want:        "1.22",
		},
		{
			name:        "greater than or equal",
			versionRange: ">=1.22",
			ltsVersion:  "1.23",
			want:        "1.22",
		},
		{
			name:        "wildcard uses LTS",
			versionRange: "1.x",
			ltsVersion:  "1.23",
			want:        "1.23",
		},
		{
			name:        "star uses LTS",
			versionRange: "*",
			ltsVersion:  "1.23",
			want:        "1.23",
		},
		{
			name:        "any uses LTS",
			versionRange: "any",
			ltsVersion:  "1.23",
			want:        "1.23",
		},
		{
			name:        "v prefix handled",
			versionRange: "v1.22",
			ltsVersion:  "1.23",
			want:        "1.22",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NormalizeVersionRange(tt.versionRange, tt.ltsVersion); got != tt.want {
				t.Errorf("NormalizeVersionRange() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestDetectLanguageVersion_Go(t *testing.T) {
	tests := []struct {
		name           string
		goModContent   string
		wantVersion    string
		wantFile       string
		wantField      string
	}{
		{
			name:         "go 1.22",
			goModContent: "module test\n\ngo 1.22\n",
			wantVersion:  "1.22",
			wantFile:     "go.mod",
			wantField:    "go",
		},
		{
			name:         "go 1.24.0",
			goModContent: "module test\n\ngo 1.24.0\n",
			wantVersion:  "1.24",
			wantFile:     "go.mod",
			wantField:    "go",
		},
		{
			name:         "no version",
			goModContent: "module test\n",
			wantVersion:  "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			goModPath := filepath.Join(dir, "go.mod")
			if err := os.WriteFile(goModPath, []byte(tt.goModContent), 0644); err != nil {
				t.Fatal(err)
			}

			detector := (*goDetector)(nil)
			src := detector.Detect(dir)

			if tt.wantVersion == "" {
				if src != nil {
					t.Errorf("Expected no version, got %v", src)
				}
				return
			}

			if src == nil {
				t.Fatalf("Expected version %s, got nil", tt.wantVersion)
			}

			if src.Version != tt.wantVersion {
				t.Errorf("Version = %s, want %s", src.Version, tt.wantVersion)
			}
			if src.File != tt.wantFile {
				t.Errorf("File = %s, want %s", src.File, tt.wantFile)
			}
			if src.Field != tt.wantField {
				t.Errorf("Field = %s, want %s", src.Field, tt.wantField)
			}
		})
	}
}

func TestDetectLanguageVersion_Node(t *testing.T) {
	tests := []struct {
		name           string
		files          map[string]string
		wantVersion    string
		wantFile       string
	}{
		{
			name: "package.json engines.node",
			files: map[string]string{
				"package.json": `{"name": "test", "engines": {"node": ">=18.0.0"}}`,
			},
			wantVersion: "18",
			wantFile:    "package.json",
		},
		{
			name: ".nvmrc with version",
			files: map[string]string{
				".nvmrc": "20\n",
			},
			wantVersion: "20",
			wantFile:    ".nvmrc",
		},
		{
			name: ".nvmrc with lts",
			files: map[string]string{
				".nvmrc": "lts/*\n",
			},
			wantVersion: "22",
			wantFile:    ".nvmrc",
		},
		{
			name: ".node-version",
			files: map[string]string{
				".node-version": "18\n",
			},
			wantVersion: "18",
			wantFile:    ".node-version",
		},
		{
			name:        "no version files",
			files:       map[string]string{},
			wantVersion: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			for filename, content := range tt.files {
				path := filepath.Join(dir, filename)
				if err := os.WriteFile(path, []byte(content), 0644); err != nil {
					t.Fatal(err)
				}
			}

			detector := (*nodeDetector)(nil)
			src := detector.Detect(dir)

			if tt.wantVersion == "" {
				if src != nil {
					t.Errorf("Expected no version, got %v", src)
				}
				return
			}

			if src == nil {
				t.Fatalf("Expected version %s, got nil", tt.wantVersion)
			}

			if src.Version != tt.wantVersion {
				t.Errorf("Version = %s, want %s", src.Version, tt.wantVersion)
			}
			if src.File != tt.wantFile {
				t.Errorf("File = %s, want %s", src.File, tt.wantFile)
			}
		})
	}
}

func TestDetectLanguageVersion_Python(t *testing.T) {
	tests := []struct {
		name        string
		files       map[string]string
		wantVersion string
		wantFile    string
	}{
		{
			name: "pyproject.toml requires-python",
			files: map[string]string{
				"pyproject.toml": "[project]\nrequires-python = \">=3.11\"\n",
			},
			wantVersion: "3.11",
			wantFile:    "pyproject.toml",
		},
		{
			name: "pyproject.toml poetry python",
			files: map[string]string{
				"pyproject.toml": "[tool.poetry.dependencies]\npython = \"^3.11\"\n",
			},
			wantVersion: "3.11",
			wantFile:    "pyproject.toml",
		},
		{
			name: ".python-version",
			files: map[string]string{
				".python-version": "3.12\n",
			},
			wantVersion: "3.12",
			wantFile:    ".python-version",
		},
		{
			name: "runtime.txt",
			files: map[string]string{
				"runtime.txt": "python-3.11.4\n",
			},
			wantVersion: "3.11",
			wantFile:    "runtime.txt",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			for filename, content := range tt.files {
				path := filepath.Join(dir, filename)
				if err := os.WriteFile(path, []byte(content), 0644); err != nil {
					t.Fatal(err)
				}
			}

			detector := (*pythonDetector)(nil)
			src := detector.Detect(dir)

			if tt.wantVersion == "" {
				if src != nil {
					t.Errorf("Expected no version, got %v", src)
				}
				return
			}

			if src == nil {
				t.Fatalf("Expected version %s, got nil", tt.wantVersion)
			}

			if src.Version != tt.wantVersion {
				t.Errorf("Version = %s, want %s", src.Version, tt.wantVersion)
			}
		})
	}
}

func TestDetectLanguageVersion_Java(t *testing.T) {
	tests := []struct {
		name        string
		files       map[string]string
		wantVersion string
		wantFile    string
	}{
		{
			name: "pom.xml with release",
			files: map[string]string{
				"pom.xml": `<project>
					<build>
						<plugins>
							<plugin>
								<groupId>org.apache.maven.plugins</groupId>
								<artifactId>maven-compiler-plugin</artifactId>
								<configuration>
									<release>17</release>
								</configuration>
							</plugin>
						</plugins>
					</build>
				</project>`,
			},
			wantVersion: "17",
			wantFile:    "pom.xml",
		},
		{
			name: "pom.xml with source",
			files: map[string]string{
				"pom.xml": `<project>
					<properties>
						<maven.compiler.source>11</maven.compiler.source>
					</properties>
				</project>`,
			},
			wantVersion: "11",
			wantFile:    "pom.xml",
		},
		{
			name: "build.gradle sourceCompatibility",
			files: map[string]string{
				"build.gradle": "plugins {}\nsourceCompatibility = '17'\n",
			},
			wantVersion: "17",
			wantFile:    "build.gradle",
		},
		{
			name: "build.gradle.kts toolchain",
			files: map[string]string{
				"build.gradle.kts": "java {\n    toolchain {\n        languageVersion.set(JavaLanguageVersion.of(21))\n    }\n}\n",
			},
			wantVersion: "21",
			wantFile:    "build.gradle.kts",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			for filename, content := range tt.files {
				path := filepath.Join(dir, filename)
				if err := os.WriteFile(path, []byte(content), 0644); err != nil {
					t.Fatal(err)
				}
			}

			detector := (*javaDetector)(nil)
			src := detector.Detect(dir)

			if tt.wantVersion == "" {
				if src != nil {
					t.Errorf("Expected no version, got %v", src)
				}
				return
			}

			if src == nil {
				t.Fatalf("Expected version %s, got nil", tt.wantVersion)
			}

			if src.Version != tt.wantVersion {
				t.Errorf("Version = %s, want %s", src.Version, tt.wantVersion)
			}
		})
	}
}

// Integration tests for the full Detect() function with version detection
func TestDetect_WithVersionDetection(t *testing.T) {
	tests := []struct {
		name           string
		files          map[string]string
		wantImage      string
		shouldHaveVersion bool
	}{
		{
			name: "go.mod with version 1.22",
			files: map[string]string{
				"go.mod": "module test\n\ngo 1.22\n",
			},
			wantImage:      "golang:1.22",
			shouldHaveVersion: true,
		},
		{
			name: "package.json with engines",
			files: map[string]string{
				"package.json": `{"name": "test", "engines": {"node": "20"}}`,
			},
			wantImage:      "node:20",
			shouldHaveVersion: true,
		},
		{
			name: ".nvmrc",
			files: map[string]string{
				".nvmrc": "18\n",
			},
			wantImage:      "",
			shouldHaveVersion: false, // .nvmrc is hidden, won't be detected
		},
		{
			name: "pyproject.toml with version",
			files: map[string]string{
				"pyproject.toml": "[project]\nrequires-python = \">=3.11\"\n",
			},
			wantImage:      "python:3.11",
			shouldHaveVersion: true,
		},
		{
			name: "go.mod without version",
			files: map[string]string{
				"go.mod": "module test\n",
			},
			wantImage:      "golang:1.23", // static default
			shouldHaveVersion: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			for filename, content := range tt.files {
				path := filepath.Join(dir, filename)
				if err := os.WriteFile(path, []byte(content), 0644); err != nil {
					t.Fatal(err)
				}
			}

			result := Detect(dir)

			if tt.wantImage != "" && result.Image != tt.wantImage {
				t.Errorf("Detect() Image = %s, want %s", result.Image, tt.wantImage)
			}

			hasVersionInRationale := tt.shouldHaveVersion &&
				strings.Contains(result.Rationale, "version:")
			if tt.shouldHaveVersion && !hasVersionInRationale {
				t.Errorf("Detect() Rationale should contain version info, got: %s", result.Rationale)
			}
		})
	}
}

func TestBuildVersionedImage(t *testing.T) {
	tests := []struct {
		name     string
		baseImage string
		lang     string
		version  string
		want     string
	}{
		{
			name:     "go major.minor",
			baseImage: "golang:1.23",
			lang:     "go",
			version:  "1.22",
			want:     "golang:1.22",
		},
		{
			name:     "node major only",
			baseImage: "node:22",
			lang:     "node",
			version:  "20",
			want:     "node:20",
		},
		{
			name:     "python major.minor",
			baseImage: "python:3.13",
			lang:     "python",
			version:  "3.11",
			want:     "python:3.11",
		},
		{
			name:     "java major only",
			baseImage: "eclipse-temurin:21-jdk",
			lang:     "java",
			version:  "17",
			want:     "eclipse-temurin:17-jdk",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := buildVersionedImage(tt.baseImage, tt.lang, tt.version); got != tt.want {
				t.Errorf("buildVersionedImage() = %v, want %v", got, tt.want)
			}
		})
	}
}
