package github

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

	hghelper "github.com/holon-run/holon/pkg/github"
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

	// Verify that context files are non-empty; if empty, fail fast to avoid silent truncation.
	if err := verifyContextFiles(c.config.OutputDir); err != nil {
		return err
	}
	printContextFileSizes(c.config.OutputDir)

	fmt.Println("Context collection complete!")
	fmt.Printf("  Output directory: %s/\n", c.config.OutputDir)
	fmt.Printf("  GitHub context directory: %s/github/\n", c.config.OutputDir)
	fmt.Println("  Files created:")
	fmt.Println("    - github/pr.json")
	fmt.Println("    - github/review_threads.json")
	fmt.Println("    - pr-fix.schema.json")
	if diff != "" {
		fmt.Println("    - github/pr.diff")
	}
	fmt.Println("    - github/review.md")

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

	// Get token (from env vars or gh CLI)
	token, fromGh := hghelper.GetTokenFromEnv()
	if token == "" {
		return fmt.Errorf("GITHUB_TOKEN or HOLON_GITHUB_TOKEN environment variable not set (or use 'gh auth login')")
	}

	if fromGh {
		fmt.Fprintln(os.Stderr, "Using GitHub token from 'gh auth token' (GITHUB_TOKEN not set)")
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

// verifyContextFiles ensures required context files are non-empty.
func verifyContextFiles(outputDir string) error {
	paths := []string{
		filepath.Join(outputDir, "github", "pr.json"),
		filepath.Join(outputDir, "github", "review_threads.json"),
		filepath.Join(outputDir, "github", "review.md"),
		filepath.Join(outputDir, "pr-fix.schema.json"),
	}
	for _, p := range paths {
		info, err := os.Stat(p)
		if err != nil {
			return fmt.Errorf("context file missing: %s: %w", p, err)
		}
		if info.Size() == 0 {
			return fmt.Errorf("context file is empty: %s (check GitHub token and network)", p)
		}
	}
	return nil
}

func printContextFileSizes(outputDir string) {
	files := []string{
		filepath.Join(outputDir, "github", "pr.json"),
		filepath.Join(outputDir, "github", "review_threads.json"),
		filepath.Join(outputDir, "github", "pr.diff"),
		filepath.Join(outputDir, "github", "review.md"),
		filepath.Join(outputDir, "pr-fix.schema.json"),
	}
	fmt.Println("  Context file sizes:")
	for _, f := range files {
		info, err := os.Stat(f)
		if err != nil {
			fmt.Printf("    - %s: error: %v\n", f, err)
			continue
		}
		fmt.Printf("    - %s: %d bytes\n", f, info.Size())
	}
}
