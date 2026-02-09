package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	gh "github.com/holon-run/holon/pkg/github"
	holonlog "github.com/holon-run/holon/pkg/log"
)

type runtimeEnvOptions struct {
	IncludeClaudeSettingsFallback bool
	IncludeGitHubActorIdentity    bool
	ResolveGitHubActor            func(context.Context, string) *gh.ActorInfo
	IncludeHolonClaudeConfig      bool
}

func applyRuntimeAutoEnv(ctx context.Context, envVars map[string]string, opts runtimeEnvOptions) {
	anthropicFallback := map[string]string{}
	if opts.IncludeClaudeSettingsFallback {
		fallback, err := readAnthropicEnvFromClaudeSettingsFile()
		if err != nil {
			holonlog.Debug("failed to read Anthropic fallback from Claude settings", "error", err)
		} else {
			anthropicFallback = fallback
		}
	}

	applyAnthropicAutoEnv(envVars, anthropicFallback)
	githubToken := applyGitHubTokenAutoEnv(envVars)
	applyGitHubActorAutoEnv(ctx, envVars, githubToken, opts)
	if opts.IncludeHolonClaudeConfig {
		applyHolonClaudeAutoEnv(envVars)
	}
}

func applyAnthropicAutoEnv(envVars map[string]string, fallback map[string]string) {
	anthropicKey := strings.TrimSpace(os.Getenv("ANTHROPIC_AUTH_TOKEN"))
	if anthropicKey == "" {
		anthropicKey = strings.TrimSpace(os.Getenv("ANTHROPIC_API_KEY"))
		if anthropicKey != "" {
			holonlog.Warn("using legacy ANTHROPIC_API_KEY; consider migrating to ANTHROPIC_AUTH_TOKEN")
		}
	}
	if anthropicKey == "" {
		anthropicKey = strings.TrimSpace(fallback["ANTHROPIC_AUTH_TOKEN"])
	}
	if anthropicKey == "" {
		anthropicKey = strings.TrimSpace(fallback["ANTHROPIC_API_KEY"])
	}
	if anthropicKey != "" {
		envVars["ANTHROPIC_AUTH_TOKEN"] = anthropicKey
		envVars["ANTHROPIC_API_KEY"] = anthropicKey
	}

	anthropicURL := strings.TrimSpace(os.Getenv("ANTHROPIC_BASE_URL"))
	if anthropicURL == "" {
		anthropicURL = strings.TrimSpace(os.Getenv("ANTHROPIC_API_URL"))
	}
	if anthropicURL == "" {
		anthropicURL = strings.TrimSpace(fallback["ANTHROPIC_BASE_URL"])
	}
	if anthropicURL == "" {
		anthropicURL = strings.TrimSpace(fallback["ANTHROPIC_API_URL"])
	}
	if anthropicURL != "" {
		envVars["ANTHROPIC_BASE_URL"] = anthropicURL
		envVars["ANTHROPIC_API_URL"] = anthropicURL
	}
}

func applyGitHubTokenAutoEnv(envVars map[string]string) string {
	if token := strings.TrimSpace(os.Getenv("HOLON_GITHUB_TOKEN")); token != "" {
		envVars["HOLON_GITHUB_TOKEN"] = token
		envVars["GITHUB_TOKEN"] = token
		envVars["GH_TOKEN"] = token
		return token
	}
	if token := strings.TrimSpace(os.Getenv("GITHUB_TOKEN")); token != "" {
		envVars["GITHUB_TOKEN"] = token
		envVars["GH_TOKEN"] = token
		return token
	}
	if token := strings.TrimSpace(os.Getenv("GH_TOKEN")); token != "" {
		envVars["GITHUB_TOKEN"] = token
		envVars["GH_TOKEN"] = token
		return token
	}
	return ""
}

func applyGitHubActorAutoEnv(ctx context.Context, envVars map[string]string, githubToken string, opts runtimeEnvOptions) {
	actorInfoProvided := false
	if actorLogin := strings.TrimSpace(os.Getenv("HOLON_ACTOR_LOGIN")); actorLogin != "" {
		envVars["HOLON_ACTOR_LOGIN"] = actorLogin
		actorInfoProvided = true

		actorType := ""
		if actorType = strings.TrimSpace(os.Getenv("HOLON_ACTOR_TYPE")); actorType != "" {
			envVars["HOLON_ACTOR_TYPE"] = actorType
		}
		if actorSource := strings.TrimSpace(os.Getenv("HOLON_ACTOR_SOURCE")); actorSource != "" {
			envVars["HOLON_ACTOR_SOURCE"] = actorSource
		}
		if actorAppSlug := strings.TrimSpace(os.Getenv("HOLON_ACTOR_APP_SLUG")); actorAppSlug != "" {
			envVars["HOLON_ACTOR_APP_SLUG"] = actorAppSlug
		}
		holonlog.Info("using explicit github actor identity", "login", actorLogin, "type", actorType)
	}

	if !opts.IncludeGitHubActorIdentity || actorInfoProvided || githubToken == "" || opts.ResolveGitHubActor == nil {
		return
	}

	if actorType := strings.TrimSpace(os.Getenv("HOLON_ACTOR_TYPE")); actorType == "App" {
		holonlog.Info("skipping github actor identity lookup for App token without explicit login")
		return
	}

	actorInfo := opts.ResolveGitHubActor(ctx, githubToken)
	if actorInfo == nil {
		holonlog.Info("github actor identity lookup failed, continuing without identity")
		return
	}

	envVars["HOLON_ACTOR_LOGIN"] = actorInfo.Login
	envVars["HOLON_ACTOR_TYPE"] = actorInfo.Type
	if actorInfo.Source != "" {
		envVars["HOLON_ACTOR_SOURCE"] = actorInfo.Source
	}
	if actorInfo.AppSlug != "" {
		envVars["HOLON_ACTOR_APP_SLUG"] = actorInfo.AppSlug
	}
	holonlog.Info("github actor identity resolved", "login", actorInfo.Login, "type", actorInfo.Type)
}

func applyHolonClaudeAutoEnv(envVars map[string]string) {
	if driver := strings.TrimSpace(os.Getenv("HOLON_CLAUDE_DRIVER")); driver != "" {
		envVars["HOLON_CLAUDE_DRIVER"] = driver
	}
	if fixture := strings.TrimSpace(os.Getenv("HOLON_CLAUDE_MOCK_FIXTURE")); fixture != "" {
		envVars["HOLON_CLAUDE_MOCK_FIXTURE"] = fixture
	}
}

func readAnthropicEnvFromClaudeSettingsFile() (map[string]string, error) {
	home, err := os.UserHomeDir()
	if err != nil || strings.TrimSpace(home) == "" {
		if err == nil {
			return nil, fmt.Errorf("user home directory is empty")
		}
		return nil, err
	}
	settingsPath := filepath.Join(home, ".claude", "settings.json")
	return readAnthropicEnvFromClaudeSettings(settingsPath)
}

func readAnthropicEnvFromClaudeSettings(path string) (map[string]string, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var payload struct {
		Env map[string]string `json:"env"`
	}
	if err := json.Unmarshal(raw, &payload); err != nil {
		return nil, fmt.Errorf("failed to parse settings.json: %w", err)
	}

	result := map[string]string{}
	for _, key := range []string{
		"ANTHROPIC_AUTH_TOKEN",
		"ANTHROPIC_BASE_URL",
		"ANTHROPIC_API_KEY",
		"ANTHROPIC_API_URL",
	} {
		if v := strings.TrimSpace(payload.Env[key]); v != "" {
			result[key] = v
		}
	}
	return result, nil
}
