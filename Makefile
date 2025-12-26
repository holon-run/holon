.PHONY: build build-host test test-all clean run-example test-agent help release-build

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
	@mkdir -p $(BIN_DIR)
	go build $(LDFLAGS) -o $(BIN_DIR)/$(BINARY_NAME) ./cmd/holon

## release-build: Build binaries for multiple platforms
release-build:
	@echo "Building release binaries..."
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
	@if command -v gotestfmt > /dev/null 2>&1; then \
		go test ./... -json -v 2>&1 | gotestfmt; \
	else \
		echo "gotestfmt not found, using plain output (install: go install github.com/gotesttools/gotestfmt/v2/cmd/gotestfmt@latest)"; \
		go test ./... -v; \
	fi

## test-raw: Run Go tests without gotestfmt (plain output)
test-raw:
	@echo "Running Go tests with plain output..."
	go test ./... -v

## test-agent: Run agent TypeScript tests
test-agent:
	@echo "Running TypeScript agent tests..."
	cd $(AGENT_DIR) && npm install && npm run build && npm test

## test-all: Run all project tests (Go + agent)
test-all: test-agent test-go

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

## test-integration: Run integration tests with structured output (requires Docker)
test-integration: build
	@echo "Running integration tests..."
	@if command -v gotestfmt > /dev/null 2>&1; then \
		go test ./tests/integration/... -json -v 2>&1 | gotestfmt; \
	else \
		echo "gotestfmt not found, using plain output (install: go install github.com/gotesttools/gotestfmt/v2/cmd/gotestfmt@latest)"; \
		go test ./tests/integration/... -v; \
	fi

## run-example: Run the fix-bug example (requires ANTHROPIC_API_KEY)
run-example: build
	@echo "Running fix-bug example..."
	./$(BIN_DIR)/$(BINARY_NAME) run --spec examples/fix-bug.yaml --image golang:1.22 --workspace . --out ./holon-output-fix

## help: Display help information
help:
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -E '^##' Makefile | sed -e 's/## //'
