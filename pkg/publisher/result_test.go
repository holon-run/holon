package publisher

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestWriteResult(t *testing.T) {
	t.Run("writes result file successfully", func(t *testing.T) {
		tmpDir := t.TempDir()

		result := PublishResult{
			Provider:   "test",
			Target:     "test-target",
			PublishedAt: time.Date(2024, 1, 1, 12, 0, 0, 0, time.UTC),
			Actions: []PublishAction{
				{
					Type:        "test_action",
					Description: "Test action description",
				},
			},
			Success: true,
		}

		err := WriteResult(tmpDir, result)
		if err != nil {
			t.Fatalf("expected no error, got %v", err)
		}

		// Verify file exists
		resultPath := filepath.Join(tmpDir, PublishResultFile)
		if _, err := os.Stat(resultPath); os.IsNotExist(err) {
			t.Error("result file was not created")
		}

		// Verify file contents
		data, err := os.ReadFile(resultPath)
		if err != nil {
			t.Fatalf("failed to read result file: %v", err)
		}

		var readResult PublishResult
		if err := json.Unmarshal(data, &readResult); err != nil {
			t.Fatalf("failed to unmarshal result: %v", err)
		}

		if readResult.Provider != "test" {
			t.Errorf("expected provider 'test', got '%s'", readResult.Provider)
		}

		if readResult.Target != "test-target" {
			t.Errorf("expected target 'test-target', got '%s'", readResult.Target)
		}

		if len(readResult.Actions) != 1 {
			t.Errorf("expected 1 action, got %d", len(readResult.Actions))
		}

		if !readResult.Success {
			t.Error("expected success to be true")
		}
	})

	t.Run("sets PublishedAt to now if zero", func(t *testing.T) {
		tmpDir := t.TempDir()
		before := time.Now()

		result := PublishResult{
			Provider:    "test",
			Target:      "test-target",
			PublishedAt: time.Time{}, // Zero value
			Success:     true,
		}

		err := WriteResult(tmpDir, result)
		if err != nil {
			t.Fatalf("expected no error, got %v", err)
		}

		after := time.Now()

		// Read back the result
		readResult, err := ReadResult(tmpDir)
		if err != nil {
			t.Fatalf("failed to read result: %v", err)
		}

		if readResult.PublishedAt.Before(before) || readResult.PublishedAt.After(after) {
			t.Error("PublishedAt was not set to current time")
		}
	})

	t.Run("creates output directory if it doesn't exist", func(t *testing.T) {
		tmpDir := t.TempDir()
		outputDir := filepath.Join(tmpDir, "nested", "dir")

		result := PublishResult{
			Provider: "test",
			Target:   "test-target",
			Success:  true,
		}

		err := WriteResult(outputDir, result)
		if err != nil {
			t.Fatalf("expected no error, got %v", err)
		}

		// Verify directory was created
		if _, err := os.Stat(outputDir); os.IsNotExist(err) {
			t.Error("output directory was not created")
		}
	})
}

func TestReadResult(t *testing.T) {
	t.Run("reads result file successfully", func(t *testing.T) {
		tmpDir := t.TempDir()

		// Write a result file
		result := PublishResult{
			Provider:    "test",
			Target:      "test-target",
			PublishedAt: time.Date(2024, 1, 1, 12, 0, 0, 0, time.UTC),
			Actions: []PublishAction{
				{
					Type:        "test_action",
					Description: "Test action description",
					Metadata: map[string]string{
						"key": "value",
					},
				},
			},
			Errors: []PublishError{
				{
					Message: "test error",
					Action:  "test_action",
				},
			},
			Success: false,
		}

		err := WriteResult(tmpDir, result)
		if err != nil {
			t.Fatalf("failed to write result: %v", err)
		}

		// Read it back
		readResult, err := ReadResult(tmpDir)
		if err != nil {
			t.Fatalf("expected no error, got %v", err)
		}

		if readResult.Provider != result.Provider {
			t.Errorf("expected provider '%s', got '%s'", result.Provider, readResult.Provider)
		}

		if readResult.Target != result.Target {
			t.Errorf("expected target '%s', got '%s'", result.Target, readResult.Target)
		}

		if len(readResult.Actions) != len(result.Actions) {
			t.Errorf("expected %d actions, got %d", len(result.Actions), len(readResult.Actions))
		}

		if len(readResult.Errors) != len(result.Errors) {
			t.Errorf("expected %d errors, got %d", len(result.Errors), len(readResult.Errors))
		}

		if readResult.Success != result.Success {
			t.Errorf("expected success %v, got %v", result.Success, readResult.Success)
		}
	})

	t.Run("returns error when file doesn't exist", func(t *testing.T) {
		tmpDir := t.TempDir()

		_, err := ReadResult(tmpDir)
		if err == nil {
			t.Error("expected error for missing file, got nil")
		}
	})
}

func TestNewError(t *testing.T) {
	t.Run("creates error with message", func(t *testing.T) {
		err := NewError("test error message")

		if err.Message != "test error message" {
			t.Errorf("expected message 'test error message', got '%s'", err.Message)
		}

		if err.Action != "" {
			t.Errorf("expected empty action, got '%s'", err.Action)
		}
	})
}

func TestNewErrorWithAction(t *testing.T) {
	t.Run("creates error with message and action", func(t *testing.T) {
		err := NewErrorWithAction("test error message", "test_action")

		if err.Message != "test error message" {
			t.Errorf("expected message 'test error message', got '%s'", err.Message)
		}

		if err.Action != "test_action" {
			t.Errorf("expected action 'test_action', got '%s'", err.Action)
		}
	})
}

func TestNewAction(t *testing.T) {
	t.Run("creates action with type and description", func(t *testing.T) {
		action := NewAction("test_action", "test description")

		if action.Type != "test_action" {
			t.Errorf("expected type 'test_action', got '%s'", action.Type)
		}

		if action.Description != "test description" {
			t.Errorf("expected description 'test description', got '%s'", action.Description)
		}

		if action.Metadata == nil {
			t.Error("expected metadata map to be initialized")
		}
	})
}

func TestPublishAction_AddMetadata(t *testing.T) {
	t.Run("adds metadata to action", func(t *testing.T) {
		action := NewAction("test_action", "test description")

		action.AddMetadata("key1", "value1")
		action.AddMetadata("key2", "value2")

		if action.Metadata["key1"] != "value1" {
			t.Errorf("expected metadata key1 to be 'value1', got '%s'", action.Metadata["key1"])
		}

		if action.Metadata["key2"] != "value2" {
			t.Errorf("expected metadata key2 to be 'value2', got '%s'", action.Metadata["key2"])
		}
	})

	t.Run("initializes metadata if nil", func(t *testing.T) {
		action := PublishAction{
			Type:        "test_action",
			Description: "test description",
			Metadata:    nil,
		}

		action.AddMetadata("key", "value")

		if action.Metadata == nil {
			t.Error("expected metadata to be initialized")
		}

		if action.Metadata["key"] != "value" {
			t.Errorf("expected metadata key to be 'value', got '%s'", action.Metadata["key"])
		}
	})
}
