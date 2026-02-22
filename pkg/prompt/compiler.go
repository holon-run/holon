package prompt

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"text/template"

	"gopkg.in/yaml.v3"
)

// Config represents the prompt configuration
type Config struct {
	Mode           string
	Language       string
	WorkingDir     string
	ContextFiles   []string
	ContextEntries []ContextEntry
	// GitHub actor identity fields (optional)
	ActorLogin   string // GitHub username or bot name
	ActorType    string // "User" or "App"
	ActorSource  string // "token" or "app"
	ActorAppSlug string // App slug if type is "App"
}

// Manifest represents the structure of manifest.yaml
type Manifest struct {
	Version  string `yaml:"version"`
	Defaults struct {
		Mode string `yaml:"mode"`
	} `yaml:"defaults"`
}

// ContextEntry represents a context file with optional description
type ContextEntry struct {
	Path        string
	Description string
}

// Compiler handles the assembly of prompts
type Compiler struct {
	assets fs.FS
}

// NewCompiler creates a new prompt compiler
func NewCompiler(assetsPath string) *Compiler {
	sub, err := AssetsFS()
	if err != nil {
		panic(err)
	}

	return &Compiler{
		assets: sub,
	}
}

// NewCompilerFromFS creates a compiler from a given FS (useful for testing or external loading)
func NewCompilerFromFS(assets fs.FS) *Compiler {
	return &Compiler{assets: assets}
}

func (c *Compiler) CompileSystemPrompt(cfg Config) (string, error) {
	resolvedCfg, err := c.resolveMode(cfg)
	if err != nil {
		return "", err
	}
	mode := resolvedCfg.Mode

	// 1. Load Common Contract (base layer, required)
	commonData, err := fs.ReadFile(c.assets, "contracts/common.md")
	if err != nil {
		return "", fmt.Errorf("failed to read common contract: %w", err)
	}

	// 2. Load Mode Contract (optional layer)
	modeContractPath := fmt.Sprintf("modes/%s/contract.md", mode)
	modeData, err := readOptionalFile(c.assets, modeContractPath)
	if err != nil {
		return "", err
	}

	// 3. Load Mode Overlay (optional layer)
	modeOverlayPath := fmt.Sprintf("modes/%s/overlay.md", mode)
	modeOverlayData, err := readOptionalFile(c.assets, modeOverlayPath)
	if err != nil {
		return "", err
	}

	// 4. Load Mode Context (optional layer, uses ContextEntries)
	modeContextPath := fmt.Sprintf("modes/%s/context.md", mode)
	modeContextData, err := readOptionalFile(c.assets, modeContextPath)
	if err != nil {
		return "", err
	}

	// 5. Combine layers in order: common + mode layers
	fullTemplate := string(commonData)

	if modeData != nil {
		fullTemplate += "\n\n" + string(modeData)
	}

	if modeOverlayData != nil {
		fullTemplate += "\n\n" + string(modeOverlayData)
	}

	if modeContextData != nil {
		fullTemplate += "\n\n" + string(modeContextData)
	}

	// 6. Template Execution
	tmpl, err := template.New("system").Parse(fullTemplate)
	if err != nil {
		return "", fmt.Errorf("failed to parse template: %w", err)
	}

	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, resolvedCfg); err != nil {
		return "", fmt.Errorf("failed to execute template: %w", err)
	}

	return buf.String(), nil
}

// CompileModeUserPrompt compiles mode-specific user prompt template at modes/<mode>/user.md.
func (c *Compiler) CompileModeUserPrompt(cfg Config) (string, error) {
	resolvedCfg, err := c.resolveMode(cfg)
	if err != nil {
		return "", err
	}

	path := fmt.Sprintf("modes/%s/user.md", resolvedCfg.Mode)
	data, err := fs.ReadFile(c.assets, path)
	if err != nil {
		return "", fmt.Errorf("failed to read mode user prompt %s: %w", path, err)
	}

	tmpl, err := template.New("mode-user").Parse(string(data))
	if err != nil {
		return "", fmt.Errorf("failed to parse mode user prompt template %s: %w", path, err)
	}

	var buf bytes.Buffer
	if err := tmpl.Execute(&buf, resolvedCfg); err != nil {
		return "", fmt.Errorf("failed to execute mode user prompt template %s: %w", path, err)
	}
	return buf.String(), nil
}

func (c *Compiler) resolveMode(cfg Config) (Config, error) {
	manifestData, err := fs.ReadFile(c.assets, "manifest.yaml")
	if err != nil {
		return Config{}, fmt.Errorf("failed to read manifest: %w", err)
	}
	var manifest Manifest
	if err := yaml.Unmarshal(manifestData, &manifest); err != nil {
		return Config{}, fmt.Errorf("failed to parse manifest: %w", err)
	}

	resolved := cfg
	if resolved.Mode == "" {
		resolved.Mode = manifest.Defaults.Mode
	}
	if resolved.Mode == "" {
		resolved.Mode = "run"
	}

	return resolved, nil
}

func readOptionalFile(assets fs.FS, path string) ([]byte, error) {
	data, err := fs.ReadFile(assets, path)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil
		}
		return nil, fmt.Errorf("failed to read optional file %s: %w", path, err)
	}
	return data, nil
}

// CompileUserPrompt assembles the user's task prompt (Goal + Context Filenames)
func (c *Compiler) CompileUserPrompt(goal string, contextFiles []string) (string, error) {
	var sb bytes.Buffer

	// 1. Goal
	sb.WriteString("### TASK GOAL\n")
	sb.WriteString(goal)
	sb.WriteString("\n")

	// 2. Context Files (Filenames only to keep prompt size manageable)
	if len(contextFiles) > 0 {
		sb.WriteString("\n\n### ADDITIONAL CONTEXT FILES\n")
		sb.WriteString("The following files provide additional context and are available at ${HOLON_INPUT_DIR}/context/:\n")
		for _, name := range contextFiles {
			sb.WriteString(fmt.Sprintf("- %s\n", name))
		}
	}

	return sb.String(), nil
}
