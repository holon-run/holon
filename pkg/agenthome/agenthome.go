package agenthome

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
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

const (
	TemplateRunDefault      = "run-default"
	TemplateSolveGitHub     = "solve-github"
	TemplateServeController = "serve-controller"
	DefaultTemplate         = TemplateServeController
)

type InitOptions struct {
	Template string
	Force    bool
}

type Config struct {
	Version       string         `yaml:"version"`
	Agent         AgentConfig    `yaml:"agent"`
	Subscriptions []Subscription `yaml:"subscriptions,omitempty"`
}

type AgentConfig struct {
	ID      string `yaml:"id"`
	Profile string `yaml:"profile"`
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
	return EnsureLayoutWithOptions(agentHome, InitOptions{
		Template: DefaultTemplate,
		Force:    false,
	})
}

func AvailableTemplates() []string {
	return []string{
		TemplateRunDefault,
		TemplateSolveGitHub,
		TemplateServeController,
	}
}

func EnsureLayoutWithOptions(agentHome string, opts InitOptions) error {
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

	template := strings.TrimSpace(opts.Template)
	if template == "" {
		template = DefaultTemplate
	}
	personaTemplate, ok := personaTemplates[template]
	if !ok {
		return fmt.Errorf("unsupported template %q (supported: %s)", template, strings.Join(AvailableTemplates(), ", "))
	}

	for name, content := range personaTemplate {
		path := filepath.Join(agentHome, name)
		if opts.Force {
			if err := writeFile(path, content); err != nil {
				return err
			}
			continue
		}
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

func writeFile(path, content string) error {
	if info, err := os.Stat(path); err == nil {
		if !info.Mode().IsRegular() {
			return fmt.Errorf("path exists but is not a regular file: %s", path)
		}
	} else if !os.IsNotExist(err) {
		return fmt.Errorf("failed to stat %s: %w", path, err)
	}
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		return fmt.Errorf("failed to write %s: %w", path, err)
	}
	return nil
}

var personaTemplates = map[string]map[string]string{
	TemplateRunDefault: {
		"AGENT.md":    "# Agent\n\nYou are a focused execution agent for one-off tasks.\nPrioritize correctness, concise communication, and deterministic outputs.\n",
		"ROLE.md":     "# ROLE: EXECUTOR\n\nExecute assigned tasks end-to-end in this run.\nDo not assume long-lived controller responsibilities.\n",
		"IDENTITY.md": "# Identity\n\nDefault runtime identity for ad-hoc run workflows.\n",
		"SOUL.md":     "# Soul\n\nPragmatic, rigorous, and action-oriented.\n",
	},
	TemplateSolveGitHub: {
		"AGENT.md":    "# Agent\n\nYou solve GitHub issues and PR feedback with clear validation and publish-ready outputs.\n",
		"ROLE.md":     "# ROLE: GITHUB_SOLVER\n\nAnalyze issue/PR context, implement fixes, run relevant tests, and produce a mergeable result.\n",
		"IDENTITY.md": "# Identity\n\nIssue-solving specialist for repository automation workflows.\n",
		"SOUL.md":     "# Soul\n\nBe explicit about tradeoffs, verification, and residual risks.\n",
	},
	TemplateServeController: {
		"AGENT.md":    "# Agent\n\nPersistent controller persona for long-running event-driven automation.\n",
		"ROLE.md":     "# ROLE: PM\n\nYou are the persistent PM controller for this agent home.\nContinuously plan, prioritize, and drive execution via GitHub workflows.\n",
		"IDENTITY.md": "# Identity\n\nController identity for continuous repository operations.\n",
		"SOUL.md":     "# Soul\n\nMaintain continuity, ownership, and disciplined follow-through.\n",
	},
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
	return cfg, nil
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
