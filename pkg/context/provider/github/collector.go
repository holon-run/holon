package github

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/holon-run/holon/pkg/context/collector"
)

// Provider implements the collector.Collector interface for GitHub
type Provider struct {
	client *Client
}

// NewProvider creates a new GitHub provider
func NewProvider() *Provider {
	return &Provider{}
}

// Name returns the provider name
func (p *Provider) Name() string {
	return "github"
}

// Validate checks if the request is valid for this provider
func (p *Provider) Validate(req collector.CollectRequest) error {
	if req.Kind != collector.KindIssue && req.Kind != collector.KindPR {
		return fmt.Errorf("unsupported kind: %s (only 'issue' and 'pr' are supported)", req.Kind)
	}

	if req.Ref == "" {
		return fmt.Errorf("ref is required")
	}

	// Parse owner and repo from ref
	owner, repo, number, err := ParseRef(req.Ref, req.RepoHint)
	if err != nil {
		return fmt.Errorf("invalid ref: %w", err)
	}

	if owner == "" {
		return fmt.Errorf("owner could not be determined from ref")
	}
	if repo == "" {
		return fmt.Errorf("repo could not be determined from ref")
	}
	if number == 0 {
		return fmt.Errorf("number could not be determined from ref")
	}

	// Ensure output directory exists
	if req.OutputDir == "" {
		return fmt.Errorf("output directory is required")
	}

	return nil
}

// Collect gathers context and writes it to the output directory
func (p *Provider) Collect(ctx context.Context, req collector.CollectRequest) (collector.CollectResult, error) {
	result := collector.CollectResult{
		Provider:    p.Name(),
		Kind:        req.Kind,
		Ref:         req.Ref,
		CollectedAt: time.Now(),
		Success:     false,
	}

	// Parse ref
	owner, repo, number, err := ParseRef(req.Ref, req.RepoHint)
	if err != nil {
		result.Error = fmt.Sprintf("failed to parse ref: %v", err)
		return result, err
	}
	result.Owner = owner
	result.Repo = repo
	result.Number = number

	// Create client
	if req.Options.Token == "" {
		result.Error = "token is required"
		return result, fmt.Errorf("token is required")
	}
	p.client = NewClient(req.Options.Token)

	// Create output directory
	if err := os.MkdirAll(req.OutputDir, 0755); err != nil {
		result.Error = fmt.Sprintf("failed to create output directory: %v", err)
		return result, fmt.Errorf("failed to create output directory: %w", err)
	}

	// Collect based on kind
	var files []collector.FileInfo
	if req.Kind == collector.KindPR {
		files, err = p.collectPR(ctx, owner, repo, number, req)
	} else if req.Kind == collector.KindIssue {
		files, err = p.collectIssue(ctx, owner, repo, number, req)
	}

	if err != nil {
		result.Error = fmt.Sprintf("collection failed: %v", err)
		return result, err
	}

	result.Files = files
	result.Success = true

	// Write manifest
	if err := WriteManifest(req.OutputDir, result); err != nil {
		result.Error = fmt.Sprintf("failed to write manifest: %v", err)
		result.Success = false
		return result, err
	}

	return result, nil
}

// collectPR collects PR context
func (p *Provider) collectPR(ctx context.Context, owner, repo string, number int, req collector.CollectRequest) ([]collector.FileInfo, error) {
	fmt.Printf("Collecting PR context for %s/%s#%d...\n", owner, repo, number)

	// Fetch PR info
	fmt.Println("Fetching PR information...")
	prInfo, err := p.client.FetchPRInfo(ctx, owner, repo, number)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch PR info: %w", err)
	}
	fmt.Printf("  PR #%d: %s\n", prInfo.Number, prInfo.Title)

	// Fetch review threads
	fmt.Println("Fetching review threads...")
	unresolvedOnly := req.Options.UnresolvedOnly
	reviewThreads, err := p.client.FetchReviewThreads(ctx, owner, repo, number, unresolvedOnly)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch review threads: %w", err)
	}
	fmt.Printf("  Found %d review threads\n", len(reviewThreads))

	// Fetch diff if requested
	var diff string
	if req.Options.IncludeDiff {
		fmt.Println("Fetching PR diff...")
		diff, err = p.client.FetchPRDiff(ctx, owner, repo, number)
		if err != nil {
			fmt.Printf("  Warning: failed to fetch diff: %v\n", err)
			// Don't fail - diff is optional
		}
	}

	// Fetch check runs and status if requested
	var checkRuns []CheckRun
	var combinedStatus *CombinedStatus
	if req.Options.IncludeChecks {
		fmt.Println("Fetching CI/check results...")

		// Fetch check runs
		checkRuns, err = p.client.FetchCheckRuns(ctx, owner, repo, prInfo.HeadSHA, req.Options.ChecksMax)
		if err != nil {
			fmt.Printf("  Warning: failed to fetch check runs: %v\n", err)
			// Don't fail - checks are optional
		} else {
			// Filter to only failed checks if requested
			// Note: Incomplete checks (queued/in_progress) have no conclusion and are excluded when filtering to only failed checks
			if req.Options.ChecksOnlyFailed {
				failedRuns := []CheckRun{}
				for _, cr := range checkRuns {
					if cr.Conclusion == "failure" || cr.Conclusion == "timed_out" || cr.Conclusion == "action_required" {
						failedRuns = append(failedRuns, cr)
					}
				}
				checkRuns = failedRuns
			}
			fmt.Printf("  Found %d check runs\n", len(checkRuns))
		}

		// Fetch combined status (optional but cheap)
		combinedStatus, err = p.client.FetchCombinedStatus(ctx, owner, repo, prInfo.HeadSHA)
		if err != nil {
			fmt.Printf("  Warning: failed to fetch combined status: %v\n", err)
			// Don't fail - status is optional
		} else {
			fmt.Printf("  Combined status: %s\n", combinedStatus.State)
		}
	}

	// Write context files
	fmt.Printf("Writing context files to %s...\n", req.OutputDir)
	files, err := WritePRContext(req.OutputDir, prInfo, reviewThreads, diff, checkRuns, combinedStatus)
	if err != nil {
		return nil, fmt.Errorf("failed to write context: %w", err)
	}

	fmt.Println("Context collection complete!")
	return files, nil
}

// collectIssue collects issue context
func (p *Provider) collectIssue(ctx context.Context, owner, repo string, number int, req collector.CollectRequest) ([]collector.FileInfo, error) {
	fmt.Printf("Collecting issue context for %s/%s#%d...\n", owner, repo, number)

	// Fetch issue info
	fmt.Println("Fetching issue information...")
	issueInfo, err := p.client.FetchIssueInfo(ctx, owner, repo, number)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch issue info: %w", err)
	}
	fmt.Printf("  Issue #%d: %s\n", issueInfo.Number, issueInfo.Title)

	// Fetch comments
	fmt.Println("Fetching comments...")
	comments, err := p.client.FetchIssueComments(ctx, owner, repo, number)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch comments: %w", err)
	}
	fmt.Printf("  Found %d comments\n", len(comments))

	// Write context files
	fmt.Printf("Writing context files to %s...\n", req.OutputDir)
	files, err := WriteIssueContext(req.OutputDir, issueInfo, comments)
	if err != nil {
		return nil, fmt.Errorf("failed to write context: %w", err)
	}

	fmt.Println("Context collection complete!")
	return files, nil
}

// ParseRef parses a reference string into owner, repo, and number
// Supports formats:
// - "123" or "#123" (requires repoHint)
// - "owner/repo#123"
// - "https://github.com/owner/repo/pull/123"
// - "https://github.com/owner/repo/issues/123"
func ParseRef(ref, repoHint string) (owner, repo string, number int, err error) {
	// Parse owner/repo from repoHint if provided
	if repoHint != "" {
		o, r, e := parseRepo(repoHint)
		if e != nil {
			return "", "", 0, fmt.Errorf("invalid repo hint %q: %w", repoHint, e)
		}
		owner = o
		repo = r
	}

	// Check if ref is a URL
	if strings.Contains(ref, "github.com") {
		return parseGitHubURL(ref)
	}

	// Check if ref contains owner/repo
	if strings.Contains(ref, "/") {
		parts := strings.SplitN(ref, "/", 3)
		if len(parts) >= 2 {
			owner = parts[0]
			repo = parts[1]
			// Extract number from the repo part if it contains "#"
			if strings.Contains(repo, "#") {
				repoParts := strings.SplitN(repo, "#", 2)
				repo = repoParts[0]
				numStr := repoParts[1]
				num, e := parseNumber(numStr)
				if e != nil {
					return "", "", 0, fmt.Errorf("failed to parse number from %s: %w", numStr, e)
				}
				number = num
			} else if len(parts) == 3 && parts[2] != "" {
				// Number is after second slash (e.g., "owner/repo/123")
				numStr := strings.TrimPrefix(parts[2], "#")
				num, e := parseNumber(numStr)
				if e != nil {
					return "", "", 0, fmt.Errorf("failed to parse number from %s: %w", parts[2], e)
				}
				number = num
			}
		}
		return owner, repo, number, nil
	}

	// Just a number (e.g., "123" or "#123")
	numStr := strings.TrimPrefix(ref, "#")
	num, e := parseNumber(numStr)
	if e != nil {
		return "", "", 0, fmt.Errorf("failed to parse number from %s: %w", ref, e)
	}
	number = num

	return owner, repo, number, nil
}

// parseGitHubURL parses a GitHub URL
func parseGitHubURL(urlStr string) (owner, repo string, number int, err error) {
	// Remove protocol
	cleaned := strings.TrimPrefix(urlStr, "https://")
	cleaned = strings.TrimPrefix(cleaned, "http://")
	cleaned = strings.TrimSuffix(cleaned, "/")

	// Expected format: github.com/owner/repo/pulls/123 or github.com/owner/repo/issues/123
	parts := strings.Split(cleaned, "/")
	if len(parts) < 5 {
		return "", "", 0, fmt.Errorf("invalid GitHub URL format: %s", urlStr)
	}

	owner = parts[1]
	repo = parts[2]

	// Parse number from last part
	num, e := parseNumber(parts[len(parts)-1])
	if e != nil {
		return "", "", 0, fmt.Errorf("failed to parse number from URL: %w", e)
	}
	number = num

	return owner, repo, number, nil
}

// parseRepo parses owner/repo from various formats
func parseRepo(repo string) (owner, name string, err error) {
	// Handle formats like:
	// - github.com/owner/repo
	// - https://github.com/owner/repo
	// - owner/repo

	cleaned := strings.TrimPrefix(repo, "https://")
	cleaned = strings.TrimPrefix(cleaned, "http://")
	cleaned = strings.TrimPrefix(cleaned, "github.com/")
	cleaned = strings.TrimSuffix(cleaned, ".git")
	cleaned = strings.TrimSuffix(cleaned, "/")

	parts := strings.Split(cleaned, "/")
	if len(parts) < 2 {
		return "", "", fmt.Errorf("invalid repository format: %s", repo)
	}

	return parts[0], parts[1], nil
}

// parseNumber parses a number from string
func parseNumber(s string) (int, error) {
	var num int
	_, err := fmt.Sscanf(s, "%d", &num)
	return num, err
}
