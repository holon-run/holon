package main

import (
	"fmt"

	"github.com/spf13/cobra"
)

// These variables are set via ldflags during build
var (
	Version   = "dev"
	Commit    = "unknown"
	BuildDate = "unknown"
)

var versionCmd = &cobra.Command{
	Use:   "version",
	Short: "Show version information",
	Long: `Display version information for the Holon CLI.

This shows the version number, git commit SHA, and build date.
The version is set at build time via git tags.`,
	RunE: func(cmd *cobra.Command, args []string) error {
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

func init() {
	rootCmd.AddCommand(versionCmd)
}
