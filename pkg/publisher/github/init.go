package github

import (
	"github.com/holon-run/holon/pkg/publisher"
)

func init() {
	// Register the GitHub publisher
	if err := publisher.Register(NewGitHubPublisher()); err != nil {
		// In production, this should be handled more gracefully
		// For now, we'll just panic if registration fails
		panic(err)
	}
}
