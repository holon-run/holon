package github

import (
	"encoding/json"
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

	// Test JSON marshaling
	data, err := json.Marshal(comment)
	if err != nil {
		t.Fatalf("Failed to marshal comment: %v", err)
	}

	// Verify is_trigger is in the JSON output
	var result map[string]interface{}
	if err := json.Unmarshal(data, &result); err != nil {
		t.Fatalf("Failed to unmarshal JSON: %v", err)
	}

	if result["is_trigger"] != true {
		t.Error("Expected is_trigger to be true in JSON output")
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

	// Test JSON marshaling
	data, err := json.Marshal(thread)
	if err != nil {
		t.Fatalf("Failed to marshal thread: %v", err)
	}

	// Verify is_trigger is in the JSON output
	var result map[string]interface{}
	if err := json.Unmarshal(data, &result); err != nil {
		t.Fatalf("Failed to unmarshal JSON: %v", err)
	}

	if result["is_trigger"] != true {
		t.Error("Expected is_trigger to be true in JSON output")
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

	// Test JSON marshaling
	data, err := json.Marshal(reply)
	if err != nil {
		t.Fatalf("Failed to marshal reply: %v", err)
	}

	// Verify is_trigger is in the JSON output
	var result map[string]interface{}
	if err := json.Unmarshal(data, &result); err != nil {
		t.Fatalf("Failed to unmarshal JSON: %v", err)
	}

	if result["is_trigger"] != true {
		t.Error("Expected is_trigger to be true in JSON output")
	}
}

// TestIssueCommentIsTriggerNotSet verifies that IsTrigger defaults to false and is omitted from JSON
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

	// Test JSON marshaling - is_trigger should be omitted due to omitempty
	data, err := json.Marshal(comment)
	if err != nil {
		t.Fatalf("Failed to marshal comment: %v", err)
	}

	// Verify is_trigger is NOT in the JSON output (omitempty)
	var result map[string]interface{}
	if err := json.Unmarshal(data, &result); err != nil {
		t.Fatalf("Failed to unmarshal JSON: %v", err)
	}

	if _, exists := result["is_trigger"]; exists {
		t.Error("Expected is_trigger to be omitted from JSON output when false (omitempty)")
	}
}

// TestIssueCommentJSONRoundTrip verifies that IsTrigger survives a JSON round-trip
func TestIssueCommentJSONRoundTrip(t *testing.T) {
	original := IssueComment{
		CommentID: 123,
		URL:       "https://github.com/owner/repo/issues/1#comment-123",
		Body:      "@holonbot fix this bug",
		Author:    "testuser",
		IsTrigger: true,
	}

	// Marshal to JSON
	data, err := json.Marshal(original)
	if err != nil {
		t.Fatalf("Failed to marshal: %v", err)
	}

	// Unmarshal back
	var restored IssueComment
	if err := json.Unmarshal(data, &restored); err != nil {
		t.Fatalf("Failed to unmarshal: %v", err)
	}

	// Verify IsTrigger survived the round-trip
	if restored.IsTrigger != original.IsTrigger {
		t.Errorf("IsTrigger round-trip failed: got %v, want %v", restored.IsTrigger, original.IsTrigger)
	}
	if restored.CommentID != original.CommentID {
		t.Errorf("CommentID round-trip failed: got %d, want %d", restored.CommentID, original.CommentID)
	}
}
