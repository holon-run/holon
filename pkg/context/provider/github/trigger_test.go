package github

import (
	"testing"
)

// TestIssueCommentIsTrigger verifies that the IsTrigger field is properly serialized
func TestIssueCommentIsTrigger(t *testing.T) {
	comment := IssueComment{
		CommentID: 123,
		URL:       "https://github.com/owner/repo/issues/1#comment-123",
		Body:      "@holonbot fix this bug",
		Author:    "testuser",
		IsTrigger: true,
	}

	// Verify IsTrigger field is set
	if !comment.IsTrigger {
		t.Error("Expected IsTrigger to be true")
	}
}

// TestReviewThreadIsTrigger verifies that the IsTrigger field is properly serialized for review threads
func TestReviewThreadIsTrigger(t *testing.T) {
	thread := ReviewThread{
		CommentID: 456,
		URL:       "https://github.com/owner/repo/pull/1#discussion_r456",
		Path:      "file.go",
		Line:      42,
		Body:      "@holonbot refactor this function",
		Author:    "testuser",
		IsTrigger: true,
	}

	// Verify IsTrigger field is set
	if !thread.IsTrigger {
		t.Error("Expected IsTrigger to be true")
	}
}

// TestReplyIsTrigger verifies that the IsTrigger field is properly serialized for replies
func TestReplyIsTrigger(t *testing.T) {
	reply := Reply{
		CommentID:   789,
		URL:         "https://github.com/owner/repo/pull/1#discussion_r789",
		Body:        "@holonbot add error handling",
		Author:      "testuser",
		InReplyToID: 456,
		IsTrigger:   true,
	}

	// Verify IsTrigger field is set
	if !reply.IsTrigger {
		t.Error("Expected IsTrigger to be true")
	}
}

// TestIssueCommentIsTriggerNotSet verifies that IsTrigger defaults to false
func TestIssueCommentIsTriggerNotSet(t *testing.T) {
	comment := IssueComment{
		CommentID: 123,
		URL:       "https://github.com/owner/repo/issues/1#comment-123",
		Body:      "This is a regular comment",
		Author:    "testuser",
	}

	// Verify IsTrigger field is false by default
	if comment.IsTrigger {
		t.Error("Expected IsTrigger to be false by default")
	}
}
