package v1

import (
	"encoding/json"
	"testing"
)

// TestHolonManifest_BackwardCompatibility tests that the Go HolonManifest type
// can unmarshal all historical manifest formats from agent bundles v0.6.0 and v0.7.0.
// This is a contract test to prevent type drift between TypeScript (agent) and Go (runner).
// See: https://github.com/holon-run/holon/issues/417
func TestHolonManifest_BackwardCompatibility(t *testing.T) {
	tests := []struct {
		name     string
		jsonData string
		validate func(t *testing.T, m *HolonManifest)
	}{
		{
			name: "v0.7.0 format with nested engine metadata",
			jsonData: `{
				"status": "completed",
				"outcome": "success",
				"duration": "12.5s",
				"artifacts": ["diff.patch", "summary.md", "evidence/"],
				"metadata": {
					"agent": "agent-claude",
					"version": "v0.7.0",
					"mode": "solve",
					"engine": {
						"model": "claude-sonnet-4-5-20250929",
						"version": "20250929"
					}
				}
			}`,
			validate: func(t *testing.T, m *HolonManifest) {
				// Verify the nested engine object is preserved in metadata
				if m.Metadata == nil {
					t.Fatal("metadata should not be nil")
				}
				engine, ok := m.Metadata["engine"]
				if !ok {
					t.Fatal("metadata should contain 'engine' key")
				}
				// engine should be a map[string]interface{} (JSON object)
				engineMap, ok := engine.(map[string]interface{})
				if !ok {
					t.Fatalf("engine should be a map, got %T", engine)
				}
				if model, ok := engineMap["model"].(string); !ok || model != "claude-sonnet-4-5-20250929" {
					t.Errorf("expected engine.model to be 'claude-sonnet-4-5-20250929', got %v", engineMap["model"])
				}
			},
		},
		{
			name: "v0.6.0 format without engine metadata",
			jsonData: `{
				"status": "completed",
				"outcome": "success",
				"duration": "8.3s",
				"artifacts": ["diff.patch", "summary.md"],
				"metadata": {
					"agent": "agent-claude",
					"version": "v0.6.0",
					"mode": "solve"
				}
			}`,
			validate: func(t *testing.T, m *HolonManifest) {
				if m.Metadata == nil {
					t.Fatal("metadata should not be nil")
				}
				if m.Metadata["agent"] != "agent-claude" {
					t.Errorf("expected agent 'agent-claude', got %v", m.Metadata["agent"])
				}
				// engine key should not exist in v0.6.0
				if _, ok := m.Metadata["engine"]; ok {
					t.Error("metadata should not contain 'engine' key in v0.6.0 format")
				}
			},
		},
		{
			name: "probe mode format",
			jsonData: `{
				"status": "completed",
				"outcome": "success",
				"duration": "0s",
				"artifacts": [{"name": "manifest.json", "path": "manifest.json"}],
				"metadata": {
					"mode": "probe"
				}
			}`,
			validate: func(t *testing.T, m *HolonManifest) {
				if m.Status != "completed" {
					t.Errorf("expected status 'completed', got %s", m.Status)
				}
				if len(m.Artifacts) != 1 {
					t.Errorf("expected 1 artifact, got %d", len(m.Artifacts))
				}
			},
		},
		{
			name: "failure outcome - note: error is not stored in manifest struct",
			jsonData: `{
				"status": "completed",
				"outcome": "failure",
				"duration": "3.2s",
				"artifacts": ["evidence/"],
				"metadata": {
					"agent": "agent-claude",
					"version": "v0.7.0",
					"mode": "solve"
				}
			}`,
			validate: func(t *testing.T, m *HolonManifest) {
				if m.Outcome != "failure" {
					t.Errorf("expected outcome 'failure', got %s", m.Outcome)
				}
				if m.Metadata == nil {
					t.Fatal("metadata should not be nil")
				}
			},
		},
		{
			name: "backward compatibility: duration as number",
			jsonData: `{
				"status": "completed",
				"outcome": "success",
				"duration": 12.5,
				"artifacts": ["diff.patch"]
			}`,
			validate: func(t *testing.T, m *HolonManifest) {
				// Duration type accepts number and normalizes to "12.5s" string
				expectedDuration := "12.5s"
				if string(m.Duration) != expectedDuration {
					t.Errorf("expected duration '%s', got '%s'", expectedDuration, m.Duration)
				}
			},
		},
		{
			name: "backward compatibility: artifacts as object array",
			jsonData: `{
				"status": "completed",
				"outcome": "success",
				"duration": "5.0s",
				"artifacts": [
					{"name": "diff.patch", "path": "diff.patch"},
					{"name": "summary.md", "path": "summary.md"}
				]
			}`,
			validate: func(t *testing.T, m *HolonManifest) {
				// Artifacts type accepts object array and normalizes to string array
				if len(m.Artifacts) != 2 {
					t.Errorf("expected 2 artifacts, got %d", len(m.Artifacts))
				}
				expected := []string{"diff.patch", "summary.md"}
				for i, artifact := range m.Artifacts {
					if artifact != expected[i] {
						t.Errorf("artifact[%d]: expected '%s', got '%s'", i, expected[i], artifact)
					}
				}
			},
		},
		{
			name: "pr-fix mode format",
			jsonData: `{
				"status": "completed",
				"outcome": "success",
				"duration": "15.7s",
				"artifacts": ["pr-fix.json", "summary.md", "evidence/"],
				"metadata": {
					"agent": "agent-claude-prfix",
					"version": "v0.7.0",
					"mode": "pr-fix"
				}
			}`,
			validate: func(t *testing.T, m *HolonManifest) {
				if len(m.Artifacts) != 3 {
					t.Errorf("expected 3 artifacts, got %d", len(m.Artifacts))
				}
				if m.Metadata == nil {
					t.Fatal("metadata should not be nil")
				}
				if m.Metadata["mode"] != "pr-fix" {
					t.Errorf("expected mode 'pr-fix', got %v", m.Metadata["mode"])
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var manifest HolonManifest
			err := json.Unmarshal([]byte(tt.jsonData), &manifest)
			if err != nil {
				t.Fatalf("Failed to unmarshal manifest: %v\nJSON: %s", err, tt.jsonData)
			}

			// Run validation
			if tt.validate != nil {
				tt.validate(t, &manifest)
			}
		})
	}
}

// TestHolonManifest_MarshalRoundTrip tests that manifests can be marshaled
// and unmarshaled without data loss.
func TestHolonManifest_MarshalRoundTrip(t *testing.T) {
	original := HolonManifest{
		Status:  "completed",
		Outcome: "success",
		Duration: Duration("12.5s"),
		Artifacts: Artifacts([]string{"diff.patch", "summary.md"}),
		Metadata: map[string]interface{}{
			"agent":   "agent-claude",
			"version": "v0.7.0",
			"mode":    "solve",
			"engine": map[string]interface{}{
				"model":   "claude-sonnet-4-5-20250929",
				"version": "20250929",
			},
		},
	}

	// Marshal
	data, err := json.Marshal(&original)
	if err != nil {
		t.Fatalf("Failed to marshal: %v", err)
	}

	// Unmarshal
	var unmarshaled HolonManifest
	err = json.Unmarshal(data, &unmarshaled)
	if err != nil {
		t.Fatalf("Failed to unmarshal: %v", err)
	}

	// Verify fields match
	if unmarshaled.Status != original.Status {
		t.Errorf("Status mismatch: got %s, want %s", unmarshaled.Status, original.Status)
	}
	if unmarshaled.Outcome != original.Outcome {
		t.Errorf("Outcome mismatch: got %s, want %s", unmarshaled.Outcome, original.Outcome)
	}
	if unmarshaled.Duration != original.Duration {
		t.Errorf("Duration mismatch: got %s, want %s", unmarshaled.Duration, original.Duration)
	}
	if len(unmarshaled.Artifacts) != len(original.Artifacts) {
		t.Errorf("Artifacts count mismatch: got %d, want %d", len(unmarshaled.Artifacts), len(original.Artifacts))
	}

	// Check metadata preserved
	if unmarshaled.Metadata == nil {
		t.Fatal("Metadata should not be nil")
	}
	if unmarshaled.Metadata["agent"] != "agent-claude" {
		t.Errorf("Agent metadata mismatch: got %v, want 'agent-claude'", unmarshaled.Metadata["agent"])
	}
	if unmarshaled.Metadata["version"] != "v0.7.0" {
		t.Errorf("Version metadata mismatch: got %v, want 'v0.7.0'", unmarshaled.Metadata["version"])
	}

	// Check nested engine object preserved
	engine, ok := unmarshaled.Metadata["engine"].(map[string]interface{})
	if !ok {
		t.Fatal("Engine metadata should be a map")
	}
	if engine["model"] != "claude-sonnet-4-5-20250929" {
		t.Errorf("Engine model mismatch: got %v, want 'claude-sonnet-4-5-20250929'", engine["model"])
	}
}

// TestHolonManifest_MetadataPreservesComplexObjects tests that the metadata field
// can preserve complex nested objects (the key issue from PR #408).
func TestHolonManifest_MetadataPreservesComplexObjects(t *testing.T) {
	// This test ensures the fix for PR #408 / Issue #415
	// The Go type MUST support map[string]interface{} (not map[string]string)
	// to preserve nested objects like the engine metadata.
	jsonData := `{
		"status": "completed",
		"outcome": "success",
		"duration": "10.0s",
		"artifacts": ["diff.patch"],
		"metadata": {
			"agent": "agent-claude",
			"version": "v0.7.0",
			"engine": {
				"model": "claude-sonnet-4-5-20250929",
				"version": "20250929",
				"nested": {
					"deep": "value"
				}
			}
		}
	}`

	var manifest HolonManifest
	err := json.Unmarshal([]byte(jsonData), &manifest)
	if err != nil {
		t.Fatalf("Failed to unmarshal manifest with nested metadata: %v", err)
	}

	// Verify metadata is present
	if manifest.Metadata == nil {
		t.Fatal("metadata should not be nil")
	}

	// Verify nested engine object is preserved
	engine, ok := manifest.Metadata["engine"]
	if !ok {
		t.Fatal("metadata should contain 'engine' key")
	}

	// engine should be a map[string]interface{} (JSON object)
	engineMap, ok := engine.(map[string]interface{})
	if !ok {
		t.Fatalf("engine should be a map, got %T", engine)
	}

	// Verify deeply nested values are preserved
	if engineMap["model"] != "claude-sonnet-4-5-20250929" {
		t.Errorf("expected engine.model 'claude-sonnet-4-5-20250929', got %v", engineMap["model"])
	}

	nested, ok := engineMap["nested"].(map[string]interface{})
	if !ok {
		t.Fatal("nested should be a map")
	}
	if nested["deep"] != "value" {
		t.Errorf("expected nested.deep 'value', got %v", nested["deep"])
	}
}
