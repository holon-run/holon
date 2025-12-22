package github

import (
	"context"
	"fmt"
	"os"
)

// CollectorConfig holds configuration for collecting PR context
type CollectorConfig struct {
	Owner          string
	Repo           string
	PRNumber       int
	Token          string
	OutputDir      string
	UnresolvedOnly bool
	IncludeDiff    bool
}

// Collector orchestrates the collection of PR context
type Collector struct {
	client *Client
	config CollectorConfig
}

// NewCollector creates a new Collector
func NewCollector(config CollectorConfig) *Collector {
	return &Collector{
		client: NewClient(config.Token),
		config: config,
	}
}

// Collect gathers all PR context and writes it to the output directory
func (c *Collector) Collect(ctx context.Context) error {
	fmt.Printf("Collecting PR context for %s/%s#%d...\n", c.config.Owner, c.config.Repo, c.config.PRNumber)

	// Fetch PR info
	fmt.Println("Fetching PR information...")
	prInfo, err := c.client.FetchPRInfo(ctx, c.config.Owner, c.config.Repo, c.config.PRNumber)
	if err != nil {
		return fmt.Errorf("failed to fetch PR info: %w", err)
	}
	fmt.Printf("  PR #%d: %s\n", prInfo.Number, prInfo.Title)

	// Fetch review threads
	fmt.Println("Fetching review threads...")
	reviewThreads, err := c.client.FetchReviewThreads(ctx, c.config.Owner, c.config.Repo, c.config.PRNumber, c.config.UnresolvedOnly)
	if err != nil {
		return fmt.Errorf("failed to fetch review threads: %w", err)
	}
	fmt.Printf("  Found %d review threads\n", len(reviewThreads))

	// Fetch diff if requested
	var diff string
	if c.config.IncludeDiff {
		fmt.Println("Fetching PR diff...")
		diff, err = c.client.FetchPRDiff(ctx, c.config.Owner, c.config.Repo, c.config.PRNumber)
		if err != nil {
			fmt.Printf("  Warning: failed to fetch diff: %v\n", err)
			// Don't fail - diff is optional
		} else {
			fmt.Printf("  Fetched diff (%d bytes)\n", len(diff))
		}
	}

	// Write context files
	fmt.Printf("Writing context files to %s...\n", c.config.OutputDir)
	if err := WriteContext(c.config.OutputDir, prInfo, reviewThreads, diff); err != nil {
		return fmt.Errorf("failed to write context: %w", err)
	}

	fmt.Println("Context collection complete!")
	fmt.Printf("  Output directory: %s/github/\n", c.config.OutputDir)
	fmt.Println("  Files created:")
	fmt.Println("    - pr.json")
	fmt.Println("    - review_threads.json")
	if diff != "" {
		fmt.Println("    - pr.diff")
	}
	fmt.Println("    - review.md")

	return nil
}

// CollectFromEnv collects PR context using environment variables
// This is useful for GitHub Actions integration
func CollectFromEnv(ctx context.Context, outputDir string) error {
	// Parse repository from GITHUB_REPOSITORY env var
	repo := os.Getenv("GITHUB_REPOSITORY")
	if repo == "" {
		return fmt.Errorf("GITHUB_REPOSITORY environment variable not set")
	}

	owner, repoName, err := ParseRepoFromURL(repo)
	if err != nil {
		return fmt.Errorf("failed to parse repository: %w", err)
	}

	// Get PR number from event
	prNumberStr := os.Getenv("PR_NUMBER")
	if prNumberStr == "" {
		return fmt.Errorf("PR_NUMBER environment variable not set")
	}

	prNumber, err := ParsePRNumber(prNumberStr)
	if err != nil {
		return fmt.Errorf("failed to parse PR number: %w", err)
	}

	// Get token
	token := os.Getenv("GITHUB_TOKEN")
	if token == "" {
		token = os.Getenv("GH_TOKEN")
	}
	if token == "" {
		return fmt.Errorf("GITHUB_TOKEN or GH_TOKEN environment variable not set")
	}

	// Check if we should only include unresolved threads
	unresolvedOnly := os.Getenv("UNRESOLVED_ONLY") == "true"

	// Check if we should include diff
	includeDiff := os.Getenv("INCLUDE_DIFF") != "false" // Default to true

	config := CollectorConfig{
		Owner:          owner,
		Repo:           repoName,
		PRNumber:       prNumber,
		Token:          token,
		OutputDir:      outputDir,
		UnresolvedOnly: unresolvedOnly,
		IncludeDiff:    includeDiff,
	}

	collector := NewCollector(config)
	return collector.Collect(ctx)
}
