package github

import (
	"context"
	"fmt"
	"time"
)

// FetchPRInfo fetches basic pull request information
func (c *Client) FetchPRInfo(ctx context.Context, owner, repo string, prNumber int) (*PRInfo, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/pulls/%d", c.baseURL, owner, repo, prNumber)

	req, err := c.NewRequest(ctx, "GET", url, nil)
	if err != nil {
		return nil, err
	}

	resp, err := c.Do(req, nil)
	if err != nil {
		return nil, err
	}
	defer resp.Close()

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

	if err := resp.DecodeJSON(&prData); err != nil {
		return nil, err
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

	req, err := c.NewRequest(ctx, "GET", url, nil)
	if err != nil {
		return nil, err
	}

	resp, err := c.Do(req, nil)
	if err != nil {
		return nil, err
	}
	// Note: resp.Close() will be called by DecodeJSON via defer

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

	if err := resp.DecodeJSON(&issueData); err != nil {
		return nil, err
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

// FetchIssueComments fetches comments for an issue with pagination
func (c *Client) FetchIssueComments(ctx context.Context, owner, repo string, issueNumber int) ([]IssueComment, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/issues/%d/comments", c.baseURL, owner, repo, issueNumber)

	paginator := NewPaginator(c, DefaultListOptions())
	items, err := paginator.FetchAll(ctx, url)
	if err != nil {
		return nil, err
	}

	comments := make([]IssueComment, len(items))
	for i, item := range items {
		// Type assert to map
		commentMap, ok := item.(map[string]interface{})
		if !ok {
			continue
		}

		comments[i] = parseIssueComment(commentMap)
	}

	return comments, nil
}

// FetchReviewThreads fetches review comment threads for a PR
func (c *Client) FetchReviewThreads(ctx context.Context, owner, repo string, prNumber int, unresolvedOnly bool) ([]ReviewThread, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/pulls/%d/comments", c.baseURL, owner, repo, prNumber)

	paginator := NewPaginator(c, DefaultListOptions())
	items, err := paginator.FetchAll(ctx, url)
	if err != nil {
		return nil, err
	}

	// Convert items to comment maps
	comments := make([]map[string]interface{}, len(items))
	for i, item := range items {
		if commentMap, ok := item.(map[string]interface{}); ok {
			comments[i] = commentMap
		}
	}

	// Group comments into threads
	threads := groupCommentsIntoThreads(comments)

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

	req, err := c.NewRequest(ctx, "GET", url, nil)
	if err != nil {
		return "", err
	}

	// Request diff format
	req.Header.Set("Accept", "application/vnd.github.v3.diff")

	resp, err := c.Do(req, nil)
	if err != nil {
		return "", err
	}
	defer resp.Close()

	diff, err := resp.ReadAll()
	if err != nil {
		return "", fmt.Errorf("failed to read diff: %w", err)
	}

	return string(diff), nil
}

// FetchCheckRuns fetches check runs for a commit ref
func (c *Client) FetchCheckRuns(ctx context.Context, owner, repo, ref string, maxResults int) ([]CheckRun, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/commits/%s/check-runs", c.baseURL, owner, repo, ref)

	paginator := NewPaginator(c, DefaultListOptions())
	paginator.SetMaxResults(maxResults)

	// We need to handle the wrapped response
	var allCheckRuns []CheckRun
	page := 1
	perPage := 100

	for {
		pageURL := fmt.Sprintf("%s?page=%d&per_page=%d", url, page, perPage)

		req, err := c.NewRequest(ctx, "GET", pageURL, nil)
		if err != nil {
			return nil, err
		}

		resp, err := c.Do(req, nil)
		if err != nil {
			return nil, err
		}
		defer resp.Close()

		var response CheckRunsResponse
		if err := resp.DecodeJSON(&response); err != nil {
			return nil, err
		}

		allCheckRuns = append(allCheckRuns, response.CheckRuns...)

		// Check if we've reached max results
		if maxResults > 0 && len(allCheckRuns) >= maxResults {
			allCheckRuns = allCheckRuns[:maxResults]
			break
		}

		// Check if we've fetched all check runs
		if len(response.CheckRuns) < perPage {
			break
		}
		page++
	}

	return allCheckRuns, nil
}

// FetchCombinedStatus fetches the combined status for a commit ref
func (c *Client) FetchCombinedStatus(ctx context.Context, owner, repo, ref string) (*CombinedStatus, error) {
	url := fmt.Sprintf("%s/repos/%s/%s/commits/%s/status", c.baseURL, owner, repo, ref)

	req, err := c.NewRequest(ctx, "GET", url, nil)
	if err != nil {
		return nil, err
	}

	resp, err := c.Do(req, nil)
	if err != nil {
		return nil, err
	}
	defer resp.Close()

	var statusData struct {
		SHA        string `json:"sha"`
		State      string `json:"state"`
		TotalCount int    `json:"total_count"`
		Statuses   []struct {
			ID          int64  `json:"id"`
			Context     string `json:"context"`
			State       string `json:"state"`
			TargetURL   string `json:"target_url,omitempty"`
			Description string `json:"description,omitempty"`
			CreatedAt   string `json:"created_at"`
			UpdatedAt   string `json:"updated_at"`
		} `json:"statuses"`
	}

	if err := resp.DecodeJSON(&statusData); err != nil {
		return nil, err
	}

	// Convert statuses
	statuses := make([]Status, len(statusData.Statuses))
	for i, s := range statusData.Statuses {
		var createdAt, updatedAt time.Time
		if t, err := time.Parse(time.RFC3339, s.CreatedAt); err == nil {
			createdAt = t
		}
		if t, err := time.Parse(time.RFC3339, s.UpdatedAt); err == nil {
			updatedAt = t
		}

		statuses[i] = Status{
			ID:          s.ID,
			Context:     s.Context,
			State:       s.State,
			TargetURL:   s.TargetURL,
			Description: s.Description,
			CreatedAt:   createdAt,
			UpdatedAt:   updatedAt,
		}
	}

	return &CombinedStatus{
		SHA:        statusData.SHA,
		State:      statusData.State,
		TotalCount: statusData.TotalCount,
		Statuses:   statuses,
	}, nil
}

// Helper functions for parsing

func parseIssueComment(comment map[string]interface{}) IssueComment {
	var result IssueComment

	if id, ok := comment["id"].(float64); ok {
		result.CommentID = int64(id)
	}
	if url, ok := comment["html_url"].(string); ok {
		result.URL = url
	}
	if body, ok := comment["body"].(string); ok {
		result.Body = body
	}
	if user, ok := comment["user"].(map[string]interface{}); ok {
		if login, ok := user["login"].(string); ok {
			result.Author = login
		}
	}
	if createdAt, ok := comment["created_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, createdAt); err == nil {
			result.CreatedAt = t
		}
	}
	if updatedAt, ok := comment["updated_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, updatedAt); err == nil {
			result.UpdatedAt = t
		}
	}

	return result
}

func groupCommentsIntoThreads(comments []map[string]interface{}) []ReviewThread {
	threadMap := make(map[int64]*ReviewThread)
	var threadIDs []int64

	// First pass: create all threads and identify top-level comments
	for _, comment := range comments {
		commentID := int64(0)
		if idVal, ok := comment["id"]; ok && idVal != nil {
			if idFloat, ok := idVal.(float64); ok {
				commentID = int64(idFloat)
			}
		}

		if commentID == 0 {
			continue
		}

		var inReplyToID int64
		if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
			if replyToFloat, ok := replyTo.(float64); ok {
				inReplyToID = int64(replyToFloat)
			}
		}

		if inReplyToID == 0 {
			thread := commentToThread(comment)
			threadMap[commentID] = &thread
			threadIDs = append(threadIDs, commentID)
		}
	}

	// Second pass: add replies to threads
	for _, comment := range comments {
		var inReplyToID int64
		if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
			if replyToFloat, ok := replyTo.(float64); ok {
				inReplyToID = int64(replyToFloat)
			}
		}

		if inReplyToID != 0 {
			parentThread := findParentThread(threadMap, inReplyToID)
			if parentThread != nil {
				reply := commentToReply(comment)
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

func findParentThread(threadMap map[int64]*ReviewThread, commentID int64) *ReviewThread {
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

func commentToThread(comment map[string]interface{}) ReviewThread {
	thread := ReviewThread{
		Replies: []Reply{},
	}

	if idVal, ok := comment["id"]; ok && idVal != nil {
		if idFloat, ok := idVal.(float64); ok {
			thread.CommentID = int64(idFloat)
		}
	}

	if urlVal, ok := comment["html_url"]; ok && urlVal != nil {
		if urlStr, ok := urlVal.(string); ok {
			thread.URL = urlStr
		}
	}

	if bodyVal, ok := comment["body"]; ok && bodyVal != nil {
		if bodyStr, ok := bodyVal.(string); ok {
			thread.Body = bodyStr
		}
	}

	if dh, ok := comment["diff_hunk"]; ok && dh != nil {
		if dhStr, ok := dh.(string); ok {
			thread.DiffHunk = dhStr
		}
	}

	if p, ok := comment["path"]; ok && p != nil {
		if pStr, ok := p.(string); ok {
			thread.Path = pStr
		}
	}

	if l, ok := comment["line"]; ok && l != nil {
		if lFloat, ok := l.(float64); ok {
			thread.Line = int(lFloat)
		}
	}

	if sl, ok := comment["start_line"]; ok && sl != nil {
		if slFloat, ok := sl.(float64); ok {
			thread.StartLine = int(slFloat)
		}
	}

	if pos, ok := comment["position"]; ok && pos != nil {
		if posFloat, ok := pos.(float64); ok {
			thread.Position = int(posFloat)
		}
	}

	if s, ok := comment["side"]; ok && s != nil {
		if sStr, ok := s.(string); ok {
			thread.Side = sStr
		}
	}

	if ss, ok := comment["start_side"]; ok && ss != nil {
		if ssStr, ok := ss.(string); ok {
			thread.StartSide = ssStr
		}
	}

	if user, ok := comment["user"].(map[string]interface{}); ok && user != nil {
		if loginVal, ok := user["login"]; ok && loginVal != nil {
			if loginStr, ok := loginVal.(string); ok {
				thread.Author = loginStr
			}
		}
	}

	if ca, ok := comment["created_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ca); err == nil {
			thread.CreatedAt = t
		}
	}

	if ua, ok := comment["updated_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ua); err == nil {
			thread.UpdatedAt = t
		}
	}

	return thread
}

func commentToReply(comment map[string]interface{}) Reply {
	reply := Reply{}

	if idVal, ok := comment["id"]; ok && idVal != nil {
		if idFloat, ok := idVal.(float64); ok {
			reply.CommentID = int64(idFloat)
		}
	}

	if urlVal, ok := comment["html_url"]; ok && urlVal != nil {
		if urlStr, ok := urlVal.(string); ok {
			reply.URL = urlStr
		}
	}

	if bodyVal, ok := comment["body"]; ok && bodyVal != nil {
		if bodyStr, ok := bodyVal.(string); ok {
			reply.Body = bodyStr
		}
	}

	if user, ok := comment["user"].(map[string]interface{}); ok && user != nil {
		if loginVal, ok := user["login"]; ok && loginVal != nil {
			if loginStr, ok := loginVal.(string); ok {
				reply.Author = loginStr
			}
		}
	}

	if ca, ok := comment["created_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ca); err == nil {
			reply.CreatedAt = t
		}
	}

	if ua, ok := comment["updated_at"].(string); ok {
		if t, err := time.Parse(time.RFC3339, ua); err == nil {
			reply.UpdatedAt = t
		}
	}

	if replyTo, ok := comment["in_reply_to_id"]; ok && replyTo != nil {
		if replyToFloat, ok := replyTo.(float64); ok {
			reply.InReplyToID = int64(replyToFloat)
		}
	}

	return reply
}
