// Package issue provides context collection for GitHub issues.
package issue

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/holon-run/holon/pkg/github"
)

// CollectorConfig holds configuration for collecting issue context
type CollectorConfig struct {
	Owner     string
	Repo      string
	IssueNum  int
	Token     string
	OutputDir string
}

// Collector orchestrates the collection of issue context
type Collector struct {
	client *github.Client
	config CollectorConfig
}

// NewCollector creates a new issue Collector
func NewCollector(config CollectorConfig) *Collector {
	return &Collector{
		client: github.NewClient(config.Token),
		config: config,
	}
}

// IssueContext holds the collected issue context
type IssueContext struct {
	Info     *github.IssueInfo     `json:"info"`
	Comments []github.IssueComment `json:"comments"`
}

// Collect gathers all issue context and writes it to the output directory
func (c *Collector) Collect(ctx context.Context) error {
	fmt.Printf("Collecting issue context for %s/%s#%d...\n", c.config.Owner, c.config.Repo, c.config.IssueNum)

	// Fetch issue info
	fmt.Println("Fetching issue information...")
	issueInfo, err := c.client.FetchIssueInfo(ctx, c.config.Owner, c.config.Repo, c.config.IssueNum)
	if err != nil {
		return fmt.Errorf("failed to fetch issue info: %w", err)
	}
	fmt.Printf("  Issue #%d: %s\n", issueInfo.Number, issueInfo.Title)

	// Fetch issue comments
	fmt.Println("Fetching issue comments...")
	comments, err := c.client.FetchIssueComments(ctx, c.config.Owner, c.config.Repo, c.config.IssueNum)
	if err != nil {
		return fmt.Errorf("failed to fetch issue comments: %w", err)
	}
	fmt.Printf("  Found %d comments\n", len(comments))

	// Write context files
	fmt.Printf("Writing context files to %s...\n", c.config.OutputDir)
	if err := c.writeContext(issueInfo, comments); err != nil {
		return fmt.Errorf("failed to write context: %w", err)
	}

	fmt.Println("Context collection complete!")
	fmt.Printf("  Output directory: %s/\n", c.config.OutputDir)
	fmt.Printf("  GitHub context directory: %s/github/\n", c.config.OutputDir)
	fmt.Println("  Files created:")
	fmt.Println("    - github/issue.json")
	fmt.Println("    - github/issue_comments.json")

	return nil
}

// writeContext writes the issue context to files
func (c *Collector) writeContext(info *github.IssueInfo, comments []github.IssueComment) error {
	// Create github directory
	githubDir := filepath.Join(c.config.OutputDir, "github")
	if err := os.MkdirAll(githubDir, 0755); err != nil {
		return fmt.Errorf("failed to create github directory: %w", err)
	}

	// Write issue info
	issueData, err := json.MarshalIndent(info, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal issue info: %w", err)
	}
	issuePath := filepath.Join(githubDir, "issue.json")
	if err := os.WriteFile(issuePath, issueData, 0644); err != nil {
		return fmt.Errorf("failed to write issue.json: %w", err)
	}

	// Write comments
	commentsData, err := json.MarshalIndent(comments, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal comments: %w", err)
	}
	commentsPath := filepath.Join(githubDir, "issue_comments.json")
	if err := os.WriteFile(commentsPath, commentsData, 0644); err != nil {
		return fmt.Errorf("failed to write issue_comments.json: %w", err)
	}

	return nil
}
