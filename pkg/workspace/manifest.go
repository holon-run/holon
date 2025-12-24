package workspace

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// WriteManifest writes a workspace.manifest.json file to the workspace root
func WriteManifest(dest string, result PrepareResult) error {
	manifest := Manifest{
		Strategy:   result.Strategy,
		Source:     result.Source,
		Ref:        result.Ref,
		HeadSHA:    result.HeadSHA,
		CreatedAt:  result.CreatedAt,
		HasHistory: result.HasHistory,
		IsShallow:  result.IsShallow,
		Notes:      result.Notes,
	}

	data, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal workspace manifest: %w", err)
	}

	manifestPath := filepath.Join(dest, "workspace.manifest.json")
	if err := os.WriteFile(manifestPath, data, 0644); err != nil {
		return fmt.Errorf("failed to write workspace manifest: %w", err)
	}

	return nil
}

// ReadManifest reads a workspace.manifest.json file from the workspace root
func ReadManifest(dest string) (*Manifest, error) {
	manifestPath := filepath.Join(dest, "workspace.manifest.json")
	data, err := os.ReadFile(manifestPath)
	if err != nil {
		return nil, fmt.Errorf("failed to read workspace manifest: %w", err)
	}

	var manifest Manifest
	if err := json.Unmarshal(data, &manifest); err != nil {
		return nil, fmt.Errorf("failed to unmarshal workspace manifest: %w", err)
	}

	return &manifest, nil
}

// NewPrepareResult creates a PrepareResult with the current timestamp
func NewPrepareResult(strategy string) PrepareResult {
	return PrepareResult{
		Strategy:  strategy,
		CreatedAt: time.Now(),
		Notes:     []string{},
	}
}
