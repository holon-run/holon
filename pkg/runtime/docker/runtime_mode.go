package docker

import (
	"fmt"
	"strings"
)

// RuntimeMode controls how agent code is sourced in container runtime.
type RuntimeMode string

const (
	// RuntimeModeProd uses the bundled agent code in composed images.
	RuntimeModeProd RuntimeMode = "prod"
	// RuntimeModeDev overlays bundled agent code with local dist output.
	RuntimeModeDev RuntimeMode = "dev"
)

func ParseRuntimeMode(s string) (RuntimeMode, error) {
	normalized := strings.ToLower(strings.TrimSpace(s))
	if normalized == "" {
		return RuntimeModeProd, nil
	}
	switch normalized {
	case "prod":
		return RuntimeModeProd, nil
	case "dev":
		return RuntimeModeDev, nil
	default:
		return "", fmt.Errorf("invalid runtime mode %q; must be one of: prod, dev", s)
	}
}

func (m RuntimeMode) String() string {
	return string(m)
}
