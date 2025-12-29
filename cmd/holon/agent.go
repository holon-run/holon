package main

import (
	"fmt"
	"net/url"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/holon-run/holon/pkg/agent"
	"github.com/holon-run/holon/pkg/agent/cache"
	"github.com/spf13/cobra"
)

var agentCmd = &cobra.Command{
	Use:   "agent",
	Short: "Manage agent bundles and aliases",
	Long: `Manage agent bundles, including installing aliases and listing cached bundles.

This command provides functionality for:
- Installing agent aliases for easier reference
- Listing configured aliases
- Managing the local agent cache`,
}

var agentInstallCmd = &cobra.Command{
	Use:   "install <url> --name <alias>",
	Short: "Install an agent bundle alias",
	Long: `Install an alias for an agent bundle URL. This allows you to use short names
instead of full URLs when referencing agent bundles.

Examples:
  holon agent install https://github.com/example/agent/releases/download/v1.0.0/agent.tar.gz --name myagent
  holon agent install https://github.com/example/agent/releases/download/v1.0.0/agent.tar.gz#sha256=abcd1234 --name myagent-verified`,
	Args: cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		url := args[0]
		name, _ := cmd.Flags().GetString("name")
		if name == "" {
			return fmt.Errorf("--name is required")
		}

		// Validate URL format and scheme
		if err := validateURL(url); err != nil {
			return fmt.Errorf("invalid URL: %w", err)
		}

		cacheDir := os.Getenv("HOLON_CACHE_DIR")
		c := cache.New(cacheDir)

		if err := c.SetAlias(name, url); err != nil {
			return fmt.Errorf("failed to install alias: %w", err)
		}

		fmt.Printf("Alias '%s' installed for: %s\n", name, url)
		return nil
	},
}

var agentListCmd = &cobra.Command{
	Use:   "list",
	Short: "List installed agent aliases",
	Long: `List all configured agent aliases. This shows the mapping between
alias names and their corresponding URLs.`,
	RunE: func(cmd *cobra.Command, args []string) error {
		cacheDir := os.Getenv("HOLON_CACHE_DIR")
		c := cache.New(cacheDir)

		aliases, err := c.ListAliases()
		if err != nil {
			return fmt.Errorf("failed to list aliases: %w", err)
		}

		if len(aliases) == 0 {
			fmt.Println("No agent aliases installed.")
			fmt.Println("Use 'holon agent install <url> --name <alias>' to add one.")
			return nil
		}

		fmt.Println("Installed agent aliases:")
		fmt.Println()

		// Sort aliases alphabetically for consistent output
		sortedAliases := make([]string, 0, len(aliases))
		for name := range aliases {
			sortedAliases = append(sortedAliases, name)
		}
		sort.Strings(sortedAliases)

		for _, name := range sortedAliases {
			fmt.Printf("  %s: %s\n", name, aliases[name])
		}
		fmt.Println()
		fmt.Println("Usage example:")
		fmt.Println("  holon run --agent <alias> --goal \"your goal\"")

		return nil
	},
}

var agentRemoveCmd = &cobra.Command{
	Use:   "remove <alias>",
	Short: "Remove an agent alias",
	Long: `Remove an installed agent alias. This does not remove any cached bundles,
just the alias name mapping.`,
	Args: cobra.ExactArgs(1),
	RunE: func(cmd *cobra.Command, args []string) error {
		name := args[0]

		cacheDir := os.Getenv("HOLON_CACHE_DIR")
		c := cache.New(cacheDir)

		if err := c.RemoveAlias(name); err != nil {
			return fmt.Errorf("failed to remove alias: %w", err)
		}

		fmt.Printf("Alias '%s' removed.\n", name)
		return nil
	},
}

// validateURL checks that the URL has a valid format and supported scheme
func validateURL(urlStr string) error {
	// Parse URL to validate format
	parsedURL, err := url.Parse(urlStr)
	if err != nil {
		return fmt.Errorf("invalid URL format: %w", err)
	}

	// Check for supported schemes
	if parsedURL.Scheme != "http" && parsedURL.Scheme != "https" {
		return fmt.Errorf("unsupported URL scheme: %s (only http and https are supported)", parsedURL.Scheme)
	}

	// Ensure the URL has a host
	if parsedURL.Host == "" {
		return fmt.Errorf("URL must have a host")
	}

	return nil
}

var agentCheckUpdateCmd = &cobra.Command{
	Use:   "check-update",
	Short: "Check for newer agent versions",
	Long: `Check if there's a newer version of the builtin agent available
from GitHub releases.

This command specifically looks for agent releases in the holon-run/holon
repository, filtering for releases that:
- Have tags starting with "agent-" (e.g., agent-claude-v0.2.0)
- Contain agent bundle assets (.tar.gz files)
- Are stable releases (not drafts or prereleases)

This ensures we distinguish between agent bundle releases and main Holon
application releases.`,
	RunE: func(cmd *cobra.Command, args []string) error {
		fmt.Println("Checking for newer agent versions...")
		fmt.Println()

		// Get current builtin agent info
		currentAgent := agent.DefaultBuiltinAgent()
		if currentAgent == nil {
			fmt.Println("No builtin agent configured.")
			return nil
		}

		fmt.Printf("Current builtin agent:\n")
		fmt.Printf("  Version: %s\n", currentAgent.Version)
		fmt.Printf("  URL: %s\n", currentAgent.URL)
		fmt.Println()

		// Check latest agent release
		latestRelease, err := agent.GetLatestAgentRelease("holon-run/holon")
		if err != nil {
			fmt.Printf("Error checking for updates: %v\n", err)
			fmt.Println("Note: This requires internet access to GitHub API.")
			return nil
		}

		fmt.Printf("Latest agent release: %s\n", latestRelease.TagName)
		fmt.Printf("Published: %s\n", latestRelease.PublishedAt)
		fmt.Printf("Release name: %s\n", latestRelease.Name)
		fmt.Println()

		// Find agent bundle in the release
		bundleName, bundleURL, err := agent.FindAgentBundleAsset(latestRelease)
		if err != nil {
			fmt.Printf("Warning: %v\n", err)
			return nil
		}

		fmt.Printf("Agent bundle: %s\n", bundleName)
		fmt.Printf("Bundle URL: %s\n", bundleURL)
		fmt.Println()

		// Compare versions
		if currentAgent.Version == latestRelease.TagName {
			fmt.Println("✓ You have the latest version!")
		} else {
			fmt.Printf("⚠ A newer version is available: %s\n", latestRelease.TagName)
			fmt.Println()
			fmt.Println("To update to the latest version, update the following in pkg/agent/builtin.go:")
			fmt.Printf("  Version:  %q\n", latestRelease.TagName)
			fmt.Printf("  URL:      %q\n", bundleURL)
			fmt.Println("  Checksum: <SHA256 checksum of the bundle>")
		}

		return nil
	},
}

var agentInfoCmd = &cobra.Command{
	Use:   "info",
	Short: "Show information about agent bundles",
	Long: `Show detailed information about agent bundles, including the builtin
default agent configuration.`,
}

var agentInfoDefaultCmd = &cobra.Command{
	Use:   "default",
	Short: "Show information about the builtin default agent",
	Long: `Display detailed information about the builtin default agent that Holon
uses when no agent is explicitly specified.

This shows the agent version, download URL, and checksum that will be used
for auto-installation.`,
	RunE: func(cmd *cobra.Command, args []string) error {
		builtinAgent := agent.DefaultBuiltinAgent()
		if builtinAgent == nil {
			fmt.Println("No builtin agent configured.")
			return nil
		}

		fmt.Printf("Builtin Default Agent:\n")
		fmt.Printf("  Name:     %s\n", builtinAgent.Name)
		fmt.Printf("  Version:  %s\n", builtinAgent.Version)
		fmt.Printf("  URL:      %s\n", builtinAgent.URL)
		fmt.Printf("  Checksum: %s\n", builtinAgent.Checksum)

		fmt.Println()
		if agent.IsAutoInstallDisabled() {
			fmt.Println("Auto-install: DISABLED (HOLON_NO_AUTO_INSTALL=1)")
		} else {
			fmt.Println("Auto-install: ENABLED")
		}

		fmt.Println()
		fmt.Println("Usage:")
		fmt.Println("  holon run --goal \"your goal\"      # Uses builtin agent")
		fmt.Println("  holon run --agent default --goal \"your goal\"  # Explicitly uses builtin agent")

		return nil
	},
}

var agentUpdateCmd = &cobra.Command{
	Use:   "update",
	Short: "Update the cached latest agent metadata",
	Long: `Eagerly refresh the cached latest agent metadata from GitHub.
This respects the configured agent channel.

This command fetches the latest agent release information from GitHub
and updates the local cache. It does not download the actual agent bundle
unless needed.`,
	RunE: func(cmd *cobra.Command, args []string) error {
		fmt.Println("Updating agent metadata...")

		// Get channel from flag or config
		channel := strings.TrimSpace(os.Getenv("HOLON_AGENT_CHANNEL"))
		if channel == "" {
			channel = "latest" // Default
		}

		cacheDir := os.Getenv("HOLON_CACHE_DIR")
		c := cache.New(cacheDir)

		if channel == "builtin" {
			fmt.Println("Channel is 'builtin' - no update needed")
			fmt.Println("The builtin agent is embedded in the Holon binary.")
			return nil
		}

		// Fetch latest from GitHub
		fmt.Println("Fetching latest agent release from GitHub...")
		latestRelease, err := agent.GetLatestAgentRelease("holon-run/holon")
		if err != nil {
			return fmt.Errorf("failed to fetch latest agent release: %w", err)
		}

		// Extract bundle info
		bundleName, bundleURL, err := agent.FindAgentBundleAsset(latestRelease)
		if err != nil {
			return fmt.Errorf("failed to find agent bundle in release: %w", err)
		}

		// Fetch checksum
		checksum, err := agent.FetchChecksum(bundleURL + ".sha256")
		if err != nil {
			fmt.Printf("Warning: failed to fetch checksum: %v\n", err)
			checksum = ""
		}

		// Store latest metadata
		metadata := &cache.LatestAgentMetadata{
			Version:   latestRelease.TagName,
			URL:       bundleURL,
			Checksum:  checksum,
			FetchedAt: time.Now().Unix(),
		}

		if err := c.SetLatestAgentMetadata(metadata); err != nil {
			return fmt.Errorf("failed to cache latest agent metadata: %w", err)
		}

		fmt.Printf("✓ Updated to version: %s\n", latestRelease.TagName)
		fmt.Printf("  Bundle: %s\n", bundleName)
		fmt.Printf("  URL: %s\n", bundleURL)
		if checksum != "" {
			fmt.Printf("  Checksum: %s\n", checksum)
		}

		// Check if current version matches
		builtinAgent := agent.DefaultBuiltinAgent()
		if builtinAgent != nil && builtinAgent.Version == latestRelease.TagName {
			fmt.Println("\n✓ Your builtin agent is already up to date!")
		} else if builtinAgent != nil {
			fmt.Printf("\n⚠ A newer version is available (your builtin: %s)\n", builtinAgent.Version)
			fmt.Println("Run 'holon run --agent-channel latest' to use the latest version.")
		}

		return nil
	},
}

func init() {
	agentInstallCmd.Flags().String("name", "", "Alias name for the agent bundle (required)")
	_ = agentInstallCmd.MarkFlagRequired("name")

	agentCmd.AddCommand(agentInstallCmd)
	agentCmd.AddCommand(agentListCmd)
	agentCmd.AddCommand(agentRemoveCmd)
	agentCmd.AddCommand(agentCheckUpdateCmd)
	agentCmd.AddCommand(agentUpdateCmd)
	agentCmd.AddCommand(agentInfoCmd)
	agentInfoCmd.AddCommand(agentInfoDefaultCmd)
}