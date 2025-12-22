.PHONY: build build-host test test-all clean run-example test-agent help

# Project variables
BINARY_NAME=holon
BIN_DIR=bin
GO_FILES=$(shell find . -type f -name '*.go')
AGENT_DIR=agents/claude

# Default target
all: build

## build: Build the holon runner CLI
build: build-host

## build-host: Build runner CLI for current OS/Arch
build-host:
	@echo "Building runner CLI..."
	@mkdir -p $(BIN_DIR)
	go build -o $(BIN_DIR)/$(BINARY_NAME) ./cmd/holon

## test: Run all project tests
test: test-agent
	@echo "Running Go tests..."
	go test ./... -v

## test-agent: Run agent TypeScript tests
test-agent:
	@echo "Running TypeScript agent tests..."
	cd $(AGENT_DIR) && npm install && npm run build && npm test

## clean: Remove build artifacts
clean:
	@echo "Cleaning up..."
	rm -rf $(BIN_DIR)
	rm -rf holon-output*

## test-integration: Run integration tests (requires Docker)
test-integration: build
	@echo "Running integration tests..."
	go test ./tests/integration/... -v

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
