package github

import (
	"context"
	"net/http"
	"time"

	ghhelper "github.com/holon-run/holon/pkg/github"
)

// Client provides methods to fetch GitHub PR and Issue context
type Client struct {
	helper     *ghhelper.Client
	token      string
	baseURL    string
	httpClient *http.Client
}

// NewClient creates a new GitHub API client
func NewClient(token string) *Client {
	helper := ghhelper.NewClient(token,
		ghhelper.WithBaseURL("https://api.github.com"),
		ghhelper.WithTimeout(30*time.Second),
	)

	return &Client{
		helper: helper,
		token:   token,
		baseURL: "https://api.github.com",
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
	}
}

// SetBaseURL sets the base URL for both the client and the helper (for testing)
func (c *Client) SetBaseURL(url string) {
	c.baseURL = url
	// Create a new helper with the new base URL
	c.helper = ghhelper.NewClient(c.token,
		ghhelper.WithBaseURL(url),
		ghhelper.WithTimeout(30*time.Second),
	)
}

// FetchPRInfo fetches basic PR information
func (c *Client) FetchPRInfo(ctx context.Context, owner, repo string, prNumber int) (*PRInfo, error) {
	info, err := c.helper.FetchPRInfo(ctx, owner, repo, prNumber)
	if err != nil {
		return nil, err
	}

	// Convert from helper type to local type
	return &PRInfo{
		Number:      info.Number,
		Title:       info.Title,
		Body:        info.Body,
		State:       info.State,
		URL:         info.URL,
		BaseRef:     info.BaseRef,
		HeadRef:     info.HeadRef,
		BaseSHA:     info.BaseSHA,
		HeadSHA:     info.HeadSHA,
		Author:      info.Author,
		CreatedAt:   info.CreatedAt,
		UpdatedAt:   info.UpdatedAt,
		Repository:  info.Repository,
		MergeCommit: info.MergeCommit,
	}, nil
}

// FetchIssueInfo fetches basic issue information
func (c *Client) FetchIssueInfo(ctx context.Context, owner, repo string, issueNumber int) (*IssueInfo, error) {
	info, err := c.helper.FetchIssueInfo(ctx, owner, repo, issueNumber)
	if err != nil {
		return nil, err
	}

	// Convert from helper type to local type
	return &IssueInfo{
		Number:     info.Number,
		Title:      info.Title,
		Body:       info.Body,
		State:      info.State,
		URL:        info.URL,
		Author:     info.Author,
		Assignee:   info.Assignee,
		CreatedAt:  info.CreatedAt,
		UpdatedAt:  info.UpdatedAt,
		Labels:     info.Labels,
		Repository: info.Repository,
	}, nil
}

// FetchIssueComments fetches comments for an issue
func (c *Client) FetchIssueComments(ctx context.Context, owner, repo string, issueNumber int) ([]IssueComment, error) {
	comments, err := c.helper.FetchIssueComments(ctx, owner, repo, issueNumber)
	if err != nil {
		return nil, err
	}

	// Convert from helper type to local type
	result := make([]IssueComment, len(comments))
	for i, comment := range comments {
		result[i] = IssueComment{
			CommentID: comment.CommentID,
			URL:       comment.URL,
			Body:      comment.Body,
			Author:    comment.Author,
			CreatedAt: comment.CreatedAt,
			UpdatedAt: comment.UpdatedAt,
		}
	}

	return result, nil
}

// FetchReviewThreads fetches review comment threads for a PR
func (c *Client) FetchReviewThreads(ctx context.Context, owner, repo string, prNumber int, unresolvedOnly bool) ([]ReviewThread, error) {
	threads, err := c.helper.FetchReviewThreads(ctx, owner, repo, prNumber, unresolvedOnly)
	if err != nil {
		return nil, err
	}

	// Convert from helper type to local type
	result := make([]ReviewThread, len(threads))
	for i, thread := range threads {
		result[i] = ReviewThread{
			CommentID:   thread.CommentID,
			URL:         thread.URL,
			Path:        thread.Path,
			Line:        thread.Line,
			Side:        thread.Side,
			StartLine:   thread.StartLine,
			StartSide:   thread.StartSide,
			DiffHunk:    thread.DiffHunk,
			Body:        thread.Body,
			Author:      thread.Author,
			CreatedAt:   thread.CreatedAt,
			UpdatedAt:   thread.UpdatedAt,
			Resolved:    thread.Resolved,
			InReplyToID: thread.InReplyToID,
			Position:    thread.Position,
		}

		// Convert replies
		result[i].Replies = make([]Reply, len(thread.Replies))
		for j, reply := range thread.Replies {
			result[i].Replies[j] = Reply{
				CommentID:   reply.CommentID,
				URL:         reply.URL,
				Body:        reply.Body,
				Author:      reply.Author,
				CreatedAt:   reply.CreatedAt,
				UpdatedAt:   reply.UpdatedAt,
				InReplyToID: reply.InReplyToID,
			}
		}
	}

	return result, nil
}

// FetchPRDiff fetches the unified diff for a PR
func (c *Client) FetchPRDiff(ctx context.Context, owner, repo string, prNumber int) (string, error) {
	return c.helper.FetchPRDiff(ctx, owner, repo, prNumber)
}

// FetchCheckRuns fetches check runs for a commit ref
// See: https://docs.github.com/en/rest/checks/runs#list-check-runs-for-a-git-reference
func (c *Client) FetchCheckRuns(ctx context.Context, owner, repo, ref string, maxResults int) ([]CheckRun, error) {
	checkRuns, err := c.helper.FetchCheckRuns(ctx, owner, repo, ref, maxResults)
	if err != nil {
		return nil, err
	}

	// Convert from helper type to local type
	result := make([]CheckRun, len(checkRuns))
	for i, cr := range checkRuns {
		result[i] = CheckRun{
			ID:           cr.ID,
			Name:         cr.Name,
			HeadSHA:      cr.HeadSHA,
			Status:       cr.Status,
			Conclusion:   cr.Conclusion,
			StartedAt:    cr.StartedAt,
			CompletedAt:  cr.CompletedAt,
			DetailsURL:   cr.DetailsURL,
			AppSlug:      cr.AppSlug,
			CheckSuiteID: cr.CheckSuiteID,
			Output: CheckRunOutput{
				Title:   cr.Output.Title,
				Summary: cr.Output.Summary,
				Text:    cr.Output.Text,
			},
		}
	}

	return result, nil
}

// FetchCombinedStatus fetches the combined status for a commit ref
// See: https://docs.github.com/en/rest/commits/statuses#get-the-combined-status-for-a-specific-reference
func (c *Client) FetchCombinedStatus(ctx context.Context, owner, repo, ref string) (*CombinedStatus, error) {
	status, err := c.helper.FetchCombinedStatus(ctx, owner, repo, ref)
	if err != nil {
		return nil, err
	}

	// Convert from helper type to local type
	result := &CombinedStatus{
		SHA:        status.SHA,
		State:      status.State,
		TotalCount: status.TotalCount,
		Statuses:   make([]Status, len(status.Statuses)),
	}

	for i, s := range status.Statuses {
		result.Statuses[i] = Status{
			ID:          s.ID,
			Context:     s.Context,
			State:       s.State,
			TargetURL:   s.TargetURL,
			Description: s.Description,
			CreatedAt:   s.CreatedAt,
			UpdatedAt:   s.UpdatedAt,
		}
	}

	return result, nil
}

// FetchWorkflowLogs downloads workflow logs from GitHub Actions.
// The detailsURL should be the check run's DetailsURL (e.g., "https://github.com/owner/repo/actions/runs/12345/job/67890").
// This function extracts the workflow run ID and uses the GitHub Actions API to download the logs.
func (c *Client) FetchWorkflowLogs(ctx context.Context, detailsURL string) ([]byte, error) {
	return c.helper.FetchWorkflowLogs(ctx, detailsURL)
}
