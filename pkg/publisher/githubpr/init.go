package githubpr

import (
	"github.com/holon-run/holon/pkg/publisher"
)

func init() {
	publisher.Register(NewPRPublisher())
}
