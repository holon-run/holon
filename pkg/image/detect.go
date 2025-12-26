// Package image provides auto-detection of Docker base images
// based on workspace language/framework signals.
package image

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
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

// Detect analyzes the workspace to determine an appropriate base image.
// If no strong signal is detected, returns a safe default.
func (d *Detector) Detect() *DetectResult {
	signals := d.collectSignals()

	if len(signals) == 0 {
		return &DetectResult{
			Image:     DefaultImage,
			Signals:   []string{},
			Rationale: "No language signals detected, using default Go image",
		}
	}

	// Score each signal by priority
	bestSignal := d.scoreSignals(signals)

	return &DetectResult{
		Image:     bestSignal.Image,
		Signals:   signalNames(signals),
		Rationale: bestSignal.Rationale,
	}
}

// signal is a detected language/framework indicator in the workspace.
type signal struct {
	Name      string   // Name of the signal (e.g., "go.mod")
	Priority  int      // Higher = more specific/stronger signal
	Image     string   // Associated Docker image
	Rationale string   // Explanation for this choice
	match     func(string, string, string) bool
}

// collectSignals scans the workspace for language/framework signals.
func (d *Detector) collectSignals() []signal {
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
			signals = append(signals, *sig)
		}

		return nil
	})

	return signals
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

// knownSignals is the registry of all detectable language/framework signals.
// Ordered by general priority (higher priority = more specific).
var knownSignals = []signal{
	// Go - High priority (go.mod is definitive)
	{
		Name:      "go.mod",
		Priority:  100,
		Image:     "golang:1.23",
		Rationale: "Detected Go module (go.mod)",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "go.mod"
		},
	},

	// Rust - High priority (Cargo.toml is definitive)
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
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "pyproject.toml"
		},
	},
	{
		Name:      "requirements.txt",
		Priority:  80,
		Image:     "python:3.13",
		Rationale: "Detected Python project (requirements.txt)",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "requirements.txt"
		},
	},
	{
		Name:      "setup.py",
		Priority:  70,
		Image:     "python:3.13",
		Rationale: "Detected Python project (setup.py)",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "setup.py"
		},
	},

	// Node.js/TypeScript - Multiple indicators
	{
		Name:      "package.json",
		Priority:  90,
		Image:     "node:22",
		Rationale: "Detected Node.js project (package.json)",
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
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "pom.xml"
		},
	},
	{
		Name:      "build.gradle",
		Priority:  90,
		Image:     "eclipse-temurin:21-jdk",
		Rationale: "Detected Gradle project (build.gradle)",
		match: func(path, lowerPath, lowerFile string) bool {
			return lowerFile == "build.gradle"
		},
	},
	{
		Name:      "build.gradle.kts",
		Priority:  90,
		Image:     "eclipse-temurin:21-jdk",
		Rationale: "Detected Gradle project (build.gradle.kts)",
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
