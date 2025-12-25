package git

import (
	"github.com/holon-run/holon/pkg/publisher"
)

func init() {
	publisher.Register(NewPublisher())
}
