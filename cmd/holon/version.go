package main

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"time"

	"github.com/holon-run/holon/pkg/github"
	"github.com/spf13/cobra"
)

// These variables are set via ldflags during build
var (
	Version   = "dev"
	Commit    = "unknown"
	BuildDate = "unknown"
)

var versionCheck bool
var versionQuiet bool

var versionCmd = &cobra.Command{
	Use:   "version",
	Short: "Show version information",
	Long: `Display version information for the Holon CLI.

This shows the version number, git commit SHA, and build date.
The version is set at build time via git tags.`,
	RunE: func(cmd *cobra.Command, args []string) error {
		// Check for updates if --check flag is provided
		if versionCheck {
			return checkForUpdates(cmd.Context())
		}

		// Default version display
		fmt.Printf("holon version %s\n", Version)
		if Commit != "" && Commit != "unknown" {
			fmt.Printf("commit: %s\n", Commit)
		}
		if BuildDate != "" && BuildDate != "unknown" {
			fmt.Printf("built at: %s\n", BuildDate)
		}
		return nil
	},
}

// checkForUpdates checks for newer Holon releases and displays the result
func checkForUpdates(ctx context.Context) error {
	// Add timeout for network operations
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()

	fmt.Printf("holon version %s\n", Version)

	release, upToDate, err := github.CheckForUpdates(ctx, Version)
	if err != nil {
		// Check if it's a disabled error
		if os.Getenv(github.VersionCheckEnvVar) != "" {
			// Silent exit when disabled
			return nil
		}
		// Print error but don't fail - this is a nice-to-have feature
		fmt.Fprintf(os.Stderr, "Warning: failed to check for updates: %v\n", err)
		return nil
	}

	// Display results
	if upToDate {
		if !versionQuiet {
			fmt.Printf("✓ You're running the latest version (%s)\n", release.TagName)
		}
		return nil
	}

	// Newer version available
	fmt.Printf("\n⚠️  A newer version is available!\n")
	fmt.Printf("   Current: %s\n", Version)
	fmt.Printf("   Latest:  %s\n", release.TagName)
	fmt.Printf("\nInstall instructions:\n")

	// Check if Homebrew is available on the system
	if isHomebrewAvailable() {
		fmt.Printf("   brew update && brew upgrade holon\n")
	} else {
		fmt.Printf("   Download: %s\n", release.HTMLURL)
	}

	return nil
}

// isHomebrewAvailable checks if Homebrew is available on the system
func isHomebrewAvailable() bool {
	_, err := exec.LookPath("brew")
	return err == nil
}

func init() {
	versionCmd.Flags().BoolVar(&versionCheck, "check", false, "Check for newer Holon releases")
	versionCmd.Flags().BoolVar(&versionQuiet, "quiet", false, "Quiet mode: suppress success message when up to date")
	rootCmd.AddCommand(versionCmd)
}
