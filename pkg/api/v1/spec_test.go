package v1

import (
	"testing"

	"gopkg.in/yaml.v3"
)

func TestHolonSpec_Unmarshal(t *testing.T) {
	yamlData := `
version: v1
kind: Holon
metadata:
  name: test-spec
context:
  workspace: /app
  env:
    FOO: bar
goal:
  description: fix the bug
output:
  artifacts:
    - path: manifest.json
      required: true
constraints:
  max_steps: 10
`
	var spec HolonSpec
	err := yaml.Unmarshal([]byte(yamlData), &spec)
	if err != nil {
		t.Fatalf("Unmarshal failed: %v", err)
	}

	if spec.Metadata.Name != "test-spec" {
		t.Errorf("Expected name 'test-spec', got %s", spec.Metadata.Name)
	}

	if spec.Context.Env["FOO"] != "bar" {
		t.Errorf("Expected env FOO='bar', got %s", spec.Context.Env["FOO"])
	}

	if len(spec.Output.Artifacts) != 1 {
		t.Errorf("Expected 1 artifact, got %d", len(spec.Output.Artifacts))
	}

	if spec.Constraints.MaxSteps != 10 {
		t.Errorf("Expected max_steps 10, got %d", spec.Constraints.MaxSteps)
	}
}
