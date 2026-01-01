package github

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
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

	// Verify context files are non-empty before returning success
	if err := verifyContextFiles(req.OutputDir, req.Kind); err != nil {
		result.Error = fmt.Sprintf("context verification failed: %v", err)
		return result, err
	}

	// Write manifest
	if err := WriteManifest(req.OutputDir, result); err != nil {
		result.Error = fmt.Sprintf("failed to write manifest: %v", err)
		return result, err
	}

	result.Success = true
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

	// Track if trigger comment has been found to avoid searching again in PR comments
	foundTrigger := false

	// Mark trigger comment if provided
	if req.Options.TriggerCommentID > 0 {
		for i := range reviewThreads {
			if reviewThreads[i].CommentID == req.Options.TriggerCommentID {
				reviewThreads[i].IsTrigger = true
				fmt.Printf("  Marked review thread comment #%d as trigger\n", req.Options.TriggerCommentID)
				foundTrigger = true
				break
			}
			// Also check replies
			for j := range reviewThreads[i].Replies {
				if reviewThreads[i].Replies[j].CommentID == req.Options.TriggerCommentID {
					reviewThreads[i].Replies[j].IsTrigger = true
					fmt.Printf("  Marked review reply #%d as trigger\n", req.Options.TriggerCommentID)
					foundTrigger = true
					break
				}
			}
			if foundTrigger {
				break
			}
		}
	}

	fmt.Printf("  Found %d review threads\n", len(reviewThreads))

	// Fetch PR comments (general discussion, not code reviews)
	fmt.Println("Fetching PR comments...")
	comments, err := p.client.FetchIssueComments(ctx, owner, repo, number)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch PR comments: %w", err)
	}

	// Mark trigger comment in PR comments if not already found in review threads
	if req.Options.TriggerCommentID > 0 && !foundTrigger {
		for i := range comments {
			if comments[i].CommentID == req.Options.TriggerCommentID {
				comments[i].IsTrigger = true
				fmt.Printf("  Marked comment #%d as trigger\n", req.Options.TriggerCommentID)
				break
			}
		}
	}

	fmt.Printf("  Found %d PR comments\n", len(comments))

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

		// Download workflow logs for failed checks
		if len(checkRuns) > 0 {
			if err := p.downloadFailedWorkflowLogs(ctx, checkRuns, req.OutputDir); err != nil {
				fmt.Printf("  Warning: failed to download workflow logs: %v\n", err)
				// Don't fail - logs are optional
			}
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
	files, err := WritePRContext(req.OutputDir, prInfo, reviewThreads, comments, diff, checkRuns, combinedStatus)
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

	// Mark trigger comment if provided
	if req.Options.TriggerCommentID > 0 {
		for i := range comments {
			if comments[i].CommentID == req.Options.TriggerCommentID {
				comments[i].IsTrigger = true
				fmt.Printf("  Marked comment #%d as trigger\n", req.Options.TriggerCommentID)
				break
			}
		}
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

// verifyContextFiles verifies that all required context files are non-empty
// This is a fail-fast check to ensure that missing tokens or network issues
// don't result in silent empty context files being passed to the agent
func verifyContextFiles(outputDir string, kind collector.Kind) error {
	var paths []string

	// Add kind-specific required files
	if kind == collector.KindPR {
		paths = []string{
			filepath.Join(outputDir, "github", "pr.json"),
			filepath.Join(outputDir, "github", "review_threads.json"),
			filepath.Join(outputDir, "github", "review.md"),
			filepath.Join(outputDir, "pr-fix.schema.json"),
		}
	} else if kind == collector.KindIssue {
		paths = []string{
			filepath.Join(outputDir, "github", "issue.json"),
			filepath.Join(outputDir, "github", "comments.json"),
			filepath.Join(outputDir, "github", "issue.md"),
		}
	}

	for _, p := range paths {
		info, err := os.Stat(p)
		if err != nil {
			if os.IsNotExist(err) {
				return fmt.Errorf("context file missing: %s (check token and network connectivity)", p)
			}
			return fmt.Errorf("failed to stat context file %s: %w", p, err)
		}
		if info.Size() == 0 {
			return fmt.Errorf("context file is empty: %s (check token and network connectivity)", p)
		}
	}
	return nil
}

// downloadFailedWorkflowLogs downloads workflow logs for failed check runs
// and saves them to the context directory for agent analysis.
func (p *Provider) downloadFailedWorkflowLogs(ctx context.Context, checkRuns []CheckRun, outputDir string) error {
	githubDir := filepath.Join(outputDir, "github")
	if err := os.MkdirAll(githubDir, 0755); err != nil {
		return fmt.Errorf("failed to create github context directory: %w", err)
	}

	var allLogs []byte
	successfulDownloads := 0

	for _, cr := range checkRuns {
		// Only download logs for failed, timed-out, or action-required checks
		// Matches the filtering logic in CollectPR (line 202)
		if cr.Conclusion != "failure" && cr.Conclusion != "timed_out" && cr.Conclusion != "action_required" {
			continue
		}

		// Skip if no DetailsURL (non-GitHub-Actions checks won't have DetailsURL)
		if cr.DetailsURL == "" {
			continue
		}

		fmt.Printf("  Downloading workflow logs for failed check: %s\n", cr.Name)

		logs, err := p.client.FetchWorkflowLogs(ctx, cr.DetailsURL)
		if err != nil {
			fmt.Printf("  Warning: failed to download logs for %s: %v\n", cr.Name, err)
			// Continue with other checks
			continue
		}

		// Only increment counter after successful download
		successfulDownloads++

		// Append logs with separator
		if len(allLogs) > 0 {
			allLogs = append(allLogs, '\n')
			allLogs = append(allLogs, []byte(strings.Repeat("=", 80))...)
			allLogs = append(allLogs, '\n')
			allLogs = append(allLogs, '\n')
		}
		allLogs = append(allLogs, []byte(fmt.Sprintf("Check: %s\nConclusion: %s\nDetails URL: %s\n\n", cr.Name, cr.Conclusion, cr.DetailsURL))...)
		allLogs = append(allLogs, logs...)
	}

	// Only write the file if we have logs
	if len(allLogs) > 0 {
		logFile := filepath.Join(githubDir, "test-failure-logs.txt")
		if err := os.WriteFile(logFile, allLogs, 0644); err != nil {
			return fmt.Errorf("failed to write test logs: %w", err)
		}
		fmt.Printf("  Saved test failure logs for %d failed check(s) to %s\n", successfulDownloads, logFile)
	}

	return nil
}
