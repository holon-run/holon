package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/holon-run/holon/pkg/image"
	"github.com/spf13/cobra"
)

var detectCmd = &cobra.Command{
	Use:   "detect",
	Short: "Detect workspace properties",
	Long: `Detect various properties of the workspace to help debug and understand
Holon's automatic detection behavior.`,
}

var (
	detectImageWorkspace string
	detectImageDebug     bool
	detectImageJSON      bool
)

var detectImageCmd = &cobra.Command{
	Use:   "image",
	Short: "Detect Docker base image for current workspace",
	Long: `Detect the appropriate Docker base image for the current workspace
by analyzing language signals (package.json, go.mod, etc.).

This is useful for:
- Testing auto-detection before running holon
- Debugging why a particular image was detected
- Understanding workspace detection behavior

The command scans the workspace for known project files and determines
the most appropriate Docker base image based on detected signals.

Examples:
  holon detect image
  holon detect image --workspace /path/to/project
  holon detect image --debug
  holon detect image --json`,
	RunE: func(cmd *cobra.Command, args []string) error {
		// Determine workspace path
		workspace := detectImageWorkspace
		if workspace == "" {
			workspace = "."
		}

		// Resolve to absolute path
		absWorkspace, err := filepath.Abs(workspace)
		if err != nil {
			return fmt.Errorf("failed to resolve workspace path: %w", err)
		}

		// Output based on format
		if detectImageJSON {
			return outputDetectJSON(absWorkspace)
		}

		if detectImageDebug {
			return outputDetectDebug(absWorkspace)
		}

		return outputDetectDefault(absWorkspace)
	},
}

// outputDetectDefault outputs the detection result in default human-readable format
func outputDetectDefault(workspace string) error {
	detector := image.NewDetector(workspace)
	debugResult := detector.DetectDebug()
	result := debugResult.DetectResult

	if result.Disabled {
		fmt.Printf("⚠ Auto-detection disabled\n")
		fmt.Printf("  Image: %s\n", result.Image)
		return nil
	}

	fmt.Printf("✓ Detected image: %s\n", result.Image)

	if len(result.Signals) > 0 {
		signals := formatSignals(result.Signals)
		fmt.Printf("  Signals: %s\n", signals)
	} else {
		fmt.Printf("  Signals: none\n")
	}

	fmt.Printf("  Rationale: %s\n", result.Rationale)
	fmt.Printf("  Scan mode: %s\n", debugResult.ScanMode)
	fmt.Printf("  Workspace: %s\n", workspace)

	return nil
}

// outputDetectDebug outputs the detection result with detailed debug information
func outputDetectDebug(workspace string) error {
	fmt.Printf("Scanning workspace: %s\n", workspace)

	// Run detection with debug information
	detector := image.NewDetector(workspace)
	debugResult := detector.DetectDebug()

	// Print detailed findings
	if len(debugResult.ScannedSignals) > 0 {
		for _, sig := range debugResult.ScannedSignals {
			if sig.Found {
				fmt.Printf("  ✓ Found signal: %s (priority: %d)\n", sig.Name, sig.Priority)
				if sig.Path != "" {
					fmt.Printf("    Path: %s\n", sig.Path)
				}
			}
		}
	} else {
		fmt.Printf("  ✗ No signals found\n")
	}

	if debugResult.VersionInfo != nil {
		fmt.Printf("  ✓ Language version detected:\n")
		fmt.Printf("    Language: %s\n", debugResult.VersionInfo.Language)
		fmt.Printf("    Version: %s\n", debugResult.VersionInfo.Version)
		fmt.Printf("    Source: %s:%s\n", debugResult.VersionInfo.SourceFile, debugResult.VersionInfo.SourceField)
		if debugResult.VersionInfo.LineNumber > 0 {
			fmt.Printf("    Line: %d\n", debugResult.VersionInfo.LineNumber)
		}
		fmt.Printf("    Raw: %q\n", debugResult.VersionInfo.RawValue)
	}

	fmt.Printf("  ✓ Total files scanned: %d\n", debugResult.FileCount)
	fmt.Printf("  ✓ Signals found: %d\n", len(debugResult.ScannedSignals))
	fmt.Printf("  ✓ Scan mode: %s\n", debugResult.ScanMode)

	if len(debugResult.ScannedSignals) > 0 {
		bestSignal := findBestSignal(debugResult.ScannedSignals)
		fmt.Printf("  ✓ Best signal: %s (priority: %d)\n", bestSignal.Name, bestSignal.Priority)
	}

	fmt.Printf("\n✓ Detected image: %s\n", debugResult.Image)
	if len(debugResult.Signals) > 0 {
		fmt.Printf("  Signals: %s\n", formatSignals(debugResult.Signals))
	} else {
		fmt.Printf("  Signals: none\n")
	}
	fmt.Printf("  Rationale: %s\n", debugResult.Rationale)

	if debugResult.VersionInfo != nil {
		fmt.Printf("  Version: %s\n", debugResult.VersionInfo.Version)
	}

	fmt.Printf("  Workspace: %s\n", workspace)
	fmt.Printf("  Files scanned: %d\n", debugResult.FileCount)
	fmt.Printf("  Scan duration: %dms\n", debugResult.DurationMs)
	fmt.Printf("  Scan mode: %s\n", debugResult.ScanMode)

	return nil
}

// outputDetectJSON outputs the detection result in JSON format
func outputDetectJSON(workspace string) error {
	detector := image.NewDetector(workspace)
	debugResult := detector.DetectDebug()
	result := debugResult.DetectResult

	type VersionSource struct {
		Language    string `json:"language,omitempty"`
		Version     string `json:"version,omitempty"`
		SourceFile  string `json:"source_file,omitempty"`
		SourceField string `json:"source_field,omitempty"`
		LineNumber  int    `json:"line_number,omitempty"`
		RawValue    string `json:"raw_value,omitempty"`
	}

	type ScanStats struct {
		FilesScanned int    `json:"files_scanned"`
		DurationMs   int64  `json:"duration_ms"`
		SignalsFound int    `json:"signals_found"`
		ScanMode     string `json:"scan_mode"`
	}

	output := struct {
		Success   bool           `json:"success"`
		Image     string         `json:"image"`
		Signals   []string       `json:"signals"`
		Rationale string         `json:"rationale"`
		Version   *VersionSource `json:"version,omitempty"`
		Workspace string         `json:"workspace"`
		Disabled  bool           `json:"disabled"`
		ScanStats *ScanStats     `json:"scan_stats,omitempty"`
	}{
		Success:   true,
		Image:     result.Image,
		Signals:   result.Signals,
		Rationale: result.Rationale,
		Workspace: workspace,
		Disabled:  result.Disabled,
		ScanStats: &ScanStats{
			FilesScanned: debugResult.FileCount,
			DurationMs:   debugResult.DurationMs,
			SignalsFound: len(debugResult.ScannedSignals),
			ScanMode:     debugResult.ScanMode,
		},
	}

	// Extract version info if available
	if debugResult.VersionInfo != nil {
		output.Version = &VersionSource{
			Language:    debugResult.VersionInfo.Language,
			Version:     debugResult.VersionInfo.Version,
			SourceFile:  debugResult.VersionInfo.SourceFile,
			SourceField: debugResult.VersionInfo.SourceField,
			LineNumber:  debugResult.VersionInfo.LineNumber,
			RawValue:    debugResult.VersionInfo.RawValue,
		}
	}

	return json.NewEncoder(os.Stdout).Encode(output)
}

// formatSignals formats a list of signal names for display
func formatSignals(signals []string) string {
	if len(signals) == 0 {
		return "none"
	}
	return strings.Join(signals, ", ")
}

// findBestSignal finds the highest priority signal from a list
func findBestSignal(signals []image.SignalMatch) image.SignalMatch {
	if len(signals) == 0 {
		return image.SignalMatch{}
	}

	best := signals[0]
	for _, sig := range signals {
		if sig.Priority > best.Priority {
			best = sig
		}
	}
	return best
}

func init() {
	detectCmd.AddCommand(detectImageCmd)

	detectImageCmd.Flags().StringVarP(&detectImageWorkspace, "workspace", "w", "",
		"Workspace path (default: current directory)")
	detectImageCmd.Flags().BoolVarP(&detectImageDebug, "debug", "d", false,
		"Enable debug output with detailed scan information")
	detectImageCmd.Flags().BoolVar(&detectImageJSON, "json", false,
		"Output result in JSON format")
}
