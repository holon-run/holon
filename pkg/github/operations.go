package github

import (
	"archive/zip"
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"regexp"
	"strings"

	"github.com/google/go-github/v68/github"
)

// FetchPRInfo fetches basic pull request information using go-github SDK
func (c *Client) FetchPRInfo(ctx context.Context, owner, repo string, prNumber int) (*PRInfo, error) {
	pr, _, err := c.GitHubClient().PullRequests.Get(ctx, owner, repo, prNumber)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch PR: %w", err)
	}

	return convertFromGitHubPR(pr), nil
}

// convertFromGitHubPR converts a github.PullRequest to our PRInfo type
func convertFromGitHubPR(pr *github.PullRequest) *PRInfo {
	// Initialize with empty strings, then populate if base/head are not nil
	var baseRef, headRef, baseSHA, headSHA string

	if base := pr.GetBase(); base != nil {
		baseRef = base.GetRef()
		baseSHA = base.GetSHA()
	}

	if head := pr.GetHead(); head != nil {
		headRef = head.GetRef()
		headSHA = head.GetSHA()
	}

	author := ""
	if user := pr.GetUser(); user != nil {
		author = user.GetLogin()
	}

	info := &PRInfo{
		Number:      pr.GetNumber(),
		Title:       pr.GetTitle(),
		Body:        pr.GetBody(),
		State:       pr.GetState(),
		URL:         pr.GetHTMLURL(),
		BaseRef:     baseRef,
		HeadRef:     headRef,
		BaseSHA:     baseSHA,
		HeadSHA:     headSHA,
		Author:      author,
		CreatedAt:   pr.GetCreatedAt().Time,
		UpdatedAt:   pr.GetUpdatedAt().Time,
		MergeCommit: pr.GetMergeCommitSHA(),
	}

	if pr.GetBase() != nil && pr.GetBase().GetRepo() != nil {
		info.Repository = pr.GetBase().GetRepo().GetFullName()
	}

	return info
}

// FetchIssueInfo fetches basic issue information using go-github SDK
func (c *Client) FetchIssueInfo(ctx context.Context, owner, repo string, issueNumber int) (*IssueInfo, error) {
	issue, _, err := c.GitHubClient().Issues.Get(ctx, owner, repo, issueNumber)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch issue: %w", err)
	}

	return convertFromGitHubIssue(issue), nil
}

// convertFromGitHubIssue converts a github.Issue to our IssueInfo type
func convertFromGitHubIssue(issue *github.Issue) *IssueInfo {
	author := ""
	if user := issue.GetUser(); user != nil {
		author = user.GetLogin()
	}

	info := &IssueInfo{
		Number:    issue.GetNumber(),
		Title:     issue.GetTitle(),
		Body:      issue.GetBody(),
		State:     issue.GetState(),
		URL:       issue.GetHTMLURL(),
		Author:    author,
		CreatedAt: issue.GetCreatedAt().Time,
		UpdatedAt: issue.GetUpdatedAt().Time,
	}

	// Repository may be nil in some API responses
	if issue.GetRepository() != nil {
		info.Repository = issue.GetRepository().GetFullName()
	}

	if issue.GetAssignee() != nil {
		info.Assignee = issue.GetAssignee().GetLogin()
	}

	labels := issue.Labels
	info.Labels = make([]string, len(labels))
	for i, label := range labels {
		info.Labels[i] = label.GetName()
	}

	return info
}

// FetchIssueComments fetches comments for an issue with pagination using go-github SDK
func (c *Client) FetchIssueComments(ctx context.Context, owner, repo string, issueNumber int) ([]IssueComment, error) {
	opts := &github.IssueListCommentsOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	var allComments []IssueComment
	for {
		comments, resp, err := c.GitHubClient().Issues.ListComments(ctx, owner, repo, issueNumber, opts)
		if err != nil {
			return nil, fmt.Errorf("failed to fetch issue comments: %w", err)
		}

		for _, comment := range comments {
			allComments = append(allComments, convertFromGitHubIssueComment(comment))
		}

		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
	}

	return allComments, nil
}

// convertFromGitHubIssueComment converts a github.IssueComment to our IssueComment type
func convertFromGitHubIssueComment(comment *github.IssueComment) IssueComment {
	author := ""
	if user := comment.GetUser(); user != nil {
		author = user.GetLogin()
	}

	return IssueComment{
		CommentID: comment.GetID(),
		URL:       comment.GetHTMLURL(),
		Body:      comment.GetBody(),
		Author:    author,
		CreatedAt: comment.GetCreatedAt().Time,
		UpdatedAt: comment.GetUpdatedAt().Time,
	}
}

// FetchReviewThreads fetches review comment threads for a PR using go-github SDK
func (c *Client) FetchReviewThreads(ctx context.Context, owner, repo string, prNumber int, unresolvedOnly bool) ([]ReviewThread, error) {
	opts := &github.PullRequestListCommentsOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	var allComments []*github.PullRequestComment
	for {
		comments, resp, err := c.GitHubClient().PullRequests.ListComments(ctx, owner, repo, prNumber, opts)
		if err != nil {
			return nil, fmt.Errorf("failed to fetch review comments: %w", err)
		}

		allComments = append(allComments, comments...)

		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
	}

	// Group comments into threads
	threads := groupGitHubCommentsIntoThreads(allComments)

	// Filter unresolved if requested
	if unresolvedOnly {
		filtered := []ReviewThread{}
		for _, thread := range threads {
			if !thread.Resolved {
				filtered = append(filtered, thread)
			}
		}
		threads = filtered
	}

	return threads, nil
}

// groupGitHubCommentsIntoThreads groups github.PullRequestComment into ReviewThread
func groupGitHubCommentsIntoThreads(comments []*github.PullRequestComment) []ReviewThread {
	threadMap := make(map[int64]*ReviewThread)
	var threadIDs []int64

	// First pass: create all threads and identify top-level comments
	for _, comment := range comments {
		commentID := comment.GetID()
		if commentID == 0 {
			continue
		}

		// Top-level comments don't have InReplyTo
		if comment.GetInReplyTo() == 0 {
			thread := convertFromGitHubPullRequestComment(comment)
			threadMap[commentID] = &thread
			threadIDs = append(threadIDs, commentID)
		}
	}

	// Second pass: add replies to threads
	for _, comment := range comments {
		inReplyToID := comment.GetInReplyTo()
		if inReplyToID != 0 {
			parentThread := findParentThreadInMap(threadMap, inReplyToID)
			if parentThread != nil {
				reply := convertFromGitHubReplyComment(comment)
				parentThread.Replies = append(parentThread.Replies, reply)
			}
		}
	}

	threads := make([]ReviewThread, 0, len(threadIDs))
	for _, id := range threadIDs {
		threads = append(threads, *threadMap[id])
	}

	return threads
}

// findParentThreadInMap finds a thread by comment ID or by checking reply chains
func findParentThreadInMap(threadMap map[int64]*ReviewThread, commentID int64) *ReviewThread {
	if thread, ok := threadMap[commentID]; ok {
		return thread
	}

	for _, thread := range threadMap {
		for _, reply := range thread.Replies {
			if reply.CommentID == commentID {
				return thread
			}
		}
	}

	return nil
}

// convertFromGitHubPullRequestComment converts a github.PullRequestComment to our ReviewThread type
func convertFromGitHubPullRequestComment(comment *github.PullRequestComment) ReviewThread {
	author := ""
	if user := comment.GetUser(); user != nil {
		author = user.GetLogin()
	}

	thread := ReviewThread{
		CommentID: comment.GetID(),
		URL:       comment.GetHTMLURL(),
		Path:      comment.GetPath(),
		Body:      comment.GetBody(),
		DiffHunk:  comment.GetDiffHunk(),
		Line:      comment.GetLine(),
		Author:    author,
		CreatedAt: comment.GetCreatedAt().Time,
		UpdatedAt: comment.GetUpdatedAt().Time,
		Replies:   []Reply{},
	}

	if comment.StartLine != nil {
		thread.StartLine = comment.GetStartLine()
	}
	if comment.Side != nil {
		thread.Side = comment.GetSide()
	}
	if comment.StartSide != nil {
		thread.StartSide = comment.GetStartSide()
	}
	if comment.Position != nil {
		thread.Position = comment.GetPosition()
	}

	return thread
}

// convertFromGitHubReplyComment converts a github.PullRequestComment to our Reply type
func convertFromGitHubReplyComment(comment *github.PullRequestComment) Reply {
	author := ""
	if user := comment.GetUser(); user != nil {
		author = user.GetLogin()
	}

	return Reply{
		CommentID:   comment.GetID(),
		URL:         comment.GetHTMLURL(),
		Body:        comment.GetBody(),
		Author:      author,
		CreatedAt:   comment.GetCreatedAt().Time,
		UpdatedAt:   comment.GetUpdatedAt().Time,
		InReplyToID: comment.GetInReplyTo(),
	}
}

// FetchPRDiff fetches the unified diff for a PR using go-github SDK
func (c *Client) FetchPRDiff(ctx context.Context, owner, repo string, prNumber int) (string, error) {
	// Use go-github client with raw Accept header for diff format
	client := c.GitHubClient()

	// Get the raw response with diff media type
	req, err := client.NewRequest("GET", fmt.Sprintf("repos/%s/%s/pulls/%d", owner, repo, prNumber), nil)
	if err != nil {
		return "", fmt.Errorf("failed to create request: %w", err)
	}

	// Request diff format
	req.Header.Set("Accept", "application/vnd.github.v3.diff")

	resp, err := client.Do(ctx, req, nil)
	if err != nil {
		return "", fmt.Errorf("failed to fetch PR diff: %w", err)
	}
	defer resp.Body.Close()

	// Read the diff body
	diffBytes, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("failed to read diff response: %w", err)
	}

	return string(diffBytes), nil
}

// FetchCheckRuns fetches check runs for a commit ref using go-github SDK
func (c *Client) FetchCheckRuns(ctx context.Context, owner, repo, ref string, maxResults int) ([]CheckRun, error) {
	opts := &github.ListCheckRunsOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	var allCheckRuns []CheckRun
	fetched := 0

PaginationLoop:
	for {
		checkRuns, resp, err := c.GitHubClient().Checks.ListCheckRunsForRef(ctx, owner, repo, ref, opts)
		if err != nil {
			return nil, fmt.Errorf("failed to fetch check runs: %w", err)
		}

		// Check for nil response
		if checkRuns == nil {
			break
		}

		for _, cr := range checkRuns.CheckRuns {
			// Check for nil check runs
			if cr == nil {
				continue
			}
			// Check max results
			if maxResults > 0 && fetched >= maxResults {
				break PaginationLoop
			}
			allCheckRuns = append(allCheckRuns, convertFromGitHubCheckRun(cr))
			fetched++
		}

		// Check if we've reached max results
		if maxResults > 0 && fetched >= maxResults {
			break
		}

		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
	}

	return allCheckRuns, nil
}

// convertFromGitHubCheckRun converts a github.CheckRun to our CheckRun type
func convertFromGitHubCheckRun(cr *github.CheckRun) CheckRun {
	checkRun := CheckRun{
		ID:         cr.GetID(),
		Name:       cr.GetName(),
		HeadSHA:    cr.GetHeadSHA(),
		Status:     cr.GetStatus(),
		Conclusion: cr.GetConclusion(),
		DetailsURL: cr.GetDetailsURL(),
	}

	if cr.CheckSuite != nil {
		checkRun.CheckSuiteID = cr.GetCheckSuite().GetID()
	}

	// Handle StartedAt and CompletedAt using GetTime() which returns nil for zero timestamps
	if t := cr.StartedAt.GetTime(); t != nil {
		checkRun.StartedAt = t
	}
	if t := cr.CompletedAt.GetTime(); t != nil {
		checkRun.CompletedAt = t
	}
	if cr.GetApp() != nil {
		checkRun.AppSlug = cr.GetApp().GetSlug()
	}
	if cr.GetOutput() != nil {
		checkRun.Output = CheckRunOutput{
			Title:   cr.GetOutput().GetTitle(),
			Summary: cr.GetOutput().GetSummary(),
			Text:    cr.GetOutput().GetText(),
		}
	}

	return checkRun
}

// FetchCombinedStatus fetches the combined status for a commit ref using go-github SDK
func (c *Client) FetchCombinedStatus(ctx context.Context, owner, repo, ref string) (*CombinedStatus, error) {
	combinedStatus, _, err := c.GitHubClient().Repositories.GetCombinedStatus(ctx, owner, repo, ref, &github.ListOptions{PerPage: 100})
	if err != nil {
		return nil, fmt.Errorf("failed to fetch combined status: %w", err)
	}

	return convertFromGitHubCombinedStatus(combinedStatus), nil
}

// convertFromGitHubCombinedStatus converts a github.CombinedStatus to our CombinedStatus type
func convertFromGitHubCombinedStatus(cs *github.CombinedStatus) *CombinedStatus {
	statuses := make([]Status, len(cs.Statuses))
	for i, s := range cs.Statuses {
		statuses[i] = Status{
			ID:          s.GetID(),
			Context:     s.GetContext(),
			State:       s.GetState(),
			TargetURL:   s.GetTargetURL(),
			Description: s.GetDescription(),
			CreatedAt:   s.GetCreatedAt().Time,
			UpdatedAt:   s.GetUpdatedAt().Time,
		}
	}

	return &CombinedStatus{
		SHA:        cs.GetSHA(),
		State:      cs.GetState(),
		TotalCount: cs.GetTotalCount(),
		Statuses:   statuses,
	}
}

// CreateIssueComment creates a new comment on an issue or PR
func (c *Client) CreateIssueComment(ctx context.Context, owner, repo string, issueNumber int, body string) (int64, error) {
	comment, _, err := c.GitHubClient().Issues.CreateComment(ctx, owner, repo, issueNumber, &github.IssueComment{Body: &body})
	if err != nil {
		return 0, fmt.Errorf("failed to create issue comment: %w", err)
	}
	return comment.GetID(), nil
}

// EditIssueComment edits an existing issue or PR comment
func (c *Client) EditIssueComment(ctx context.Context, owner, repo string, commentID int64, body string) error {
	_, _, err := c.GitHubClient().Issues.EditComment(ctx, owner, repo, commentID, &github.IssueComment{Body: &body})
	if err != nil {
		return fmt.Errorf("failed to edit issue comment: %w", err)
	}
	return nil
}

// CreateIssue creates a new GitHub issue
func (c *Client) CreateIssue(ctx context.Context, owner, repo, title, body string, labels []string) (string, error) {
	req := &github.IssueRequest{
		Title:  &title,
		Body:   &body,
		Labels: &labels,
	}

	issue, _, err := c.GitHubClient().Issues.Create(ctx, owner, repo, req)
	if err != nil {
		return "", fmt.Errorf("failed to create issue: %w", err)
	}

	return issue.GetHTMLURL(), nil
}

// ListIssueComments lists all issue/PR comments with pagination
func (c *Client) ListIssueComments(ctx context.Context, owner, repo string, issueNumber int) ([]IssueComment, error) {
	return c.FetchIssueComments(ctx, owner, repo, issueNumber)
}

// CreatePullRequestComment creates a reply to a review comment
func (c *Client) CreatePullRequestComment(ctx context.Context, owner, repo string, prNumber int, body string, inReplyTo int64) (int64, error) {
	createdComment, _, err := c.GitHubClient().PullRequests.CreateCommentInReplyTo(ctx, owner, repo, prNumber, body, inReplyTo)
	if err != nil {
		return 0, fmt.Errorf("failed to create pull request comment: %w", err)
	}
	return createdComment.GetID(), nil
}

// ListPullRequestComments lists all PR review comments with pagination
func (c *Client) ListPullRequestComments(ctx context.Context, owner, repo string, prNumber int) ([]*github.PullRequestComment, error) {
	opts := &github.PullRequestListCommentsOptions{
		ListOptions: github.ListOptions{PerPage: 100},
	}

	var allComments []*github.PullRequestComment
	for {
		comments, resp, err := c.GitHubClient().PullRequests.ListComments(ctx, owner, repo, prNumber, opts)
		if err != nil {
			return nil, fmt.Errorf("failed to list pull request comments: %w", err)
		}

		allComments = append(allComments, comments...)

		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
	}

	return allComments, nil
}

// CreatePullRequest creates a new pull request
func (c *Client) CreatePullRequest(ctx context.Context, owner, repo string, newPR *NewPullRequest) (*PRInfo, error) {
	pr, _, err := c.GitHubClient().PullRequests.Create(ctx, owner, repo, &github.NewPullRequest{
		Title:               &newPR.Title,
		Head:                &newPR.Head,
		Base:                &newPR.Base,
		Body:                &newPR.Body,
		MaintainerCanModify: github.Bool(newPR.MaintainerCanModify),
	})
	if err != nil {
		return nil, fmt.Errorf("failed to create pull request: %w", err)
	}
	return convertFromGitHubPR(pr), nil
}

// UpdatePullRequest updates an existing pull request
func (c *Client) UpdatePullRequest(ctx context.Context, owner, repo string, prNumber int, title, body string) (*PRInfo, error) {
	pr, _, err := c.GitHubClient().PullRequests.Edit(ctx, owner, repo, prNumber, &github.PullRequest{
		Title: &title,
		Body:  &body,
	})
	if err != nil {
		return nil, fmt.Errorf("failed to update pull request: %w", err)
	}
	return convertFromGitHubPR(pr), nil
}

// ListPullRequests lists all pull requests with optional filters
func (c *Client) ListPullRequests(ctx context.Context, owner, repo string, state string) ([]*PRInfo, error) {
	opts := &github.PullRequestListOptions{
		State: state,
		ListOptions: github.ListOptions{
			PerPage: 100,
		},
	}

	var allPRs []*PRInfo
	for {
		prs, resp, err := c.GitHubClient().PullRequests.List(ctx, owner, repo, opts)
		if err != nil {
			return nil, fmt.Errorf("failed to list pull requests: %w", err)
		}

		// Convert each PR to PRInfo using the existing conversion function
		for _, pr := range prs {
			allPRs = append(allPRs, convertFromGitHubPR(pr))
		}

		if resp.NextPage == 0 {
			break
		}
		opts.Page = resp.NextPage
	}

	return allPRs, nil
}

// GetCurrentUser retrieves the authenticated user's identity information
// Returns ActorInfo with login and type (User or App)
// Handles both PAT/user tokens (via /user endpoint) and GitHub App tokens (via /app endpoint)
// Returns nil if the request fails (non-critical operation)
func (c *Client) GetCurrentUser(ctx context.Context) (*ActorInfo, error) {
	// First, try the /user endpoint (works for PAT and user tokens)
	user, resp, err := c.GitHubClient().Users.Get(ctx, "")
	if err == nil {
		info := &ActorInfo{
			Login:  user.GetLogin(),
			Type:   user.GetType(),
			Source: "token",
		}

		// For GitHub Apps, try to get the app slug
		if user.GetType() == "Bot" && info.Login != "" {
			// Bot usernames end with "[bot]", extract the app slug
			// e.g., "github-actions[bot]" -> "github-actions"
			if idx := strings.Index(info.Login, "[bot]"); idx > 0 {
				info.AppSlug = info.Login[:idx]
				info.Type = "App"
			}
		}

		return info, nil
	}

	// Check if this is a 403 error indicating an App installation token
	// GitHub App tokens get "403 Resource not accessible by integration" when calling /user
	if resp != nil && resp.StatusCode == 403 {
		// This is likely an App installation token, try the /app endpoint
		return c.getCurrentApp(ctx)
	}

	// For other errors, return the error
	return nil, fmt.Errorf("failed to get current user: %w", err)
}

// getCurrentApp retrieves the authenticated GitHub App's identity information
// Called as a fallback when /user returns 403 for App installation tokens
func (c *Client) getCurrentApp(ctx context.Context) (*ActorInfo, error) {
	app, resp, err := c.GitHubClient().Apps.Get(ctx, "")
	if err != nil {
		// If /app also fails, return a minimal ActorInfo for App tokens
		// This handles the case where we have an App installation token but can't call /app
		if resp != nil && resp.StatusCode == 403 {
			// Return minimal ActorInfo for App installation token
			return &ActorInfo{
				Type:   "App",
				Source: "app",
			}, nil
		}
		return nil, fmt.Errorf("failed to get current app: %w", err)
	}

	info := &ActorInfo{
		Login:   app.GetSlug(),
		Type:    "App",
		Source:  "app",
		AppSlug: app.GetSlug(),
	}

	return info, nil
}

// extractZipFiles extracts all text files from a ZIP archive.
// Returns concatenated text content with file name separators.
func extractZipFiles(zipData []byte) ([]byte, error) {
	reader, err := zip.NewReader(bytes.NewReader(zipData), int64(len(zipData)))
	if err != nil {
		return nil, fmt.Errorf("failed to open ZIP archive: %w", err)
	}

	var allLogs []byte
	for _, file := range reader.File {
		if file.FileInfo().IsDir() {
			continue
		}

		rc, err := file.Open()
		if err != nil {
			continue // Skip files that can't be opened
		}

		content, err := io.ReadAll(rc)
		rc.Close()

		if err != nil {
			continue // Skip files that can't be read
		}

		// Add file name as separator
		allLogs = append(allLogs, []byte(fmt.Sprintf("\n=== %s ===\n", file.Name))...)
		allLogs = append(allLogs, content...)
		allLogs = append(allLogs, '\n')
	}

	return allLogs, nil
}

// FetchWorkflowLogs downloads logs for a GitHub Actions workflow run.
// The detailsURL should be the check run's DetailsURL (e.g., "https://github.com/owner/repo/actions/runs/12345/job/67890").
// This function extracts the workflow run ID and uses the GitHub Actions API to download the logs.
// Returns the log content as bytes (ZIP archive contents).
func (c *Client) FetchWorkflowLogs(ctx context.Context, detailsURL string) ([]byte, error) {
	if detailsURL == "" {
		return nil, fmt.Errorf("details URL is empty")
	}

	// Extract workflow run ID from DetailsURL
	// Format: https://github.com/owner/repo/actions/runs/{run_id}/job/{job_id}
	runIDRegex := regexp.MustCompile(`/actions/runs/(\d+)`)
	matches := runIDRegex.FindStringSubmatch(detailsURL)
	if len(matches) < 2 {
		return nil, fmt.Errorf("failed to extract workflow run ID from details URL: %s", detailsURL)
	}
	runID := matches[1]

	// Extract owner and repo from details URL
	// Format: https://github.com/{owner}/{repo}/actions/runs/...
	parts := strings.Split(strings.TrimPrefix(detailsURL, "https://github.com/"), "/")
	if len(parts) < 2 {
		return nil, fmt.Errorf("failed to extract owner/repo from details URL: %s", detailsURL)
	}
	owner, repo := parts[0], parts[1]

	// Construct GitHub Actions API endpoint for workflow run logs
	apiURL := fmt.Sprintf("%s/repos/%s/%s/actions/runs/%s/logs", c.baseURL, owner, repo, runID)

	// Create HTTP request to the logs API endpoint
	req, err := http.NewRequestWithContext(ctx, "GET", apiURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	// Set authentication headers (use "token" not "Bearer" for GitHub API)
	if c.token != "" {
		req.Header.Set("Authorization", "token "+c.token)
	}
	req.Header.Set("Accept", "application/vnd.github.v3+json")

	// Make request to the logs API endpoint
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to request logs: %w", err)
	}
	defer resp.Body.Close()

	// Handle successful response directly (unlikely for logs endpoint, but possible)
	if resp.StatusCode == http.StatusOK {
		logs, err := io.ReadAll(resp.Body)
		if err != nil {
			return nil, fmt.Errorf("failed to read logs: %w", err)
		}
		// Auto-extract ZIP archives
		if bytes.HasPrefix(logs, []byte("PK")) {
			extractedLogs, err := extractZipFiles(logs)
			if err != nil {
				// If extraction fails, log warning but return original data
				fmt.Printf("Warning: failed to extract ZIP logs: %v\n", err)
				return logs, nil
			}
			return extractedLogs, nil
		}
		return logs, nil
	}

	// Handle redirect responses (GitHub Actions logs redirect to pre-signed URL)
	if resp.StatusCode == http.StatusFound || resp.StatusCode == http.StatusTemporaryRedirect {
		redirectURL := resp.Header.Get("Location")
		if redirectURL == "" {
			body, _ := io.ReadAll(resp.Body)
			return nil, fmt.Errorf("redirect status %d without Location header (body: %s)", resp.StatusCode, string(body))
		}

		// Follow redirect to download logs from pre-signed URL
		// Do not forward the Authorization header to avoid leaking tokens to third-party hosts
		redirectReq, err := http.NewRequestWithContext(ctx, "GET", redirectURL, nil)
		if err != nil {
			return nil, fmt.Errorf("failed to create redirect request: %w", err)
		}

		redirectResp, err := c.httpClient.Do(redirectReq)
		if err != nil {
			return nil, fmt.Errorf("failed to follow redirect for logs download: %w", err)
		}
		defer redirectResp.Body.Close()

		if redirectResp.StatusCode != http.StatusOK {
			body, _ := io.ReadAll(redirectResp.Body)
			return nil, fmt.Errorf("failed to download logs from redirect URL: HTTP %d (body: %s)", redirectResp.StatusCode, string(body))
		}

		logs, err := io.ReadAll(redirectResp.Body)
		if err != nil {
			return nil, fmt.Errorf("failed to read logs from redirect response: %w", err)
		}

		// Auto-extract ZIP archives
		if bytes.HasPrefix(logs, []byte("PK")) {
			extractedLogs, err := extractZipFiles(logs)
			if err != nil {
				// If extraction fails, log warning but return original data
				fmt.Printf("Warning: failed to extract ZIP logs: %v\n", err)
				return logs, nil
			}
			return extractedLogs, nil
		}

		return logs, nil
	}

	// All other non-OK responses are treated as errors
	body, _ := io.ReadAll(resp.Body)
	return nil, fmt.Errorf("failed to download logs: HTTP %d (body: %s)", resp.StatusCode, string(body))
}
