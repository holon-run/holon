package github

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// WriteContext writes PR context to the specified directory
func WriteContext(outputDir string, prInfo *PRInfo, reviewThreads []ReviewThread, diff string) error {
	// Create output directory structure
	githubDir := filepath.Join(outputDir, "github")
	if err := os.MkdirAll(githubDir, 0755); err != nil {
		return fmt.Errorf("failed to create github context directory: %w", err)
	}

	// Write pr.json
	if err := writePRJSON(githubDir, prInfo); err != nil {
		return fmt.Errorf("failed to write pr.json: %w", err)
	}

	// Write review_threads.json
	if err := writeReviewThreadsJSON(githubDir, reviewThreads); err != nil {
		return fmt.Errorf("failed to write review_threads.json: %w", err)
	}

	// Write pr.diff if available
	if diff != "" {
		if err := writePRDiff(githubDir, diff); err != nil {
			return fmt.Errorf("failed to write pr.diff: %w", err)
		}
	}

	// Write review.md (optional human-readable rendering)
	if err := writeReviewMarkdown(githubDir, prInfo, reviewThreads); err != nil {
		return fmt.Errorf("failed to write review.md: %w", err)
	}

	return nil
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

// writeReviewMarkdown writes a human-readable markdown summary
func writeReviewMarkdown(dir string, prInfo *PRInfo, threads []ReviewThread) error {
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

// resolvedStatus returns a string representation of the resolved status
func resolvedStatus(resolved bool) string {
	if resolved {
		return "Resolved ✓"
	}
	return "Unresolved"
}
