package github

import (
	"log"

	"github.com/holon-run/holon/pkg/publisher"
)

func init() {
	// Register the GitHub publisher
	if err := publisher.Register(NewGitHubPublisher()); err != nil {
		// Log the error and allow the application to start without the GitHub publisher.
		// The application should handle the missing publisher gracefully at runtime.
		log.Printf("github publisher registration failed: %v", err)
	}
}
