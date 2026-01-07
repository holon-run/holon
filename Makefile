.PHONY: build build-host test test-all clean run-example test-agent help release-build validate-schema test-contract build-agent-bundle

# Project variables
BINARY_NAME=holon
BIN_DIR=bin
GO_FILES=$(shell find . -type f -name '*.go')
AGENT_DIR=agents/claude

# Version variables (injected via ldflags)
VERSION?=$(shell git describe --tags --always --dirty 2>/dev/null || echo "dev")
COMMIT?=$(shell git rev-parse --short HEAD 2>/dev/null || echo "unknown")
BUILD_DATE?=$(shell date -u +"%Y-%m-%dT%H:%M:%SZ" 2>/dev/null || echo "unknown")
LDFLAGS=-ldflags "-X main.Version=$(VERSION) -X main.Commit=$(COMMIT) -X main.BuildDate=$(BUILD_DATE)"

# Default target
all: build

## build: Build the holon runner CLI
build: build-host

## build-host: Build runner CLI for current OS/Arch with version info
build-host:
	@echo "Building runner CLI (Version: $(VERSION), Commit: $(COMMIT))..."
	@echo "Generating builtin skills..."
	@go generate ./pkg/builtin
	@mkdir -p $(BIN_DIR)
	go build $(LDFLAGS) -o $(BIN_DIR)/$(BINARY_NAME) ./cmd/holon

## release-build: Build binaries for multiple platforms
release-build:
	@echo "Building release binaries..."
	@echo "Generating builtin skills..."
	@go generate ./pkg/builtin
	@mkdir -p $(BIN_DIR)
	@echo "Building for linux/amd64..."
	GOOS=linux GOARCH=amd64 go build $(LDFLAGS) -o $(BIN_DIR)/$(BINARY_NAME)-linux-amd64 ./cmd/holon
	@echo "Building for darwin/amd64..."
	GOOS=darwin GOARCH=amd64 go build $(LDFLAGS) -o $(BIN_DIR)/$(BINARY_NAME)-darwin-amd64 ./cmd/holon
	@echo "Building for darwin/arm64..."
	GOOS=darwin GOARCH=arm64 go build $(LDFLAGS) -o $(BIN_DIR)/$(BINARY_NAME)-darwin-arm64 ./cmd/holon
	@echo "Release binaries built successfully"

## test: Run all project tests with structured output
test: test-go

## test-go: Run Go tests with structured output
test-go:
	@echo "Running Go tests..."
	@echo "Generating builtin skills..."
	@go generate ./pkg/builtin
	@if command -v gotestfmt > /dev/null 2>&1; then \
		go test ./... -json -v 2>&1 | gotestfmt; \
	else \
		echo "gotestfmt not found, using plain output (install: go install github.com/gotesttools/gotestfmt/v2/cmd/gotestfmt@latest)"; \
		go test ./... -v; \
	fi

## test-raw: Run Go tests without gotestfmt (plain output)
test-raw:
	@echo "Running Go tests with plain output..."
	@echo "Generating builtin skills..."
	@go generate ./pkg/builtin
	go test ./... -v

## test-unit: Run unit tests (non-integration) with structured output
test-unit:
	@echo "Running unit tests..."
	@echo "Generating builtin skills..."
	@go generate ./pkg/builtin
	@if command -v gotestfmt > /dev/null 2>&1; then \
		go test $$(go list ./... | grep -v '^github.com/holon-run/holon/tests/') -short -json -v 2>&1 | gotestfmt; \
	else \
		echo "gotestfmt not found, using plain output (install: go install github.com/gotesttools/gotestfmt/v2/cmd/gotestfmt@latest)"; \
		go test $$(go list ./... | grep -v '^github.com/holon-run/holon/tests/') -short -v; \
	fi

## test-unit-raw: Run unit tests (non-integration) without gotestfmt
test-unit-raw:
	@echo "Running unit tests with plain output..."
	@echo "Generating builtin skills..."
	@go generate ./pkg/builtin
	go test $$(go list ./... | grep -v '^github.com/holon-run/holon/tests/') -short -v

## test-agent: Run agent TypeScript tests
test-agent:
	@echo "Running TypeScript agent tests..."
	cd $(AGENT_DIR) && npm install && npm run build && npm test

## build-agent-bundle: Build agent bundle from repo sources
build-agent-bundle:
	@echo "Building agent bundle from repo sources..."
	@cd $(AGENT_DIR) && npm ci && npm run bundle

## test-all: Run all project tests (Go + agent)
test-all: test-agent test-go

## validate-schema: Validate JSON schema syntax
validate-schema:
	@echo "Validating run manifest JSON schema..."
	@which ajv > /dev/null 2>&1 || (echo "ajv not found. Install with: npm install -g ajv-cli" && exit 1)
	@ajv compile -s schemas/run.manifest.schema.json
	@echo "Schema validation passed"

## test-contract: Run manifest contract tests (backward compatibility)
test-contract:
	@echo "Running manifest contract tests..."
	@go test ./pkg/api/v1/... -v -run TestHolonManifest

## install-gotestfmt: Install gotestfmt tool for structured test output
install-gotestfmt:
	@echo "Installing gotestfmt..."
	@go install github.com/gotesttools/gotestfmt/v2/cmd/gotestfmt@latest
	@echo "gotestfmt installed successfully"

## clean: Remove build artifacts
clean:
	@echo "Cleaning up..."
	rm -rf $(BIN_DIR)
	rm -rf holon-output*
	rm -rf _testwork
	rm -rf $(AGENT_DIR)/dist

## test-integration: Run integration tests with structured output (requires Docker)
test-integration: build build-agent-bundle
	@echo "Running integration tests..."
	@if command -v gotestfmt > /dev/null 2>&1; then \
		go test ./tests/integration/... -json -v 2>&1 | gotestfmt; \
	else \
		echo "gotestfmt not found, using plain output (install: go install github.com/gotesttools/gotestfmt/v2/cmd/gotestfmt@latest)"; \
		go test ./tests/integration/... -v; \
	fi

## test-integration-raw: Run integration tests without gotestfmt (plain output)
test-integration-raw: build build-agent-bundle
	@echo "Running integration tests with plain output..."
	go test ./tests/integration/... -v

## test-integration-artifacts: Run integration tests and capture artifacts on failure
test-integration-artifacts: build build-agent-bundle
	@echo "Running integration tests with artifact capture..."
	@mkdir -p _testwork
	@go test ./tests/integration/... -v -work

## run-example: Run the fix-bug example (requires ANTHROPIC_API_KEY)
run-example: build
	@echo "Running fix-bug example..."
	./$(BIN_DIR)/$(BINARY_NAME) run --spec examples/fix-bug.yaml --image golang:1.22 --workspace . --output ./holon-output-fix

## help: Display help information
help:
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -E '^##' Makefile | sed -e 's/## //'
