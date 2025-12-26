package image

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

// Package-level compiled regex patterns for performance.
var (
	// Go version patterns
	reGoVersion = regexp.MustCompile(`^\s*go\s+(\d+(?:\.\d+)?)`)

	// Python version patterns
	reRequiresPython = regexp.MustCompile(`^\s*requires-python\s*=\s*["']([^"']+)["']`)
	rePoetryPython   = regexp.MustCompile(`^\s*python\s*=\s*["']([^"']+)["']`)

	// Java pom.xml patterns
	rePomSource   = regexp.MustCompile(`<(?:maven\.compiler\.)?source>(\d+)</(?:maven\.compiler\.)?source>`)
	rePomTarget   = regexp.MustCompile(`<(?:maven\.compiler\.)?target>(\d+)</(?:maven\.compiler\.)?target>`)
	rePomRelease  = regexp.MustCompile(`<(?:maven\.compiler\.)?release>(\d+)</(?:maven\.compiler\.)?release>`)

	// Java Gradle patterns
	reGradleSourceCompat = regexp.MustCompile(`sourceCompatibility\s*=\s*["']?(\d+)["']?`)
	reGradleTargetCompat = regexp.MustCompile(`targetCompatibility\s*=\s*["']?(\d+)["']?`)
	reGradleToolchain    = regexp.MustCompile(`JavaLanguageVersion\.of\((\d+)\)`)
	reGradleJavaVersion  = regexp.MustCompile(`javaVersion\s*=\s*["']?(\d+)["']?`)
)

// versionSource represents where a version was detected.
type versionSource struct {
	Version  string
	File     string
	Field    string
	Line     int
	Original string
}

// versionDetector defines the interface for language-specific version detection.
type versionDetector interface {
	// Detect attempts to detect the language version from project files.
	// Returns the detected version string or empty string if not found.
	Detect(workspace string) *versionSource
}

// Go version detector
type goDetector struct{}

// Detect Go version from go.mod
func (d *goDetector) Detect(workspace string) *versionSource {
	goModPath := filepath.Join(workspace, "go.mod")
	if _, err := os.Stat(goModPath); err != nil {
		return nil
	}

	file, err := os.Open(goModPath)
	if err != nil {
		return nil
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	lineNum := 0

	for scanner.Scan() {
		lineNum++
		line := strings.TrimSpace(scanner.Text())
		if matches := reGoVersion.FindStringSubmatch(line); matches != nil {
			return &versionSource{
				Version:  matches[1],
				File:     "go.mod",
				Field:    "go",
				Line:     lineNum,
				Original: matches[1],
			}
		}
	}

	return nil
}

// Node.js version detector
type nodeDetector struct{}

// Detect Node.js version from multiple sources in priority order
func (d *nodeDetector) Detect(workspace string) *versionSource {
	// 1. Check package.json engines.field
	if src := d.detectFromPackageJSON(workspace); src != nil {
		return src
	}

	// 2. Check .nvmrc
	if src := d.detectFromNvmrc(workspace); src != nil {
		return src
	}

	// 3. Check .node-version
	if src := d.detectFromNodeVersion(workspace); src != nil {
		return src
	}

	return nil
}

func (d *nodeDetector) detectFromPackageJSON(workspace string) *versionSource {
	path := filepath.Join(workspace, "package.json")
	if _, err := os.Stat(path); err != nil {
		return nil
	}

	file, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer file.Close()

	var pkg struct {
		Engines struct {
			Node string `json:"node"`
		} `json:"engines"`
	}

	if err := json.NewDecoder(file).Decode(&pkg); err != nil {
		return nil
	}

	if pkg.Engines.Node != "" {
		normalized := NormalizeVersionRange(pkg.Engines.Node, "22") // 22 is current Node LTS
		if normalized != "" {
			return &versionSource{
				Version:  normalized,
				File:     "package.json",
				Field:    "engines.node",
				Line:     0,
				Original: pkg.Engines.Node,
			}
		}
	}

	return nil
}

func (d *nodeDetector) detectFromNvmrc(workspace string) *versionSource {
	path := filepath.Join(workspace, ".nvmrc")
	if _, err := os.Stat(path); err != nil {
		return nil
	}

	content, err := os.ReadFile(path)
	if err != nil {
		return nil
	}

	version := strings.TrimSpace(string(content))
	if version == "" {
		return nil
	}

	// .nvmrc can contain "node", "lts/*", etc. - handle these
	lower := strings.ToLower(version)
	if lower == "node" || strings.HasPrefix(lower, "lts") || lower == "latest" {
		return &versionSource{
			Version:  "22", // Current Node LTS
			File:     ".nvmrc",
			Field:    "content",
			Line:     1,
			Original: version,
		}
	}

	normalized := NormalizeVersionRange(version, "22")
	if normalized != "" {
		return &versionSource{
			Version:  normalized,
			File:     ".nvmrc",
			Field:    "content",
			Line:     1,
			Original: version,
		}
	}

	return nil
}

func (d *nodeDetector) detectFromNodeVersion(workspace string) *versionSource {
	path := filepath.Join(workspace, ".node-version")
	if _, err := os.Stat(path); err != nil {
		return nil
	}

	content, err := os.ReadFile(path)
	if err != nil {
		return nil
	}

	version := strings.TrimSpace(string(content))
	if version == "" {
		return nil
	}

	normalized := NormalizeVersionRange(version, "22")
	if normalized != "" {
		return &versionSource{
			Version:  normalized,
			File:     ".node-version",
			Field:    "content",
			Line:     1,
			Original: version,
		}
	}

	return nil
}

// Python version detector
type pythonDetector struct{}

// Detect Python version from multiple sources in priority order
func (d *pythonDetector) Detect(workspace string) *versionSource {
	// 1. Check pyproject.toml
	if src := d.detectFromPyproject(workspace); src != nil {
		return src
	}

	// 2. Check .python-version
	if src := d.detectFromPythonVersion(workspace); src != nil {
		return src
	}

	// 3. Check runtime.txt
	if src := d.detectFromRuntimeTxt(workspace); src != nil {
		return src
	}

	return nil
}

func (d *pythonDetector) detectFromPyproject(workspace string) *versionSource {
	path := filepath.Join(workspace, "pyproject.toml")
	if _, err := os.Stat(path); err != nil {
		return nil
	}

	file, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	lineNum := 0

	// Look for requires-python or python version field
	// Poetry: [tool.poetry.dependencies] python = "^3.11"
	// PEP 621: project.requires-python = ">=3.11"

	inPoetryDeps := false
	for scanner.Scan() {
		lineNum++
		line := scanner.Text()

		// Track if we're in [tool.poetry.dependencies] section (or its subtables)
		if strings.Contains(line, "[tool.poetry.dependencies]") {
			inPoetryDeps = true
			continue
		}
		// Exit poetry dependencies section only when encountering a different section
		if inPoetryDeps && strings.HasPrefix(line, "[") && !strings.Contains(line, "[tool.poetry.dependencies") {
			inPoetryDeps = false
		}

		// Try requires-python first
		if matches := reRequiresPython.FindStringSubmatch(line); matches != nil {
			normalized := NormalizeVersionRange(matches[1], "3.13")
			if normalized != "" {
				return &versionSource{
					Version:  normalized,
					File:     "pyproject.toml",
					Field:    "requires-python",
					Line:     lineNum,
					Original: matches[1],
				}
			}
		}

		// Try poetry python field if in dependencies section
		if inPoetryDeps {
			if matches := rePoetryPython.FindStringSubmatch(line); matches != nil {
				normalized := NormalizeVersionRange(matches[1], "3.13")
				if normalized != "" {
					return &versionSource{
						Version:  normalized,
						File:     "pyproject.toml",
						Field:    "tool.poetry.dependencies.python",
						Line:     lineNum,
						Original: matches[1],
					}
				}
			}
		}
	}

	return nil
}

func (d *pythonDetector) detectFromPythonVersion(workspace string) *versionSource {
	path := filepath.Join(workspace, ".python-version")
	if _, err := os.Stat(path); err != nil {
		return nil
	}

	content, err := os.ReadFile(path)
	if err != nil {
		return nil
	}

	version := strings.TrimSpace(string(content))
	if version == "" {
		return nil
	}

	normalized := NormalizeVersionRange(version, "3.13")
	if normalized != "" {
		return &versionSource{
			Version:  normalized,
			File:     ".python-version",
			Field:    "content",
			Line:     1,
			Original: version,
		}
	}

	return nil
}

func (d *pythonDetector) detectFromRuntimeTxt(workspace string) *versionSource {
	path := filepath.Join(workspace, "runtime.txt")
	if _, err := os.Stat(path); err != nil {
		return nil
	}

	content, err := os.ReadFile(path)
	if err != nil {
		return nil
	}

	// Heroku runtime.txt format: "python-3.11.4"
	line := strings.TrimSpace(string(content))
	if strings.HasPrefix(line, "python-") {
		version := strings.TrimPrefix(line, "python-")
		normalized := NormalizeVersionRange(version, "3.13")
		if normalized != "" {
			return &versionSource{
				Version:  normalized,
				File:     "runtime.txt",
				Field:    "content",
				Line:     1,
				Original: line,
			}
		}
	}

	return nil
}

// Java version detector
type javaDetector struct{}

// Detect Java version from multiple sources
func (d *javaDetector) Detect(workspace string) *versionSource {
	// 1. Check pom.xml (Maven)
	pomPath := filepath.Join(workspace, "pom.xml")
	if _, err := os.Stat(pomPath); err == nil {
		return d.detectFromPomXml(pomPath)
	}

	// 2. Check build.gradle or build.gradle.kts (Gradle)
	gradlePath := filepath.Join(workspace, "build.gradle")
	if _, err := os.Stat(gradlePath); err == nil {
		return d.detectFromGradle(gradlePath)
	}

	gradleKtsPath := filepath.Join(workspace, "build.gradle.kts")
	if _, err := os.Stat(gradleKtsPath); err == nil {
		return d.detectFromGradle(gradleKtsPath)
	}

	// 3. Check gradle.properties
	propsPath := filepath.Join(workspace, "gradle.properties")
	if _, err := os.Stat(propsPath); err == nil {
		return d.detectFromGradleProperties(propsPath)
	}

	return nil
}

func (d *javaDetector) detectFromPomXml(path string) *versionSource {
	file, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer file.Close()

	// Simple regex-based parsing for maven-compiler-plugin configuration
	// Looking for: <source>17</source>, <target>17</target>, or <release>17</release>
	// Also supports dotted property names like: <maven.compiler.source>11</maven.compiler.source>
	scanner := bufio.NewScanner(file)
	lineNum := 0

	for scanner.Scan() {
		lineNum++
		line := scanner.Text()

		// Try release first (most specific)
		if matches := rePomRelease.FindStringSubmatch(line); matches != nil {
			return &versionSource{
				Version:  matches[1],
				File:     "pom.xml",
				Field:    "maven-compiler-plugin.release",
				Line:     lineNum,
				Original: matches[1],
			}
		}

		// Try source
		if matches := rePomSource.FindStringSubmatch(line); matches != nil {
			return &versionSource{
				Version:  matches[1],
				File:     "pom.xml",
				Field:    "maven-compiler-plugin.source",
				Line:     lineNum,
				Original: matches[1],
			}
		}

		// Try target
		if matches := rePomTarget.FindStringSubmatch(line); matches != nil {
			return &versionSource{
				Version:  matches[1],
				File:     "pom.xml",
				Field:    "maven-compiler-plugin.target",
				Line:     lineNum,
				Original: matches[1],
			}
		}
	}

	return nil
}

func (d *javaDetector) detectFromGradle(path string) *versionSource {
	file, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	lineNum := 0

	// Gradle can specify Java version in various ways:
	// sourceCompatibility = "11"
	// targetCompatibility = "11"
	// java.toolchain.languageVersion = JavaLanguageVersion.of(17)

	for scanner.Scan() {
		lineNum++
		line := scanner.Text()

		if matches := reGradleToolchain.FindStringSubmatch(line); matches != nil {
			return &versionSource{
				Version:  matches[1],
				File:     filepath.Base(path),
				Field:    "JavaLanguageVersion",
				Line:     lineNum,
				Original: matches[1],
			}
		}

		if matches := reGradleJavaVersion.FindStringSubmatch(line); matches != nil {
			return &versionSource{
				Version:  matches[1],
				File:     filepath.Base(path),
				Field:    "javaVersion",
				Line:     lineNum,
				Original: matches[1],
			}
		}

		if matches := reGradleSourceCompat.FindStringSubmatch(line); matches != nil {
			return &versionSource{
				Version:  matches[1],
				File:     filepath.Base(path),
				Field:    "sourceCompatibility",
				Line:     lineNum,
				Original: matches[1],
			}
		}

		if matches := reGradleTargetCompat.FindStringSubmatch(line); matches != nil {
			return &versionSource{
				Version:  matches[1],
				File:     filepath.Base(path),
				Field:    "targetCompatibility",
				Line:     lineNum,
				Original: matches[1],
			}
		}
	}

	return nil
}

func (d *javaDetector) detectFromGradleProperties(path string) *versionSource {
	file, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	lineNum := 0

	// gradle.properties can have: org.gradle.java.home=/path/to/java17
	// Or custom properties like: javaVersion=17

	for scanner.Scan() {
		lineNum++
		line := strings.TrimSpace(scanner.Text())

		// Skip comments and empty lines
		if line == "" || strings.HasPrefix(line, "#") || strings.HasPrefix(line, "!") {
			continue
		}

		// Look for java version properties
		if strings.Contains(line, "=") {
			parts := strings.SplitN(line, "=", 2)
			if len(parts) == 2 {
				key := strings.TrimSpace(parts[0])
				value := strings.TrimSpace(parts[1])

				// Check for common java version property keys
				if strings.Contains(strings.ToLower(key), "java") &&
					!strings.Contains(strings.ToLower(key), "home") {
					// Extract version number
					re := regexp.MustCompile(`(\d+)`)
					if matches := re.FindStringSubmatch(value); matches != nil {
						return &versionSource{
							Version:  matches[1],
							File:     "gradle.properties",
							Field:    key,
							Line:     lineNum,
							Original: value,
						}
					}
				}
			}
		}
	}

	return nil
}

// detectLanguageVersion attempts to detect the version for a given language.
// Returns the detected version string and the source, or empty string if not detected.
func detectLanguageVersion(workspace string, lang string) *versionSource {
	switch lang {
	case "go":
		return (*goDetector)(nil).Detect(workspace)
	case "node":
		return (*nodeDetector)(nil).Detect(workspace)
	case "python":
		return (*pythonDetector)(nil).Detect(workspace)
	case "java":
		return (*javaDetector)(nil).Detect(workspace)
	default:
		return nil
	}
}

// formatVersionSource formats a versionSource for logging/rationale.
func formatVersionSource(src *versionSource) string {
	if src == nil {
		return ""
	}

	if src.Line > 0 {
		return fmt.Sprintf("%s (%s: %s, line %d: %s)", src.Version, src.File, src.Field, src.Line, src.Original)
	}
	return fmt.Sprintf("%s (%s: %s: %s)", src.Version, src.File, src.Field, src.Original)
}
