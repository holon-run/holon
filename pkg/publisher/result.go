package publisher

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

const PublishResultFile = "publish-result.json"

// WriteResult writes the publish result to the output directory.
// The result is written as "publish-result.json" in the specified directory.
func WriteResult(outputDir string, result PublishResult) error {
	// Ensure the output directory exists
	if err := os.MkdirAll(outputDir, 0755); err != nil {
		return fmt.Errorf("failed to create output directory: %w", err)
	}

	// Set published_at to now if not already set
	if result.PublishedAt.IsZero() {
		result.PublishedAt = time.Now()
	}

	// Marshal to JSON with indentation for readability
	data, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal publish result: %w", err)
	}

	// Write to file
	resultPath := filepath.Join(outputDir, PublishResultFile)
	if err := os.WriteFile(resultPath, data, 0644); err != nil {
		return fmt.Errorf("failed to write publish result: %w", err)
	}

	return nil
}

// ReadResult reads a publish result from the output directory.
func ReadResult(outputDir string) (PublishResult, error) {
	var result PublishResult

	resultPath := filepath.Join(outputDir, PublishResultFile)
	data, err := os.ReadFile(resultPath)
	if err != nil {
		return result, fmt.Errorf("failed to read publish result: %w", err)
	}

	if err := json.Unmarshal(data, &result); err != nil {
		return result, fmt.Errorf("failed to unmarshal publish result: %w", err)
	}

	return result, nil
}

// NewError creates a PublishError from an error message.
func NewError(message string) PublishError {
	return PublishError{
		Message: message,
	}
}

// NewErrorWithAction creates a PublishError for a failed action.
func NewErrorWithAction(message, action string) PublishError {
	return PublishError{
		Message: message,
		Action:  action,
	}
}

// NewAction creates a PublishAction.
func NewAction(actionType, description string) PublishAction {
	return PublishAction{
		Type:        actionType,
		Description: description,
		Metadata:    make(map[string]string),
	}
}

// AddMetadata adds metadata to an action.
func (a *PublishAction) AddMetadata(key, value string) {
	if a.Metadata == nil {
		a.Metadata = make(map[string]string)
	}
	a.Metadata[key] = value
}
