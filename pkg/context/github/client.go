package github

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"time"
)

// Client provides methods to fetch GitHub PR context
type Client struct {
	token      string
	baseURL    string
	httpClient *http.Client
}

// NewClient creates a new GitHub API client
func NewClient(token string) *Client {
	return &Client{
		token:   token,
		baseURL: "https://api.github.com",
		httpClient: &http.Client{
			Timeout: 30 * time.Second,
		},
	}
}

// FetchPRInfo fetches basic PR information
func (c *Client) FetchPRInfo(ctx context.Context, owner, repo string, prNumber int) (*PRInfo, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/pulls/%d", c.baseURL, owner, repo, prNumber)

	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	c.setHeaders(req)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch PR info: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("GitHub API returned status %d: %s", resp.StatusCode, string(body))
	}

	var prData struct {
		Number    int    `json:"number"`
		Title     string `json:"title"`
		Body      string `json:"body"`
		State     string `json:"state"`
		HTMLURL   string `json:"html_url"`
		CreatedAt string `json:"created_at"`
		UpdatedAt string `json:"updated_at"`
		Base      struct {
			Ref  string `json:"ref"`
			SHA  string `json:"sha"`
			Repo struct {
				FullName string `json:"full_name"`
			} `json:"repo"`
		} `json:"base"`
		Head struct {
			Ref string `json:"ref"`
			SHA string `json:"sha"`
		} `json:"head"`
		User struct {
			Login string `json:"login"`
		} `json:"user"`
		MergeCommitSHA string `json:"merge_commit_sha"`
	}

	if err := json.NewDecoder(resp.Body).Decode(&prData); err != nil {
		return nil, fmt.Errorf("failed to decode PR data: %w", err)
	}

	var createdAt, updatedAt time.Time
	if t, err := time.Parse(time.RFC3339, prData.CreatedAt); err == nil {
		createdAt = t
	}
	// If parsing fails, createdAt remains zero time - this is a non-fatal issue
	if t, err := time.Parse(time.RFC3339, prData.UpdatedAt); err == nil {
		updatedAt = t
	}
	// If parsing fails, updatedAt remains zero time - this is a non-fatal issue

	return &PRInfo{
		Number:      prData.Number,
		Title:       prData.Title,
		Body:        prData.Body,
		State:       prData.State,
		URL:         prData.HTMLURL,
		BaseRef:     prData.Base.Ref,
		HeadRef:     prData.Head.Ref,
		BaseSHA:     prData.Base.SHA,
		HeadSHA:     prData.Head.SHA,
		Author:      prData.User.Login,
		CreatedAt:   createdAt,
		UpdatedAt:   updatedAt,
		Repository:  prData.Base.Repo.FullName,
		MergeCommit: prData.MergeCommitSHA,
	}, nil
}

// FetchReviewThreads fetches review comment threads for a PR
func (c *Client) FetchReviewThreads(ctx context.Context, owner, repo string, prNumber int, unresolvedOnly bool) ([]ReviewThread, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/pulls/%d/comments", c.baseURL, owner, repo, prNumber)

	allComments, err := c.fetchAllComments(ctx, url)
	if err != nil {
		return nil, err
	}

	// Group comments by thread (top-level comment + replies)
	threads := c.groupCommentsIntoThreads(allComments)

	// Filter unresolved if requested
	if unresolvedOnly {
		filtered := []ReviewThread{}
		for _, thread := range threads {
			// GraphQL provides resolved status, but REST API doesn't have it directly
			// For now, we'll include all threads and mark them as unresolved
			// A more sophisticated approach would use GraphQL
			if !thread.Resolved {
				filtered = append(filtered, thread)
			}
		}
		threads = filtered
	}

	return threads, nil
}

// FetchPRDiff fetches the unified diff for a PR
func (c *Client) FetchPRDiff(ctx context.Context, owner, repo string, prNumber int) (string, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/pulls/%d", c.baseURL, owner, repo, prNumber)

	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		return "", fmt.Errorf("failed to create request: %w", err)
	}

	// Request diff format
	req.Header.Set("Accept", "application/vnd.github.v3.diff")
	if c.token != "" {
		req.Header.Set("Authorization", "token "+c.token)
	}

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("failed to fetch PR diff: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return "", fmt.Errorf("GitHub API returned status %d: %s", resp.StatusCode, string(body))
	}

	diff, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", fmt.Errorf("failed to read diff: %w", err)
	}

	return string(diff), nil
}

// fetchAllComments fetches all comments with pagination
func (c *Client) fetchAllComments(ctx context.Context, url string) ([]map[string]interface{}, error) {
	var allComments []map[string]interface{}
	page := 1
	perPage := 100

	for {
		pageURL := fmt.Sprintf("%s?page=%d&per_page=%d", url, page, perPage)

		req, err := http.NewRequestWithContext(ctx, "GET", pageURL, nil)
		if err != nil {
			return nil, fmt.Errorf("failed to create request: %w", err)
		}

		c.setHeaders(req)

		resp, err := c.httpClient.Do(req)
		if err != nil {
			return nil, fmt.Errorf("failed to fetch comments: %w", err)
		}

		if resp.StatusCode != http.StatusOK {
			body, _ := io.ReadAll(resp.Body)
			resp.Body.Close()
			return nil, fmt.Errorf("GitHub API returned status %d: %s", resp.StatusCode, string(body))
		}

		var comments []map[string]interface{}
		if err := json.NewDecoder(resp.Body).Decode(&comments); err != nil {
			resp.Body.Close()
			return nil, fmt.Errorf("failed to decode comments: %w", err)
		}
		resp.Body.Close()

		if len(comments) == 0 {
			break
		}

		allComments = append(allComments, comments...)

		// Check if there are more pages
		if len(comments) < perPage {
			break
		}
		page++
	}

	return allComments, nil
}

// groupCommentsIntoThreads groups comments into threads (top-level + replies)
func (c *Client) groupCommentsIntoThreads(comments []map[string]interface{}) []ReviewThread {
	threadMap := make(map[int64]*ReviewThread)
	var threadIDs []int64

	// First pass: create all threads and identify top-level comments
	for _, comment := range comments {
		commentID := int64(comment["id"].(float64))

		// Check if this is a reply
		var inReplyToID int64
		if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
			inReplyToID = int64(replyTo.(float64))
		}

		if inReplyToID == 0 {
			// Top-level comment - create thread
			thread := c.commentToThread(comment)
			threadMap[commentID] = &thread
			threadIDs = append(threadIDs, commentID)
		}
	}

	// Second pass: add replies to threads
	for _, comment := range comments {
		var inReplyToID int64
		if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
			inReplyToID = int64(replyTo.(float64))
		}

		if inReplyToID != 0 {
			// This is a reply - find the parent thread
			parentThread := c.findParentThread(threadMap, inReplyToID)
			if parentThread != nil {
				reply := c.commentToReply(comment)
				parentThread.Replies = append(parentThread.Replies, reply)
			}
		}
	}

	// Convert map to slice
	threads := make([]ReviewThread, 0, len(threadIDs))
	for _, id := range threadIDs {
		threads = append(threads, *threadMap[id])
	}

	return threads
}

// findParentThread finds the root thread for a comment
func (c *Client) findParentThread(threadMap map[int64]*ReviewThread, commentID int64) *ReviewThread {
	// Check if this comment ID is a thread itself
	if thread, ok := threadMap[commentID]; ok {
		return thread
	}

	// Otherwise, iterate through threads to find which one contains this comment as a reply
	for _, thread := range threadMap {
		for _, reply := range thread.Replies {
			if reply.CommentID == commentID {
				return thread
			}
		}
	}

	return nil
}

// commentToThread converts a GitHub API comment to a ReviewThread
func (c *Client) commentToThread(comment map[string]interface{}) ReviewThread {
	commentID := int64(comment["id"].(float64))
	url := comment["html_url"].(string)
	body := comment["body"].(string)
	diffHunk := ""
	if dh, ok := comment["diff_hunk"]; ok && dh != nil {
		diffHunk = dh.(string)
	}

	path := ""
	if p, ok := comment["path"]; ok && p != nil {
		path = p.(string)
	}

	var line, startLine, position int
	if l, ok := comment["line"]; ok && l != nil {
		line = int(l.(float64))
	}
	if sl, ok := comment["start_line"]; ok && sl != nil {
		startLine = int(sl.(float64))
	}
	if pos, ok := comment["position"]; ok && pos != nil {
		position = int(pos.(float64))
	}

	side := ""
	if s, ok := comment["side"]; ok && s != nil {
		side = s.(string)
	}

	startSide := ""
	if ss, ok := comment["start_side"]; ok && ss != nil {
		startSide = ss.(string)
	}

	author := ""
	if user, ok := comment["user"].(map[string]interface{}); ok {
		author = user["login"].(string)
	}

	var createdAt, updatedAt time.Time
	if ca, ok := comment["created_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ca); err == nil {
			createdAt = t
		}
		// If parsing fails, createdAt remains zero time - this is a non-fatal issue
	}
	if ua, ok := comment["updated_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ua); err == nil {
			updatedAt = t
		}
		// If parsing fails, updatedAt remains zero time - this is a non-fatal issue
	}

	return ReviewThread{
		CommentID: commentID,
		URL:       url,
		Path:      path,
		Line:      line,
		Side:      side,
		StartLine: startLine,
		StartSide: startSide,
		DiffHunk:  diffHunk,
		Body:      body,
		Author:    author,
		CreatedAt: createdAt,
		UpdatedAt: updatedAt,
		Resolved:  false, // REST API doesn't provide resolved status
		Position:  position,
		Replies:   []Reply{},
	}
}

// commentToReply converts a GitHub API comment to a Reply
func (c *Client) commentToReply(comment map[string]interface{}) Reply {
	commentID := int64(comment["id"].(float64))
	url := comment["html_url"].(string)
	body := comment["body"].(string)

	author := ""
	if user, ok := comment["user"].(map[string]interface{}); ok {
		author = user["login"].(string)
	}

	var createdAt, updatedAt time.Time
	if ca, ok := comment["created_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ca); err == nil {
			createdAt = t
		}
		// If parsing fails, createdAt remains zero time - this is a non-fatal issue
	}
	if ua, ok := comment["updated_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ua); err == nil {
			updatedAt = t
		}
		// If parsing fails, updatedAt remains zero time - this is a non-fatal issue
	}

	var inReplyToID int64
	if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
		inReplyToID = int64(replyTo.(float64))
	}

	return Reply{
		CommentID:   commentID,
		URL:         url,
		Body:        body,
		Author:      author,
		CreatedAt:   createdAt,
		UpdatedAt:   updatedAt,
		InReplyToID: inReplyToID,
	}
}

// setHeaders sets common headers for GitHub API requests
func (c *Client) setHeaders(req *http.Request) {
	req.Header.Set("Accept", "application/vnd.github.v3+json")
	if c.token != "" {
		req.Header.Set("Authorization", "token "+c.token)
	}
}

// ParseRepoFromURL parses owner and repo from a GitHub URL
func ParseRepoFromURL(repoURL string) (owner, repo string, err error) {
	// Handle formats like:
	// - github.com/owner/repo
	// - https://github.com/owner/repo
	// - owner/repo

	cleaned := strings.TrimPrefix(repoURL, "https://")
	cleaned = strings.TrimPrefix(cleaned, "http://")
	cleaned = strings.TrimPrefix(cleaned, "github.com/")
	cleaned = strings.TrimSuffix(cleaned, ".git")
	cleaned = strings.TrimSuffix(cleaned, "/")

	parts := strings.Split(cleaned, "/")
	if len(parts) < 2 {
		return "", "", fmt.Errorf("invalid repository format: %s", repoURL)
	}

	return parts[0], parts[1], nil
}

// ParsePRNumber parses PR number from various inputs
func ParsePRNumber(input string) (int, error) {
	// Handle formats like:
	// - 123
	// - #123
	// - https://github.com/owner/repo/pull/123

	cleaned := strings.TrimPrefix(input, "#")

	// Check if it's a URL
	if strings.Contains(cleaned, "github.com") {
		parts := strings.Split(cleaned, "/")
		for i, part := range parts {
			if part == "pull" && i+1 < len(parts) {
				cleaned = parts[i+1]
				break
			}
		}
	}

	return strconv.Atoi(cleaned)
}
