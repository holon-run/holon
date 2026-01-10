package github

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

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
	// Read schema from the prompt assets' schemas/ directory
	data, err := prompt.ReadAsset("schemas/pr-fix.schema.json")
	if err != nil {
		return fmt.Errorf("failed to read pr-fix schema: %w", err)
	}

	path := filepath.Join(dir, "pr-fix.schema.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}

	return nil
}
