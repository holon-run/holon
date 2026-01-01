package github

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/holon-run/holon/pkg/context/collector"
	"github.com/holon-run/holon/pkg/prompt"
)

// WriteManifest writes the collection manifest to the output directory
func WriteManifest(outputDir string, result collector.CollectResult) error {
	data, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal manifest: %w", err)
	}

	path := filepath.Join(outputDir, "manifest.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write manifest: %w", err)
	}

	return nil
}

// WritePRContext writes PR context files and returns the list of files written
func WritePRContext(outputDir string, prInfo *PRInfo, reviewThreads []ReviewThread, comments []IssueComment, diff string, checkRuns []CheckRun, combinedStatus *CombinedStatus) ([]collector.FileInfo, error) {
	// Create output directory structure
	githubDir := filepath.Join(outputDir, "github")
	if err := os.MkdirAll(githubDir, 0755); err != nil {
		return nil, fmt.Errorf("failed to create github context directory: %w", err)
	}

	var files []collector.FileInfo

	// Write pr.json
	if err := writePRJSON(githubDir, prInfo); err != nil {
		return nil, fmt.Errorf("failed to write pr.json: %w", err)
	}
	files = append(files, collector.FileInfo{
		Path:        "github/pr.json",
		ContentType: "application/json",
		Description: "Pull request metadata",
	})

	// Write review_threads.json
	if err := writeReviewThreadsJSON(githubDir, reviewThreads); err != nil {
		return nil, fmt.Errorf("failed to write review_threads.json: %w", err)
	}
	files = append(files, collector.FileInfo{
		Path:        "github/review_threads.json",
		ContentType: "application/json",
		Description: "Review comment threads",
	})

	// Write comments.json if available
	if len(comments) > 0 {
		if err := writeCommentsJSON(githubDir, comments); err != nil {
			return nil, fmt.Errorf("failed to write comments.json: %w", err)
		}
		files = append(files, collector.FileInfo{
			Path:        "github/comments.json",
			ContentType: "application/json",
			Description: "PR comments",
		})
	}

	// Write pr-fix.schema.json
	if err := writePRFixSchema(outputDir); err != nil {
		return nil, fmt.Errorf("failed to write pr-fix schema: %w", err)
	}
	files = append(files, collector.FileInfo{
		Path:        "pr-fix.schema.json",
		ContentType: "application/json",
		Description: "PR-fix output schema",
	})

	// Write pr.diff if available
	if diff != "" {
		if err := writePRDiff(githubDir, diff); err != nil {
			return nil, fmt.Errorf("failed to write pr.diff: %w", err)
		}
		files = append(files, collector.FileInfo{
			Path:        "github/pr.diff",
			ContentType: "text/plain",
			Description: "Pull request diff",
		})
	}

	// Write check_runs.json if available
	if len(checkRuns) > 0 {
		if err := writeCheckRunsJSON(githubDir, checkRuns); err != nil {
			return nil, fmt.Errorf("failed to write check_runs.json: %w", err)
		}
		files = append(files, collector.FileInfo{
			Path:        "github/check_runs.json",
			ContentType: "application/json",
			Description: "CI check runs",
		})
	}

	// Write commit_status.json if available
	if combinedStatus != nil {
		if err := writeCommitStatusJSON(githubDir, combinedStatus); err != nil {
			return nil, fmt.Errorf("failed to write commit_status.json: %w", err)
		}
		files = append(files, collector.FileInfo{
			Path:        "github/commit_status.json",
			ContentType: "application/json",
			Description: "Combined commit status",
		})
	}

	// Write review.md (optional human-readable rendering)
	if err := writeReviewMarkdown(githubDir, prInfo, reviewThreads, checkRuns, combinedStatus); err != nil {
		return nil, fmt.Errorf("failed to write review.md: %w", err)
	}
	files = append(files, collector.FileInfo{
		Path:        "github/review.md",
		ContentType: "text/markdown",
		Description: "Review summary in markdown format",
	})

	return files, nil
}

// WriteIssueContext writes issue context files and returns the list of files written
func WriteIssueContext(outputDir string, issueInfo *IssueInfo, comments []IssueComment) ([]collector.FileInfo, error) {
	// Create output directory structure
	githubDir := filepath.Join(outputDir, "github")
	if err := os.MkdirAll(githubDir, 0755); err != nil {
		return nil, fmt.Errorf("failed to create github context directory: %w", err)
	}

	var files []collector.FileInfo

	// Write issue.json
	if err := writeIssueJSON(githubDir, issueInfo); err != nil {
		return nil, fmt.Errorf("failed to write issue.json: %w", err)
	}
	files = append(files, collector.FileInfo{
		Path:        "github/issue.json",
		ContentType: "application/json",
		Description: "Issue metadata",
	})

	// Write comments.json
	if err := writeCommentsJSON(githubDir, comments); err != nil {
		return nil, fmt.Errorf("failed to write comments.json: %w", err)
	}
	files = append(files, collector.FileInfo{
		Path:        "github/comments.json",
		ContentType: "application/json",
		Description: "Issue comments",
	})

	// Write issue.md (optional human-readable rendering)
	if err := writeIssueMarkdown(githubDir, issueInfo, comments); err != nil {
		return nil, fmt.Errorf("failed to write issue.md: %w", err)
	}
	files = append(files, collector.FileInfo{
		Path:        "github/issue.md",
		ContentType: "text/markdown",
		Description: "Issue summary in markdown format",
	})

	return files, nil
}

// writePRJSON writes PR information as JSON
func writePRJSON(dir string, prInfo *PRInfo) error {
	data, err := json.MarshalIndent(prInfo, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal PR info: %w", err)
	}

	path := filepath.Join(dir, "pr.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// writeReviewThreadsJSON writes review threads as JSON
func writeReviewThreadsJSON(dir string, threads []ReviewThread) error {
	data, err := json.MarshalIndent(threads, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal review threads: %w", err)
	}

	path := filepath.Join(dir, "review_threads.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// writePRDiff writes the PR diff
func writePRDiff(dir string, diff string) error {
	path := filepath.Join(dir, "pr.diff")
	if err := os.WriteFile(path, []byte(diff), 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// writeIssueJSON writes issue information as JSON
func writeIssueJSON(dir string, issueInfo *IssueInfo) error {
	data, err := json.MarshalIndent(issueInfo, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal issue info: %w", err)
	}

	path := filepath.Join(dir, "issue.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// writeCommentsJSON writes issue comments as JSON
func writeCommentsJSON(dir string, comments []IssueComment) error {
	data, err := json.MarshalIndent(comments, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal comments: %w", err)
	}

	path := filepath.Join(dir, "comments.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// writeCheckRunsJSON writes check runs as JSON
func writeCheckRunsJSON(dir string, checkRuns []CheckRun) error {
	data, err := json.MarshalIndent(checkRuns, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal check runs: %w", err)
	}

	path := filepath.Join(dir, "check_runs.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// writeCommitStatusJSON writes combined status as JSON
func writeCommitStatusJSON(dir string, status *CombinedStatus) error {
	data, err := json.MarshalIndent(status, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal combined status: %w", err)
	}

	path := filepath.Join(dir, "commit_status.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

func writePRFixSchema(dir string) error {
	data, err := prompt.ReadAsset("modes/pr-fix/pr-fix.schema.json")
	if err != nil {
		return fmt.Errorf("failed to read pr-fix schema asset: %w", err)
	}

	path := filepath.Join(dir, "pr-fix.schema.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// writeReviewMarkdown writes a human-readable markdown summary for PRs
func writeReviewMarkdown(dir string, prInfo *PRInfo, threads []ReviewThread, checkRuns []CheckRun, combinedStatus *CombinedStatus) error {
	var sb strings.Builder

	sb.WriteString("# Pull Request Review Context\n\n")

	// PR Information
	sb.WriteString("## Pull Request Information\n\n")
	sb.WriteString(fmt.Sprintf("- **Number**: #%d\n", prInfo.Number))
	sb.WriteString(fmt.Sprintf("- **Title**: %s\n", prInfo.Title))
	sb.WriteString(fmt.Sprintf("- **Author**: @%s\n", prInfo.Author))
	sb.WriteString(fmt.Sprintf("- **State**: %s\n", prInfo.State))
	sb.WriteString(fmt.Sprintf("- **URL**: %s\n", prInfo.URL))
	baseSHA := prInfo.BaseSHA
	if len(baseSHA) > 7 {
		baseSHA = baseSHA[:7]
	}
	headSHA := prInfo.HeadSHA
	if len(headSHA) > 7 {
		headSHA = headSHA[:7]
	}
	sb.WriteString(fmt.Sprintf("- **Base**: %s (%s)\n", prInfo.BaseRef, baseSHA))
	sb.WriteString(fmt.Sprintf("- **Head**: %s (%s)\n", prInfo.HeadRef, headSHA))
	sb.WriteString(fmt.Sprintf("- **Created**: %s\n", prInfo.CreatedAt.Format(time.RFC3339)))
	sb.WriteString(fmt.Sprintf("- **Updated**: %s\n\n", prInfo.UpdatedAt.Format(time.RFC3339)))

	if prInfo.Body != "" {
		sb.WriteString("### Description\n\n")
		sb.WriteString(prInfo.Body)
		sb.WriteString("\n\n")
	}

	// CI/Check Results Summary
	if len(checkRuns) > 0 || combinedStatus != nil {
		sb.WriteString("## CI/Check Results\n\n")

		if combinedStatus != nil {
			sb.WriteString(fmt.Sprintf("### Combined Status\n\n"))
			sb.WriteString(fmt.Sprintf("**State**: %s\n", statusBadge(combinedStatus.State)))
			sb.WriteString(fmt.Sprintf("**Total Checks**: %d\n\n", combinedStatus.TotalCount))
		}

		if len(checkRuns) > 0 {
			sb.WriteString(fmt.Sprintf("### Check Runs (%d)\n\n", len(checkRuns)))

			// Group by conclusion for summary
			successCount := 0
			failureCount := 0
			otherCount := 0
			for _, cr := range checkRuns {
				switch cr.Conclusion {
				case "success":
					successCount++
				case "failure", "timed_out", "action_required":
					failureCount++
				default:
					otherCount++
				}
			}

			sb.WriteString(fmt.Sprintf("- **Passed**: %d\n", successCount))
			sb.WriteString(fmt.Sprintf("- **Failed**: %d\n", failureCount))
			sb.WriteString(fmt.Sprintf("- **Other**: %d\n\n", otherCount))

			// List individual check runs
			for _, cr := range checkRuns {
				sb.WriteString(fmt.Sprintf("#### %s\n", cr.Name))
				sb.WriteString(fmt.Sprintf("- **Status**: %s", cr.Status))
				if cr.Conclusion != "" {
					sb.WriteString(fmt.Sprintf(", **Conclusion**: %s", statusBadge(cr.Conclusion)))
				}
				sb.WriteString("\n")
				if cr.AppSlug != "" {
					sb.WriteString(fmt.Sprintf("- **App**: %s\n", cr.AppSlug))
				}
				if cr.DetailsURL != "" {
					sb.WriteString(fmt.Sprintf("- **Details**: [View Details](%s)\n", cr.DetailsURL))
				}
				if cr.Output.Title != "" {
					sb.WriteString(fmt.Sprintf("- **Title**: %s\n", cr.Output.Title))
				}
				if cr.Output.Summary != "" {
					sb.WriteString(fmt.Sprintf("- **Summary**: %s\n", cr.Output.Summary))
				}
				sb.WriteString("\n")
			}
		}
	}

	// Review Threads
	sb.WriteString("## Review Comments\n\n")

	if len(threads) == 0 {
		sb.WriteString("*No review comments found.*\n")
	} else {
		sb.WriteString(fmt.Sprintf("Total threads: %d\n\n", len(threads)))

		for i, thread := range threads {
			sb.WriteString(fmt.Sprintf("### Thread %d\n\n", i+1))
			sb.WriteString(fmt.Sprintf("- **File**: `%s`\n", thread.Path))
			if thread.Line > 0 {
				start := thread.Line
				end := thread.Line
				if thread.StartLine > 0 && thread.StartLine != thread.Line {
					start = thread.StartLine
				}
				sb.WriteString(fmt.Sprintf("- **Line**: %d", start))
				if end != start {
					sb.WriteString(fmt.Sprintf("-%d", end))
				}
				sb.WriteString("\n")
			}
			if thread.Side != "" {
				sb.WriteString(fmt.Sprintf("- **Side**: %s\n", thread.Side))
			}
			sb.WriteString(fmt.Sprintf("- **Author**: @%s\n", thread.Author))
			sb.WriteString(fmt.Sprintf("- **Status**: %s\n", resolvedStatus(thread.Resolved)))
			sb.WriteString(fmt.Sprintf("- **URL**: %s\n", thread.URL))
			sb.WriteString("\n")

			if thread.DiffHunk != "" {
				sb.WriteString("**Context:**\n\n")
				sb.WriteString("```diff\n")
				sb.WriteString(thread.DiffHunk)
				sb.WriteString("\n```\n\n")
			}

			sb.WriteString("**Comment:**\n\n")
			sb.WriteString("> " + strings.ReplaceAll(thread.Body, "\n", "\n> "))
			sb.WriteString("\n\n")

			// Add replies
			if len(thread.Replies) > 0 {
				sb.WriteString("**Replies:**\n\n")
				for j, reply := range thread.Replies {
					sb.WriteString(fmt.Sprintf("%d. **@%s** (%s):\n", j+1, reply.Author, reply.CreatedAt.Format("2006-01-02 15:04")))
					sb.WriteString("   > " + strings.ReplaceAll(reply.Body, "\n", "\n   > "))
					sb.WriteString("\n\n")
				}
			}

			sb.WriteString("---\n\n")
		}
	}

	path := filepath.Join(dir, "review.md")
	if err := os.WriteFile(path, []byte(sb.String()), 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// writeIssueMarkdown writes a human-readable markdown summary for issues
func writeIssueMarkdown(dir string, issueInfo *IssueInfo, comments []IssueComment) error {
	var sb strings.Builder

	sb.WriteString("# Issue Context\n\n")

	// Issue Information
	sb.WriteString("## Issue Information\n\n")
	sb.WriteString(fmt.Sprintf("- **Number**: #%d\n", issueInfo.Number))
	sb.WriteString(fmt.Sprintf("- **Title**: %s\n", issueInfo.Title))
	sb.WriteString(fmt.Sprintf("- **Author**: @%s\n", issueInfo.Author))
	sb.WriteString(fmt.Sprintf("- **State**: %s\n", issueInfo.State))
	sb.WriteString(fmt.Sprintf("- **URL**: %s\n", issueInfo.URL))
	if issueInfo.Assignee != "" {
		sb.WriteString(fmt.Sprintf("- **Assignee**: @%s\n", issueInfo.Assignee))
	}
	sb.WriteString(fmt.Sprintf("- **Created**: %s\n", issueInfo.CreatedAt.Format(time.RFC3339)))
	sb.WriteString(fmt.Sprintf("- **Updated**: %s\n\n", issueInfo.UpdatedAt.Format(time.RFC3339)))

	if issueInfo.Body != "" {
		sb.WriteString("### Description\n\n")
		sb.WriteString(issueInfo.Body)
		sb.WriteString("\n\n")
	}

	// Comments
	sb.WriteString("## Comments\n\n")

	if len(comments) == 0 {
		sb.WriteString("*No comments found.*\n")
	} else {
		sb.WriteString(fmt.Sprintf("Total comments: %d\n\n", len(comments)))

		for i, comment := range comments {
			sb.WriteString(fmt.Sprintf("### Comment %d\n\n", i+1))
			sb.WriteString(fmt.Sprintf("- **Author**: @%s\n", comment.Author))
			sb.WriteString(fmt.Sprintf("- **Created**: %s\n", comment.CreatedAt.Format(time.RFC3339)))
			sb.WriteString(fmt.Sprintf("- **URL**: %s\n", comment.URL))
			sb.WriteString("\n")

			sb.WriteString("**Comment:**\n\n")
			sb.WriteString("> " + strings.ReplaceAll(comment.Body, "\n", "\n> "))
			sb.WriteString("\n\n")

			sb.WriteString("---\n\n")
		}
	}

	path := filepath.Join(dir, "issue.md")
	if err := os.WriteFile(path, []byte(sb.String()), 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}

// resolvedStatus returns a string representation of the resolved status
func resolvedStatus(resolved bool) string {
	if resolved {
		return "Resolved ✓"
	}
	return "Unresolved"
}

// statusBadge returns a string representation of a status/conclusion with emoji
func statusBadge(status string) string {
	switch status {
	case "success":
		return "Success ✓"
	case "failure":
		return "Failure ✗"
	case "pending":
		return "Pending ⏳"
	case "error":
		return "Error ⚠️"
	case "timed_out":
		return "Timed Out ⏱️"
	case "cancelled":
		return "Cancelled 🚫"
	case "action_required":
		return "Action Required ⚡"
	case "neutral":
		return "Neutral ○"
	case "skipped":
		return "Skipped ↷"
	default:
		// Capitalize first letter
		if len(status) == 0 {
			return "Unknown"
		}
		return strings.ToUpper(status[:1]) + status[1:]
	}
}
