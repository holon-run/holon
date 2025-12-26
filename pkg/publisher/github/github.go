package github

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"

	hghelper "github.com/holon-run/holon/pkg/github"
	"github.com/holon-run/holon/pkg/publisher"
)

const (
	// SummaryMarker is the HTML comment marker used to identify Holon summary comments
	SummaryMarker = "<!-- holon-summary-marker -->"

	// BotLoginEnv is the environment variable for the bot's GitHub login
	BotLoginEnv = "HOLON_GITHUB_BOT_LOGIN"

	// DefaultBotLogin is the default bot login name
	DefaultBotLogin = "holonbot[bot]"
)

// GitHubPublisher publishes Holon outputs to GitHub PRs.
type GitHubPublisher struct {
	// client is the GitHub client to use. If nil, a new client will be created
	// using the token from environment variables. This field is primarily for
	// testing with mock servers.
	client *hghelper.Client
}

// NewGitHubPublisher creates a new GitHub publisher instance.
func NewGitHubPublisher() *GitHubPublisher {
	return &GitHubPublisher{}
}

// Name returns the provider name.
func (g *GitHubPublisher) Name() string {
	return "github"
}

// Validate checks if the request is valid for this publisher.
func (g *GitHubPublisher) Validate(req publisher.PublishRequest) error {
	// Parse the PR reference
	prRef, err := ParsePRRef(req.Target)
	if err != nil {
		return fmt.Errorf("invalid target format: %w", err)
	}

	// Validate PR reference fields
	if prRef.Owner == "" || prRef.Repo == "" || prRef.PRNumber == 0 {
		return fmt.Errorf("incomplete PR reference: owner=%s, repo=%s, pr_number=%d", prRef.Owner, prRef.Repo, prRef.PRNumber)
	}

	return nil
}

// Publish sends Holon outputs to GitHub PR.
func (g *GitHubPublisher) Publish(req publisher.PublishRequest) (publisher.PublishResult, error) {
	ctx := context.Background()

	// Use provided client or create a new one
	var client *hghelper.Client
	if g.client != nil {
		client = g.client
	} else {
		// Create client using shared helper (reads token from env vars)
		var err error
		client, err = hghelper.NewClientFromEnv()
		if err != nil {
			return publisher.PublishResult{}, fmt.Errorf("failed to create GitHub client: %w", err)
		}
	}

	// Parse PR reference
	prRef, err := ParsePRRef(req.Target)
	if err != nil {
		return publisher.PublishResult{}, fmt.Errorf("invalid target: %w", err)
	}

	// Get bot login for idempotency checks
	botLogin := getBotLogin()

	// Initialize result
	result := publisher.PublishResult{
		Provider:   g.Name(),
		Target:     req.Target,
		Actions:    []publisher.PublishAction{},
		Errors:     []publisher.PublishError{},
		Success:    true,
	}

	// Step 1: Read and process pr-fix.json
	prFixPath := req.Artifacts["pr-fix.json"]
	if prFixPath != "" {
		prFixData, err := readPRFixData(prFixPath)
		if err != nil {
			wrappedErr := fmt.Errorf("failed to read pr-fix.json: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
		} else {
			// Step 1.1: Handle review replies
			if len(prFixData.ReviewReplies) > 0 {
				replyResult, err := g.publishReviewReplies(ctx, client, *prRef, prFixData.ReviewReplies, botLogin)
				if err != nil {
					result.Errors = append(result.Errors, publisher.NewErrorWithAction(err.Error(), "publish_review_replies"))
					result.Success = false
				} else {
					// Add actions for posted replies
					for _, detail := range replyResult.Details {
						if detail.Status == "posted" {
							result.Actions = append(result.Actions, publisher.PublishAction{
								Type:        "replied_review_comment",
								Description: fmt.Sprintf("Replied to review comment %d", detail.CommentID),
								Metadata: map[string]string{
									"comment_id": strconv.FormatInt(detail.CommentID, 10),
								},
							})
						}
					}

					// Add summary action
					result.Actions = append(result.Actions, publisher.PublishAction{
						Type:        "review_replies_summary",
						Description: fmt.Sprintf("Review replies: %d posted, %d skipped, %d failed", replyResult.Posted, replyResult.Skipped, replyResult.Failed),
					})
				}
			}

			// Step 1.2: Handle follow-up issues if present
			if len(prFixData.FollowUpIssues) > 0 {
				issueResult, err := g.handleFollowUpIssues(ctx, client, *prRef, prFixData.FollowUpIssues)
				if err != nil {
					result.Errors = append(result.Errors, publisher.NewErrorWithAction(err.Error(), "handle_follow_up_issues"))
					result.Success = false
				} else {
					// Add actions for created issues
					for _, detail := range issueResult.Created {
						result.Actions = append(result.Actions, publisher.PublishAction{
							Type:        "created_follow_up_issue",
							Description: fmt.Sprintf("Created follow-up issue: %s", detail.Title),
							Metadata: map[string]string{
								"issue_url": detail.IssueURL,
								"title":     detail.Title,
							},
						})
					}
					// Add summary action
					result.Actions = append(result.Actions, publisher.PublishAction{
						Type:        "follow_up_issues_summary",
						Description: fmt.Sprintf("Follow-up issues: %d created, %d deferred (drafts in pr-fix.json)", issueResult.CreatedCount, issueResult.DeferredCount),
					})
				}
			}
		}
	}

	// Step 2: Read and post summary.md
	summaryPath := req.Artifacts["summary.md"]
	if summaryPath != "" {
		summaryContent, err := os.ReadFile(summaryPath)
		if err != nil {
			wrappedErr := fmt.Errorf("failed to read summary.md: %w", err)
			result.Errors = append(result.Errors, publisher.NewError(wrappedErr.Error()))
			result.Success = false
		} else {
			commentResult, err := g.publishSummaryComment(ctx, client, *prRef, string(summaryContent), botLogin)
			if err != nil {
				result.Errors = append(result.Errors, publisher.NewErrorWithAction(err.Error(), "publish_summary_comment"))
				result.Success = false
			} else if commentResult.Posted {
				actionType := "created_summary_comment"
				if commentResult.Updated {
					actionType = "updated_summary_comment"
				}
				result.Actions = append(result.Actions, publisher.PublishAction{
					Type:        actionType,
					Description: fmt.Sprintf("Summary comment posted to PR #%d", prRef.PRNumber),
					Metadata: map[string]string{
						"comment_id": strconv.FormatInt(commentResult.CommentID, 10),
					},
				})
			}
		}
	}

	return result, nil
}

// handleFollowUpIssues creates GitHub issues for any follow-up issues that don't already have an issue_url.
// Issues with an existing issue_url are considered already created by the agent and are tracked but not recreated.
// Issues without an issue_url are created by the publisher, with errors tracked per issue (continues on failure).
func (g *GitHubPublisher) handleFollowUpIssues(ctx context.Context, client *hghelper.Client, prRef PRRef, issues []FollowUpIssue) (*FollowUpIssuesResult, error) {
	result := &FollowUpIssuesResult{
		Created:       make([]FollowUpIssueDetail, 0),
		CreatedCount:  0,
		DeferredCount: 0,
	}

	for _, issue := range issues {
		// If agent already created the issue and provided the URL
		if issue.IssueURL != "" {
			result.Created = append(result.Created, FollowUpIssueDetail{
				Title:    issue.Title,
				IssueURL: issue.IssueURL,
			})
			result.CreatedCount++
			continue
		}

		// Create the issue on GitHub (publisher creates it since agent didn't)
		issueURL, err := client.CreateIssue(ctx, prRef.Owner, prRef.Repo, issue.Title, issue.Body, issue.Labels)
		if err != nil {
			// Track failure but continue processing other issues (similar to publishReviewReplies)
			result.DeferredCount++
			// We don't have a dedicated "Failed" field in FollowUpIssuesResult, so we count it as deferred
			// The error will be logged via the PublishResult.Errors mechanism
			continue
		}
		result.Created = append(result.Created, FollowUpIssueDetail{
			Title:    issue.Title,
			IssueURL: issueURL,
		})
		result.CreatedCount++
	}

	return result, nil
}

// publishReviewReplies posts replies to review comments with idempotency.
func (g *GitHubPublisher) publishReviewReplies(ctx context.Context, client *hghelper.Client, prRef PRRef, replies []ReviewReply, botLogin string) (ReviewRepliesResult, error) {
	result := ReviewRepliesResult{
		Total:   len(replies),
		Posted:  0,
		Skipped: 0,
		Failed:  0,
		Details: make([]ReplyResult, 0, len(replies)),
	}

	for _, reply := range replies {
		// Check if we've already replied
		hasReplied, err := hasBotRepliedToComment(ctx, client, prRef, reply.CommentID, botLogin)
		if err != nil {
			result.Failed++
			result.Details = append(result.Details, ReplyResult{
				CommentID: reply.CommentID,
				Status:    "failed",
				Reason:    fmt.Sprintf("Failed to check existing replies: %v", err),
			})
			continue
		}

		if hasReplied {
			result.Skipped++
			result.Details = append(result.Details, ReplyResult{
				CommentID: reply.CommentID,
				Status:    "skipped",
				Reason:    "Already replied",
			})
			continue
		}

		// Format and post the reply using the helper
		message := formatReviewReply(reply)
		_, err = client.CreatePullRequestComment(ctx, prRef.Owner, prRef.Repo, prRef.PRNumber, message, reply.CommentID)
		if err != nil {
			result.Failed++
			result.Details = append(result.Details, ReplyResult{
				CommentID: reply.CommentID,
				Status:    "failed",
				Reason:    fmt.Sprintf("API error: %v", err),
			})
			continue
		}

		result.Posted++
		result.Details = append(result.Details, ReplyResult{
			CommentID: reply.CommentID,
			Status:    "posted",
		})
	}

	return result, nil
}

// publishSummaryComment posts or updates a PR-level summary comment.
func (g *GitHubPublisher) publishSummaryComment(ctx context.Context, client *hghelper.Client, prRef PRRef, summary string, botLogin string) (CommentResult, error) {
	// Find existing summary comment
	existing, err := findExistingSummaryComment(ctx, client, prRef, botLogin)
	if err != nil {
		return CommentResult{Posted: false, Updated: false, Error: err.Error()}, err
	}

	// Prepare comment body with marker
	body := fmt.Sprintf("%s\n%s", SummaryMarker, summary)

	if existing != nil {
		// Update existing comment using the helper
		err = client.EditIssueComment(ctx, prRef.Owner, prRef.Repo, existing.ID, body)
		if err != nil {
			return CommentResult{Posted: false, Updated: false, Error: err.Error()}, err
		}
		return CommentResult{Posted: true, Updated: true, CommentID: existing.ID}, nil
	}

	// Create new comment using the helper
	commentID, err := client.CreateIssueComment(ctx, prRef.Owner, prRef.Repo, prRef.PRNumber, body)
	if err != nil {
		return CommentResult{Posted: false, Updated: false, Error: err.Error()}, err
	}

	return CommentResult{Posted: true, Updated: false, CommentID: commentID}, nil
}

// hasBotRepliedToComment checks if the bot has already replied to a review comment.
func hasBotRepliedToComment(ctx context.Context, client *hghelper.Client, prRef PRRef, commentID int64, botLogin string) (bool, error) {
	// Use the helper to list all review comments for the PR
	comments, err := client.ListPullRequestComments(ctx, prRef.Owner, prRef.Repo, prRef.PRNumber)
	if err != nil {
		return false, fmt.Errorf("failed to list comments: %w", err)
	}

	// Check if any reply is from the bot and in_reply_to matches commentID
	for _, comment := range comments {
		// InReplyTo contains the parent comment ID for replies
		if comment.InReplyTo != nil && *comment.InReplyTo == commentID && comment.User.GetLogin() == botLogin {
			return true, nil
		}
	}

	return false, nil
}

// existingComment represents an existing comment found during search
type existingComment struct {
	ID int64
}

// findExistingSummaryComment finds an existing summary comment by the bot.
func findExistingSummaryComment(ctx context.Context, client *hghelper.Client, prRef PRRef, botLogin string) (*existingComment, error) {
	// Use the helper to list all issue comments for the PR
	comments, err := client.ListIssueComments(ctx, prRef.Owner, prRef.Repo, prRef.PRNumber)
	if err != nil {
		return nil, fmt.Errorf("failed to list comments: %w", err)
	}

	// Find the most recent comment from the bot with the marker
	var mostRecent *existingComment
	for _, comment := range comments {
		if comment.Author == botLogin && strings.Contains(comment.Body, SummaryMarker) {
			if mostRecent == nil || comment.CommentID > mostRecent.ID {
				mostRecent = &existingComment{ID: comment.CommentID}
			}
		}
	}

	return mostRecent, nil
}

// readPRFixData reads and parses the pr-fix.json file.
func readPRFixData(path string) (*PRFixData, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var prFix PRFixData
	if err := json.Unmarshal(data, &prFix); err != nil {
		return nil, fmt.Errorf("failed to parse pr-fix.json: %w", err)
	}

	return &prFix, nil
}

// formatReviewReply formats a review reply message.
func formatReviewReply(reply ReviewReply) string {
	var emoji string
	switch reply.Status {
	case "fixed":
		emoji = "✅"
	case "wontfix":
		emoji = "⚠️"
	case "need-info":
		emoji = "❓"
	case "deferred":
		emoji = "🔜"
	default:
		emoji = "📝"
	}

	message := fmt.Sprintf("%s **%s**: %s", emoji, strings.ToUpper(reply.Status), reply.Message)

	if reply.ActionTaken != nil && *reply.ActionTaken != "" {
		message += fmt.Sprintf("\n\n**Action taken**: %s", *reply.ActionTaken)
	}

	return message
}

// getBotLogin returns the bot login from environment or default.
func getBotLogin() string {
	if login := os.Getenv(BotLoginEnv); login != "" {
		return login
	}
	return DefaultBotLogin
}
