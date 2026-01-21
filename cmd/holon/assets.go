package main

import (
	"embed"
	"io/fs"
)

//go:embed templates/*.yml
var workflowTemplates embed.FS

// getWorkflowTemplate reads the holon-trigger.yml workflow template from embedded assets.
// Returns the template content or an error if it cannot be read.
func getWorkflowTemplate() ([]byte, error) {
	// Read the workflow template from the embedded filesystem
	content, err := fs.ReadFile(workflowTemplates, "templates/workflow.yml")
	if err != nil {
		return nil, err
	}
	return content, nil
}
