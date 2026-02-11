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

type Config struct {
	Version string `yaml:"version"`
	Agent   struct {
		ID      string `yaml:"id"`
		Profile string `yaml:"profile"`
	} `yaml:"agent"`
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
			return Resolution{}, err
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
		filepath.Join(agentHome, "workspace"),
	}
	for _, dir := range dirs {
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("failed to create directory %s: %w", dir, err)
		}
	}

	defaultFiles := map[string]string{
		filepath.Join(agentHome, "AGENT.md"):    "# Agent\n\nDefault agent persona.\n",
		filepath.Join(agentHome, "ROLE.md"):     "# Role\n\nDefault role definition.\n",
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
		if err := SaveConfig(agentHome, cfg); err != nil {
			return err
		}
	} else if err != nil {
		return fmt.Errorf("failed to stat %s: %w", cfgPath, err)
	}

	return nil
}

func ensureFile(path, content string) error {
	if _, err := os.Stat(path); err == nil {
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
	return cfg, nil
}
