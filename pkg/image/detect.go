// Package image provides auto-detection of Docker base images
// based on workspace language/framework signals.
package image

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// DefaultImage is the fallback Docker image used when no language signal is detected.
const DefaultImage = "golang:1.22"

// Detector detects the appropriate Docker base image for a workspace.
type Detector struct {
	workspace string
}

// NewDetector creates a new Detector for the given workspace path.
func NewDetector(workspace string) *Detector {
	return &Detector{workspace: workspace}
}

// DetectResult contains the detected image and detection rationale.
type DetectResult struct {
	Image     string   // Detected Docker image (e.g., image.DefaultImage)
	Signals   []string // List of signals that contributed to this detection
	Rationale string   // Human-readable explanation of the detection
	Disabled  bool     // True if auto-detection is disabled
}

// DebugDetectResult contains detailed detection information for debugging.
type DebugDetectResult struct {
	*DetectResult

	// ScannedSignals lists all signals found during scanning
	ScannedSignals []SignalMatch `json:"scanned_signals"`

	// VersionInfo contains version detection details
	VersionInfo *VersionDebugInfo `json:"version_info,omitempty"`

	// FileCount is the total number of files scanned
	FileCount int `json:"file_count"`

	// DurationMs is how long the detection took
	DurationMs int64 `json:"duration_ms"`
}

// SignalMatch represents a found signal with location info.
type SignalMatch struct {
	Name     string `json:"name"`
	Priority int    `json:"priority"`
	Path     string `json:"path"`
	Found    bool   `json:"found"`
}

// VersionDebugInfo contains version detection details for debugging.
type VersionDebugInfo struct {
	Language    string `json:"language"`
	Version     string `json:"version"`
	SourceFile  string `json:"source_file"`
	SourceField string `json:"source_field"`
	LineNumber  int    `json:"line_number"`
	RawValue    string `json:"raw_value"`
}

// Detect analyzes the workspace to determine an appropriate base image.
// If no strong signal is detected, returns a safe default.
// Attempts to detect language versions from project files.
func (d *Detector) Detect() *DetectResult {
	signals := d.collectSignals(false)

	if len(signals) == 0 {
		return &DetectResult{
			Image:     DefaultImage,
			Signals:   []string{},
			Rationale: "No language signals detected, using default Go image",
		}
	}

	// Score each signal by priority
	bestSignal := d.scoreSignals(signals)

	// Detect language version and update image accordingly
	image, rationale := d.detectVersion(bestSignal)

	return &DetectResult{
		Image:     image,
		Signals:   signalNames(signals),
		Rationale: rationale,
	}
}

// DetectDebug performs detection with detailed debug information.
func (d *Detector) DetectDebug() *DebugDetectResult {
	start := time.Now()

	// Collect signals with path tracking
	signals := d.collectSignals(true)

	// Count files
	fileCount := 0
	_ = filepath.Walk(d.workspace, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info == nil {
			return nil
		}
		if !info.IsDir() {
			fileCount++
		}
		return nil
	})

	var result *DetectResult
	var versionInfo *VersionDebugInfo

	if len(signals) == 0 {
		result = &DetectResult{
			Image:     DefaultImage,
			Signals:   []string{},
			Rationale: "No language signals detected, using default Go image",
		}
	} else {
		bestSignal := d.scoreSignals(signals)
		image, rationale, vInfo := d.detectVersionDebug(bestSignal)
		versionInfo = vInfo

		result = &DetectResult{
			Image:     image,
			Signals:   signalNames(signals),
			Rationale: rationale,
		}
	}

	return &DebugDetectResult{
		DetectResult:   result,
		ScannedSignals: signalsToMatches(signals),
		VersionInfo:    versionInfo,
		FileCount:      fileCount,
		DurationMs:     time.Since(start).Milliseconds(),
	}
}

// signalsToMatches converts signals to SignalMatch structs
func signalsToMatches(signals []signal) []SignalMatch {
	matches := make([]SignalMatch, len(signals))
	for i, sig := range signals {
		matches[i] = SignalMatch{
			Name:     sig.Name,
			Priority: sig.Priority,
			Path:     sig.path,
			Found:    true,
		}
	}
	return matches
}

// signal is a detected language/framework indicator in the workspace.
type signal struct {
	Name      string   // Name of the signal (e.g., "go.mod")
	Priority  int      // Higher = more specific/stronger signal
	Image     string   // Associated Docker image (base template)
	Rationale string   // Explanation for this choice
	match     func(string, string, string) bool
	lang      string   // Language for version detection (e.g., "go", "node")
	path      string   // Relative path where signal was found (for debug output)
}

// collectSignals scans the workspace for language/framework signals.
// If trackPaths is true, stores the path where each signal was found.
func (d *Detector) collectSignals(trackPaths bool) []signal {
	var signals []signal

	// Walk the workspace directory looking for known files
	_ = filepath.Walk(d.workspace, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return nil // Skip errors
		}

		// Skip directories and hidden files
		if info.IsDir() {
			// Don't recurse into common directories we don't care about
			if shouldSkipDirectory(path) {
				return filepath.SkipDir
			}
			return nil
		}

		// Skip hidden files
		if strings.HasPrefix(info.Name(), ".") {
			return nil
		}

		// Get relative path from workspace
		relPath, err := filepath.Rel(d.workspace, path)
		if err != nil {
			return nil
		}

		// Check if this file matches a known signal
		if sig := matchSignal(relPath, info.Name()); sig != nil {
			if trackPaths {
				sig.path = relPath
			}
			signals = append(signals, *sig)
		}

		return nil
	})

	// Add monorepo-specific signals (check for common monorepo patterns)
	monorepoSignals := d.checkMonorepoSignals()
	signals = append(signals, monorepoSignals...)

	return signals
}

// checkMonorepoSignals checks for monorepo structure patterns.
// Returns signals for common monorepo directory layouts.
func (d *Detector) checkMonorepoSignals() []signal {
	// Define monorepo roots and their priorities. We will detect any
	// package.json files recursively under these roots.
	monorepoPatterns := []struct {
		root     string
		pattern  string
		priority int
	}{
		{"packages", "packages/**/package.json", 85},
		{"apps", "apps/**/package.json", 85},
		{"typescript", "typescript/**/package.json", 85},
		{"workspaces", "workspaces/**/package.json", 85},
	}

	// Count matching package.json files per monorepo root.
	counts := make(map[string]int)

	_ = filepath.Walk(d.workspace, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return nil // Skip errors
		}

		if info == nil {
			return nil
		}

		if info.IsDir() {
			// Reuse the same directory-skipping logic as in collectSignals.
			if shouldSkipDirectory(path) {
				return filepath.SkipDir
			}
			return nil
		}

		// Only consider package.json files.
		if info.Name() != "package.json" {
			return nil
		}

		relPath, relErr := filepath.Rel(d.workspace, path)
		if relErr != nil {
			return nil
		}

		// Split the relative path into components for root matching.
		components := strings.Split(relPath, string(os.PathSeparator))
		if len(components) < 3 {
			// Need at least: <root>/<something>/package.json
			return nil
		}

		for _, pat := range monorepoPatterns {
			if components[0] == pat.root && components[len(components)-1] == "package.json" {
				counts[pat.root]++
				// A single package.json belongs to at most one root.
				break
			}
		}

		return nil
	})

	var foundSignals []signal
	for _, pat := range monorepoPatterns {
		if cnt, ok := counts[pat.root]; ok && cnt > 0 {
			foundSignals = append(foundSignals, signal{
				Name:      "monorepo:" + pat.pattern,
				Priority:  pat.priority,
				Image:     "node:22",
				Rationale: fmt.Sprintf("Detected monorepo structure (%d packages in %s)", cnt, pat.pattern),
				lang:      "node",
			})
		}
	}

	return foundSignals
}

// matchSignal checks if a file matches any known language/framework signals.
func matchSignal(relPath, filename string) *signal {
	// Normalize path to use forward slashes
	normalPath := filepath.ToSlash(relPath)
	lowerPath := strings.ToLower(normalPath)
	lowerFile := strings.ToLower(filename)

	// Check each known signal type
	for _, sig := range knownSignals {
		if sig.match(normalPath, lowerPath, lowerFile) {
			return &sig
		}
	}

	return nil
}

// shouldSkipDirectory returns true if we should skip recursing into this directory.
func shouldSkipDirectory(path string) bool {
	base := filepath.Base(path)
	// Skip common directories that don't contain project code
	switch base {
	case ".git", ".hg", ".svn", "node_modules", "vendor", "target", "build",
		"dist", "out", "bin", "obj", ".venv", "venv", "env", "__pycache__",
		".pytest_cache", ".mypy_cache", ".tox", ".cache", "tmp", "temp":
		return true
	}
	return strings.HasPrefix(base, ".")
}

// scoreSignals selects the best signal based on priority.
// For polyglot repos, picks the highest priority (most specific) signal.
func (d *Detector) scoreSignals(signals []signal) signal {
	// Sort by priority (highest first)
	sort.Slice(signals, func(i, j int) bool {
		return signals[i].Priority > signals[j].Priority
	})

	return signals[0]
}

// signalNames extracts the names from a slice of signals.
func signalNames(signals []signal) []string {
	names := make([]string, len(signals))
	for i, sig := range signals {
		names[i] = sig.Name
	}
	return names
}

// detectVersion attempts to detect the language version and updates the image accordingly.
// Returns the final image and rationale.
func (d *Detector) detectVersion(sig signal) (string, string) {
	image, rationale, _ := d.detectVersionDebug(sig)
	return image, rationale
}

// detectVersionDebug attempts to detect the language version and updates the image accordingly.
// Returns the final image, rationale, and debug version info.
func (d *Detector) detectVersionDebug(sig signal) (string, string, *VersionDebugInfo) {
	// If signal has no language association, return the static image
	if sig.lang == "" {
		return sig.Image, sig.Rationale, nil
	}

	// Try to detect language version
	versionSource := detectLanguageVersion(d.workspace, sig.lang)
	if versionSource == nil || versionSource.Version == "" {
		// No version detected, use static image with note
		return sig.Image, fmt.Sprintf("%s (no version hint detected, using static default)", sig.Rationale), nil
	}

	// Build version-specific image
	image := buildVersionedImage(sig.Image, sig.lang, versionSource.Version)
	rationale := fmt.Sprintf("%s (version: %s)", sig.Rationale, formatVersionSource(versionSource))

	// Build debug info
	versionInfo := &VersionDebugInfo{
		Language:    sig.lang,
		Version:     versionSource.Version,
		SourceFile:  versionSource.File,
		SourceField: versionSource.Field,
		LineNumber:  versionSource.Line,
		RawValue:    versionSource.Original,
	}

	return image, rationale, versionInfo
}

// buildVersionedImage constructs a Docker image with the detected version.
// The base image template uses placeholders like "{{version}}".
func buildVersionedImage(baseImage, lang, version string) string {
	// Parse version to ensure it's valid
	v, err := ParseVersion(version)
	if err != nil {
		// If parsing fails, fall back to the original base image
		return baseImage
	}

	// For Node.js, we use just the major version (e.g., node:22).
	// Java images also typically use a major-only version in the tag (e.g., eclipse-temurin:17-jdk).
	// For Go and Python with minor versions, we use major.minor (e.g., golang:1.22, python:3.11).
	useMajorOnly := (lang == "node" || lang == "java")
	versionStr := v.ImageString(useMajorOnly)

	// Replace version placeholder in image template
	// Most images follow pattern: "name:version"
	parts := strings.Split(baseImage, ":")
	if len(parts) == 2 {
		tagParts := strings.Split(parts[1], "-")
		if len(tagParts) > 1 {
			// Handle special tags like "21-jdk" - replace version part
			return fmt.Sprintf("%s:%s-%s", parts[0], versionStr, strings.Join(tagParts[1:], "-"))
		}
		return fmt.Sprintf("%s:%s", parts[0], versionStr)
	}

	// If no colon in image, append version
	return fmt.Sprintf("%s:%s", baseImage, versionStr)
}

// knownSignals is the registry of all detectable language/framework signals.
// Ordered by general priority (higher priority = more specific).
var knownSignals = []signal{
	// Go - High priority (go.mod is definitive)
	{
		Name:      "go.mod",
		Priority:  100,
		Image:     "golang:1.23",
		Rationale: "Detected Go module (go.mod)",
		lang:      "go",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "go.mod"
		},
	},

	// Rust - High priority (Cargo.toml is definitive)
	// Note: Rust version detection not implemented yet
	{
		Name:      "Cargo.toml",
		Priority:  100,
		Image:     "rust:1.83",
		Rationale: "Detected Rust project (Cargo.toml)",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "cargo.toml"
		},
	},

	// Python - Multiple indicators
	{
		Name:      "pyproject.toml",
		Priority:  90,
		Image:     "python:3.13",
		Rationale: "Detected Python project (pyproject.toml)",
		lang:      "python",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "pyproject.toml"
		},
	},
	{
		Name:      "requirements.txt",
		Priority:  80,
		Image:     "python:3.13",
		Rationale: "Detected Python project (requirements.txt)",
		lang:      "python",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "requirements.txt"
		},
	},
	{
		Name:      "setup.py",
		Priority:  70,
		Image:     "python:3.13",
		Rationale: "Detected Python project (setup.py)",
		lang:      "python",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "setup.py"
		},
	},

	// Node.js/TypeScript - pnpm workspace (higher priority for monorepos)
	{
		Name:      "pnpm-workspace.yaml",
		Priority:  95,
		Image:     "node:22",
		Rationale: "Detected pnpm workspace (pnpm-workspace.yaml)",
		lang:      "node",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "pnpm-workspace.yaml" || lowerFile == "pnpm-workspace.yml"
		},
	},

	// Node.js/TypeScript - Multiple indicators
	{
		Name:      "package.json",
		Priority:  90,
		Image:     "node:22",
		Rationale: "Detected Node.js project (package.json)",
		lang:      "node",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "package.json"
		},
	},

	// Java - Multiple indicators
	{
		Name:      "pom.xml",
		Priority:  90,
		Image:     "eclipse-temurin:21-jdk",
		Rationale: "Detected Maven project (pom.xml)",
		lang:      "java",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "pom.xml"
		},
	},
	{
		Name:      "build.gradle",
		Priority:  90,
		Image:     "eclipse-temurin:21-jdk",
		Rationale: "Detected Gradle project (build.gradle)",
		lang:      "java",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "build.gradle"
		},
	},
	{
		Name:      "build.gradle.kts",
		Priority:  90,
		Image:     "eclipse-temurin:21-jdk",
		Rationale: "Detected Gradle project (build.gradle.kts)",
		lang:      "java",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "build.gradle.kts"
		},
	},

	// Ruby
	{
		Name:      "Gemfile",
		Priority:  90,
		Image:     "ruby:3.3",
		Rationale: "Detected Ruby project (Gemfile)",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "gemfile"
		},
	},

	// PHP
	{
		Name:      "composer.json",
		Priority:  90,
		Image:     "php:8.3",
		Rationale: "Detected PHP project (composer.json)",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "composer.json"
		},
	},

	// .NET/C#
	{
		Name:      "*.csproj",
		Priority:  90,
		Image:     "mcr.microsoft.com/dotnet/sdk:8.0",
		Rationale: "Detected .NET project (*.csproj)",
		match: func(path, lowerPath, lowerFile string) bool {
			return strings.HasSuffix(lowerFile, ".csproj")
		},
	},

	// Dockerfile
	{
		Name:      "Dockerfile",
		Priority:  60,
		Image:     "docker:24",
		Rationale: "Detected Dockerfile (using docker image)",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "dockerfile"
		},
	},
}

// Detect is a convenience function that detects the image for a workspace.
func Detect(workspace string) *DetectResult {
	d := NewDetector(workspace)
	return d.Detect()
}

// FormatResult formats a DetectResult as a human-readable string.
func FormatResult(result *DetectResult) string {
	if result.Disabled {
		return "Auto-detection disabled"
	}

	signalList := "none"
	if len(result.Signals) > 0 {
		signalList = strings.Join(result.Signals, ", ")
	}

	return fmt.Sprintf("Detected image: %s (signals: %s) - %s",
		result.Image, signalList, result.Rationale)
}
