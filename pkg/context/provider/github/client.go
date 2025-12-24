package github

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// Client provides methods to fetch GitHub PR and Issue context
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
	if t, err := time.Parse(time.RFC3339, prData.UpdatedAt); err == nil {
		updatedAt = t
	}

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

// FetchIssueInfo fetches basic issue information
func (c *Client) FetchIssueInfo(ctx context.Context, owner, repo string, issueNumber int) (*IssueInfo, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/issues/%d", c.baseURL, owner, repo, issueNumber)

	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}

	c.setHeaders(req)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch issue info: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("GitHub API returned status %d: %s", resp.StatusCode, string(body))
	}

	var issueData struct {
		Number    int    `json:"number"`
		Title     string `json:"title"`
		Body      string `json:"body"`
		State     string `json:"state"`
		HTMLURL   string `json:"html_url"`
		CreatedAt string `json:"created_at"`
		UpdatedAt string `json:"updated_at"`
		User      struct {
			Login string `json:"login"`
		} `json:"user"`
		Assignee *struct {
			Login string `json:"login"`
		} `json:"assignee"`
		Labels []struct {
			Name string `json:"name"`
		} `json:"labels"`
		Repository string `json:"repository"`
	}

	if err := json.NewDecoder(resp.Body).Decode(&issueData); err != nil {
		return nil, fmt.Errorf("failed to decode issue data: %w", err)
	}

	var createdAt, updatedAt time.Time
	if t, err := time.Parse(time.RFC3339, issueData.CreatedAt); err == nil {
		createdAt = t
	}
	if t, err := time.Parse(time.RFC3339, issueData.UpdatedAt); err == nil {
		updatedAt = t
	}

	labels := make([]string, len(issueData.Labels))
	for i, label := range issueData.Labels {
		labels[i] = label.Name
	}

	assignee := ""
	if issueData.Assignee != nil {
		assignee = issueData.Assignee.Login
	}

	return &IssueInfo{
		Number:     issueData.Number,
		Title:      issueData.Title,
		Body:       issueData.Body,
		State:      issueData.State,
		URL:        issueData.HTMLURL,
		Author:     issueData.User.Login,
		Assignee:   assignee,
		CreatedAt:  createdAt,
		UpdatedAt:  updatedAt,
		Labels:     labels,
		Repository: issueData.Repository,
	}, nil
}

// FetchIssueComments fetches comments for an issue
func (c *Client) FetchIssueComments(ctx context.Context, owner, repo string, issueNumber int) ([]IssueComment, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/issues/%d/comments", c.baseURL, owner, repo, issueNumber)

	allComments, err := c.fetchAllIssueComments(ctx, url)
	if err != nil {
		return nil, err
	}

	return allComments, nil
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

// fetchAllComments fetches all PR review comments with pagination
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

		if len(comments) < perPage {
			break
		}
		page++
	}

	return allComments, nil
}

// fetchAllIssueComments fetches all issue comments with pagination
func (c *Client) fetchAllIssueComments(ctx context.Context, url string) ([]IssueComment, error) {
	var allComments []IssueComment
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

		var comments []struct {
			ID        int64  `json:"id"`
			HTMLURL   string `json:"html_url"`
			Body      string `json:"body"`
			CreatedAt string `json:"created_at"`
			UpdatedAt string `json:"updated_at"`
			User      struct {
				Login string `json:"login"`
			} `json:"user"`
		}

		if err := json.NewDecoder(resp.Body).Decode(&comments); err != nil {
			resp.Body.Close()
			return nil, fmt.Errorf("failed to decode comments: %w", err)
		}
		resp.Body.Close()

		if len(comments) == 0 {
			break
		}

		for _, comment := range comments {
			var createdAt, updatedAt time.Time
			if t, err := time.Parse(time.RFC3339, comment.CreatedAt); err == nil {
				createdAt = t
			}
			if t, err := time.Parse(time.RFC3339, comment.UpdatedAt); err == nil {
				updatedAt = t
			}

			allComments = append(allComments, IssueComment{
				CommentID: comment.ID,
				URL:       comment.HTMLURL,
				Body:      comment.Body,
				Author:    comment.User.Login,
				CreatedAt: createdAt,
				UpdatedAt: updatedAt,
			})
		}

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
		// Safe type assertion for comment ID
		var commentID int64
		if idVal, ok := comment["id"]; ok && idVal != nil {
			if idFloat, ok := idVal.(float64); ok {
				commentID = int64(idFloat)
			} else {
				continue // Skip if id is not a valid number
			}
		} else {
			continue // Skip if id is missing
		}

		// Safe type assertion for in_reply_to_id
		var inReplyToID int64
		if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
			if replyToFloat, ok := replyTo.(float64); ok {
				inReplyToID = int64(replyToFloat)
			}
		}

		if inReplyToID == 0 {
			thread := c.commentToThread(comment)
			threadMap[commentID] = &thread
			threadIDs = append(threadIDs, commentID)
		}
	}

	// Second pass: add replies to threads
	for _, comment := range comments {
		// Safe type assertion for in_reply_to_id
		var inReplyToID int64
		if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
			if replyToFloat, ok := replyTo.(float64); ok {
				inReplyToID = int64(replyToFloat)
			}
		}

		if inReplyToID != 0 {
			parentThread := c.findParentThread(threadMap, inReplyToID)
			if parentThread != nil {
				reply := c.commentToReply(comment)
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

// findParentThread finds the root thread for a comment
func (c *Client) findParentThread(threadMap map[int64]*ReviewThread, commentID int64) *ReviewThread {
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

// commentToThread converts a GitHub API comment to a ReviewThread
func (c *Client) commentToThread(comment map[string]interface{}) ReviewThread {
	// Extract required fields with safe type assertions
	commentID := int64(0)
	if idVal, ok := comment["id"]; ok && idVal != nil {
		if idFloat, ok := idVal.(float64); ok {
			commentID = int64(idFloat)
		}
	}

	url := ""
	if urlVal, ok := comment["html_url"]; ok && urlVal != nil {
		if urlStr, ok := urlVal.(string); ok {
			url = urlStr
		}
	}

	body := ""
	if bodyVal, ok := comment["body"]; ok && bodyVal != nil {
		if bodyStr, ok := bodyVal.(string); ok {
			body = bodyStr
		}
	}

	diffHunk := ""
	if dh, ok := comment["diff_hunk"]; ok && dh != nil {
		if dhStr, ok := dh.(string); ok {
			diffHunk = dhStr
		}
	}

	path := ""
	if p, ok := comment["path"]; ok && p != nil {
		if pStr, ok := p.(string); ok {
			path = pStr
		}
	}

	var line, startLine, position int
	if l, ok := comment["line"]; ok && l != nil {
		if lFloat, ok := l.(float64); ok {
			line = int(lFloat)
		}
	}
	if sl, ok := comment["start_line"]; ok && sl != nil {
		if slFloat, ok := sl.(float64); ok {
			startLine = int(slFloat)
		}
	}
	if pos, ok := comment["position"]; ok && pos != nil {
		if posFloat, ok := pos.(float64); ok {
			position = int(posFloat)
		}
	}

	side := ""
	if s, ok := comment["side"]; ok && s != nil {
		if sStr, ok := s.(string); ok {
			side = sStr
		}
	}

	startSide := ""
	if ss, ok := comment["start_side"]; ok && ss != nil {
		if ssStr, ok := ss.(string); ok {
			startSide = ssStr
		}
	}

	author := ""
	if user, ok := comment["user"].(map[string]interface{}); ok && user != nil {
		if loginVal, ok := user["login"]; ok && loginVal != nil {
			if loginStr, ok := loginVal.(string); ok {
				author = loginStr
			}
		}
	}

	var createdAt, updatedAt time.Time
	if ca, ok := comment["created_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ca); err == nil {
			createdAt = t
		}
	}
	if ua, ok := comment["updated_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ua); err == nil {
			updatedAt = t
		}
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
		Resolved:  false,
		Position:  position,
		Replies:   []Reply{},
	}
}

// commentToReply converts a GitHub API comment to a Reply
func (c *Client) commentToReply(comment map[string]interface{}) Reply {
	// Extract required fields with safe type assertions
	commentID := int64(0)
	if idVal, ok := comment["id"]; ok && idVal != nil {
		if idFloat, ok := idVal.(float64); ok {
			commentID = int64(idFloat)
		}
	}

	url := ""
	if urlVal, ok := comment["html_url"]; ok && urlVal != nil {
		if urlStr, ok := urlVal.(string); ok {
			url = urlStr
		}
	}

	body := ""
	if bodyVal, ok := comment["body"]; ok && bodyVal != nil {
		if bodyStr, ok := bodyVal.(string); ok {
			body = bodyStr
		}
	}

	author := ""
	if user, ok := comment["user"].(map[string]interface{}); ok && user != nil {
		if loginVal, ok := user["login"]; ok && loginVal != nil {
			if loginStr, ok := loginVal.(string); ok {
				author = loginStr
			}
		}
	}

	var createdAt, updatedAt time.Time
	if ca, ok := comment["created_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ca); err == nil {
			createdAt = t
		}
	}
	if ua, ok := comment["updated_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ua); err == nil {
			updatedAt = t
		}
	}

	var inReplyToID int64
	if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
		if replyToFloat, ok := replyTo.(float64); ok {
			inReplyToID = int64(replyToFloat)
		}
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
