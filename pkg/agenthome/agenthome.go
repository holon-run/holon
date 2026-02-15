package agenthome

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

var agentIDPattern = regexp.MustCompile(`^[a-zA-Z0-9_-]+$`)

type ResolveOptions struct {
	AgentID          string
	AgentHome        string
	Command          string
	EphemeralAllowed bool
}

type Resolution struct {
	AgentID   string
	AgentHome string
	Ephemeral bool
}

type Config struct {
	Version       string         `yaml:"version"`
	Agent         AgentConfig    `yaml:"agent"`
	Runtime       RuntimeConfig  `yaml:"runtime,omitempty"`
	Subscriptions []Subscription `yaml:"subscriptions,omitempty"`
}

type AgentConfig struct {
	ID      string `yaml:"id"`
	Profile string `yaml:"profile"`
}

type RuntimeConfig struct {
	Mounts []RuntimeMount `yaml:"mounts,omitempty"`
}

type RuntimeMount struct {
	Path string `yaml:"path"`
	Mode string `yaml:"mode,omitempty"` // ro (default) | rw
}

type Subscription struct {
	GitHub *GitHubSubscription `yaml:"github,omitempty"`
}

type GitHubSubscription struct {
	Repos     []string                    `yaml:"repos,omitempty"`
	Transport GitHubSubscriptionTransport `yaml:"transport,omitempty"`
}

type GitHubSubscriptionTransport struct {
	Mode         string `yaml:"mode,omitempty"` // auto, gh_forward, websocket
	WebsocketURL string `yaml:"websocket_url,omitempty"`
}

func ValidateAgentID(id string) error {
	trimmed := strings.TrimSpace(id)
	if trimmed == "" {
		return errors.New("agent id cannot be empty")
	}
	if !agentIDPattern.MatchString(trimmed) {
		return fmt.Errorf("invalid agent id %q: only [a-zA-Z0-9_-] is allowed", trimmed)
	}
	return nil
}

func DefaultRoot() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("failed to resolve user home: %w", err)
	}
	return filepath.Join(home, ".holon", "agents"), nil
}

func Resolve(opts ResolveOptions) (Resolution, error) {
	id := strings.TrimSpace(opts.AgentID)
	home := strings.TrimSpace(opts.AgentHome)

	switch {
	case home != "":
		absHome, err := filepath.Abs(home)
		if err != nil {
			return Resolution{}, fmt.Errorf("failed to resolve --agent-home: %w", err)
		}
		if id == "" {
			id = filepath.Base(absHome)
		}
		if err := ValidateAgentID(id); err != nil {
			return Resolution{}, fmt.Errorf("invalid agent id derived from --agent-home %q: %w", absHome, err)
		}
		return Resolution{AgentID: id, AgentHome: absHome}, nil
	case id != "":
		if err := ValidateAgentID(id); err != nil {
			return Resolution{}, err
		}
		root, err := DefaultRoot()
		if err != nil {
			return Resolution{}, err
		}
		return Resolution{AgentID: id, AgentHome: filepath.Join(root, id)}, nil
	default:
		if strings.TrimSpace(opts.Command) == "serve" {
			root, err := DefaultRoot()
			if err != nil {
				return Resolution{}, err
			}
			return Resolution{AgentID: "main", AgentHome: filepath.Join(root, "main")}, nil
		}
		if opts.EphemeralAllowed {
			tmp, err := os.MkdirTemp("", "holon-agent-*")
			if err != nil {
				return Resolution{}, fmt.Errorf("failed to create temporary agent home: %w", err)
			}
			return Resolution{
				AgentID:   filepath.Base(tmp),
				AgentHome: tmp,
				Ephemeral: true,
			}, nil
		}
		root, err := DefaultRoot()
		if err != nil {
			return Resolution{}, err
		}
		return Resolution{AgentID: "main", AgentHome: filepath.Join(root, "main")}, nil
	}
}

func EnsureLayout(agentHome string) error {
	dirs := []string{
		agentHome,
		filepath.Join(agentHome, "state"),
		filepath.Join(agentHome, "sessions"),
		filepath.Join(agentHome, "channels"),
		filepath.Join(agentHome, "jobs"),
	}
	for _, dir := range dirs {
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("failed to create directory %s: %w", dir, err)
		}
	}

	defaultFiles := map[string]string{
		filepath.Join(agentHome, "AGENT.md"):    "# Agent\n\nDefault agent persona.\n",
		filepath.Join(agentHome, "ROLE.md"):     "# ROLE: PM\n\nYou are the persistent PM controller for this agent home.\nContinuously plan, prioritize, and drive execution via GitHub workflows.\n",
		filepath.Join(agentHome, "IDENTITY.md"): "# Identity\n\nDefault identity definition.\n",
		filepath.Join(agentHome, "SOUL.md"):     "# Soul\n\nDefault principles.\n",
	}
	for path, content := range defaultFiles {
		if err := ensureFile(path, content); err != nil {
			return err
		}
	}

	cfgPath := filepath.Join(agentHome, "agent.yaml")
	if _, err := os.Stat(cfgPath); os.IsNotExist(err) {
		cfg := Config{Version: "v1"}
		cfg.Agent.ID = filepath.Base(agentHome)
		cfg.Agent.Profile = "default"
		// Set default subscription with auto transport mode
		cfg.Subscriptions = []Subscription{
			{
				GitHub: &GitHubSubscription{
					Repos: []string{},
					Transport: GitHubSubscriptionTransport{
						Mode: "auto",
					},
				},
			},
		}
		if err := SaveConfig(agentHome, cfg); err != nil {
			return err
		}
	} else if err != nil {
		return fmt.Errorf("failed to stat %s: %w", cfgPath, err)
	} else {
		cfg, err := LoadConfig(agentHome)
		if err != nil {
			return fmt.Errorf("existing agent config is invalid: %w", err)
		}
		if cfg.Version != "v1" {
			return fmt.Errorf("unsupported agent config version %q in %s", cfg.Version, cfgPath)
		}
	}

	return nil
}

func ensureFile(path, content string) error {
	if info, err := os.Stat(path); err == nil {
		if !info.Mode().IsRegular() {
			return fmt.Errorf("path exists but is not a regular file: %s", path)
		}
		return nil
	} else if !os.IsNotExist(err) {
		return fmt.Errorf("failed to stat %s: %w", path, err)
	}
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		return fmt.Errorf("failed to write %s: %w", path, err)
	}
	return nil
}

func SaveConfig(agentHome string, cfg Config) error {
	data, err := yaml.Marshal(&cfg)
	if err != nil {
		return fmt.Errorf("failed to marshal agent config: %w", err)
	}
	cfgPath := filepath.Join(agentHome, "agent.yaml")
	if err := os.WriteFile(cfgPath, data, 0644); err != nil {
		return fmt.Errorf("failed to write %s: %w", cfgPath, err)
	}
	return nil
}

func LoadConfig(agentHome string) (Config, error) {
	cfgPath := filepath.Join(agentHome, "agent.yaml")
	data, err := os.ReadFile(cfgPath)
	if err != nil {
		return Config{}, fmt.Errorf("failed to read %s: %w", cfgPath, err)
	}
	var cfg Config
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return Config{}, fmt.Errorf("failed to parse %s: %w", cfgPath, err)
	}
	if strings.TrimSpace(cfg.Version) == "" {
		return Config{}, fmt.Errorf("invalid config %s: version is required", cfgPath)
	}
	if strings.TrimSpace(cfg.Agent.ID) == "" {
		return Config{}, fmt.Errorf("invalid config %s: agent.id is required", cfgPath)
	}
	// Validate subscriptions if present
	if err := validateSubscriptions(cfg); err != nil {
		return Config{}, fmt.Errorf("invalid subscriptions in %s: %w", cfgPath, err)
	}
	normalizedMounts, err := normalizeRuntimeMounts(cfg.Runtime.Mounts)
	if err != nil {
		return Config{}, fmt.Errorf("invalid runtime.mounts in %s: %w", cfgPath, err)
	}
	cfg.Runtime.Mounts = normalizedMounts
	return cfg, nil
}

func normalizeRuntimeMounts(mounts []RuntimeMount) ([]RuntimeMount, error) {
	if len(mounts) == 0 {
		return nil, nil
	}

	normalized := make([]RuntimeMount, 0, len(mounts))
	seen := make(map[string]int, len(mounts))

	for i, mount := range mounts {
		rawPath := strings.TrimSpace(mount.Path)
		if rawPath == "" {
			return nil, fmt.Errorf("mounts[%d].path is required", i)
		}
		if !filepath.IsAbs(rawPath) {
			return nil, fmt.Errorf("mounts[%d].path must be an absolute path: %q", i, rawPath)
		}

		cleaned := filepath.Clean(rawPath)
		if isFilesystemRoot(cleaned) {
			return nil, fmt.Errorf("mounts[%d].path cannot be filesystem root: %q", i, cleaned)
		}

		info, err := os.Stat(cleaned)
		if err != nil {
			if os.IsNotExist(err) {
				return nil, fmt.Errorf("mounts[%d].path does not exist: %q", i, cleaned)
			}
			return nil, fmt.Errorf("failed to stat mounts[%d].path %q: %w", i, cleaned, err)
		}
		if !info.IsDir() && !info.Mode().IsRegular() {
			return nil, fmt.Errorf("mounts[%d].path must be a file or directory: %q", i, cleaned)
		}

		resolved, err := filepath.EvalSymlinks(cleaned)
		if err != nil {
			return nil, fmt.Errorf("failed to resolve symlinks for mounts[%d].path %q: %w", i, cleaned, err)
		}
		resolvedAbs, err := filepath.Abs(resolved)
		if err != nil {
			return nil, fmt.Errorf("failed to resolve absolute path for mounts[%d].path %q: %w", i, resolved, err)
		}

		mode := strings.ToLower(strings.TrimSpace(mount.Mode))
		if mode == "" {
			mode = "ro"
		}
		if mode != "ro" && mode != "rw" {
			return nil, fmt.Errorf("mounts[%d].mode must be ro or rw: %q", i, mount.Mode)
		}

		if seenIdx, exists := seen[resolvedAbs]; exists {
			return nil, fmt.Errorf("mounts[%d].path duplicates mounts[%d].path after normalization: %q", i, seenIdx, resolvedAbs)
		}
		for prevPath, prevIdx := range seen {
			if pathOverlaps(resolvedAbs, prevPath) {
				return nil, fmt.Errorf("mounts[%d].path conflicts with mounts[%d].path (overlapping paths): %q vs %q", i, prevIdx, resolvedAbs, prevPath)
			}
		}
		seen[resolvedAbs] = i
		normalized = append(normalized, RuntimeMount{
			Path: resolvedAbs,
			Mode: mode,
		})
	}

	// Keep order deterministic for diagnostics/logging.
	sort.SliceStable(normalized, func(i, j int) bool {
		return normalized[i].Path < normalized[j].Path
	})
	return normalized, nil
}

func pathOverlaps(a, b string) bool {
	if a == b {
		return true
	}
	relAB, err := filepath.Rel(a, b)
	if err == nil && relAB != "." && relAB != ".." && !strings.HasPrefix(relAB, ".."+string(filepath.Separator)) {
		return true
	}
	relBA, err := filepath.Rel(b, a)
	if err == nil && relBA != "." && relBA != ".." && !strings.HasPrefix(relBA, ".."+string(filepath.Separator)) {
		return true
	}
	return false
}

func isFilesystemRoot(path string) bool {
	clean := filepath.Clean(path)
	if clean == string(filepath.Separator) {
		return true
	}
	volume := filepath.VolumeName(clean)
	return volume != "" && clean == volume+string(filepath.Separator)
}

func validateSubscriptions(cfg Config) error {
	for i, sub := range cfg.Subscriptions {
		if sub.GitHub != nil {
			// Validate repo format
			for _, repo := range sub.GitHub.Repos {
				if strings.TrimSpace(repo) == "" {
					return fmt.Errorf("subscription[%d].github.repos contains empty repo", i)
				}
				parts := strings.Split(repo, "/")
				if len(parts) != 2 {
					return fmt.Errorf("subscription[%d].github.repos: invalid repo format %q (expected owner/repo)", i, repo)
				}
				if strings.TrimSpace(parts[0]) == "" || strings.TrimSpace(parts[1]) == "" {
					return fmt.Errorf("subscription[%d].github.repos: invalid repo format %q (expected owner/repo)", i, repo)
				}
			}
			// Validate transport mode
			mode := strings.TrimSpace(sub.GitHub.Transport.Mode)
			if mode == "" {
				mode = "auto"
			}
			if mode != "auto" && mode != "gh_forward" && mode != "websocket" {
				return fmt.Errorf("subscription[%d].github.transport.mode: invalid mode %q (expected auto, gh_forward, or websocket)", i, mode)
			}
			if mode == "websocket" && strings.TrimSpace(sub.GitHub.Transport.WebsocketURL) == "" {
				return fmt.Errorf("subscription[%d].github.transport.websocket_url is required when mode=websocket", i)
			}
		}
	}
	return nil
}
