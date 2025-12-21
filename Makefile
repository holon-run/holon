.PHONY: build build-adapter build-host test test-all clean run-example ensure-adapter-image test-adapter help

# Project variables
BINARY_NAME=holon
BIN_DIR=bin
GO_FILES=$(shell find . -type f -name '*.go')
ADAPTER_TS_DIR=images/adapter-claude-ts

# Default target
all: build

## build: Build the holon host CLI
build: build-host

## build-host: Build host CLI for current OS/Arch
build-host:
	@echo "Building host CLI..."
	@mkdir -p $(BIN_DIR)
	go build -o $(BIN_DIR)/$(BINARY_NAME) ./cmd/holon

## build-adapter-image: Build the Claude adapter Docker image
build-adapter-image:
	@echo "Building Claude adapter image (TypeScript)..."
	docker build -t holon-adapter-claude-ts ./images/adapter-claude-ts

## ensure-adapter-image: Ensure the Claude adapter Docker image exists
ensure-adapter-image:
	@echo "Checking for holon-adapter-claude-ts image..."
	@if ! docker image inspect holon-adapter-claude-ts >/dev/null 2>&1; then \
		echo "Image not found, building holon-adapter-claude-ts..."; \
		$(MAKE) build-adapter-image; \
	else \
		echo "holon-adapter-claude-ts image found."; \
	fi

## test: Run all project tests
test: test-adapter
	@echo "Running Go tests..."
	go test ./... -v

## test-adapter: Run adapter TypeScript checks
test-adapter:
	@echo "Running TypeScript adapter checks..."
	cd $(ADAPTER_TS_DIR) && npm install && npm run build

## clean: Remove build artifacts
clean:
	@echo "Cleaning up..."
	rm -rf $(BIN_DIR)
	rm -rf holon-output*

## test-integration: Run integration tests (requires Docker)
test-integration: build ensure-adapter-image
	@echo "Running integration tests..."
	go test ./tests/integration/... -v

## run-example: Run the fix-bug example (requires ANTHROPIC_API_KEY)
run-example: build ensure-adapter-image
	@echo "Running fix-bug example..."
	./$(BIN_DIR)/$(BINARY_NAME) run --spec examples/fix-bug.yaml --image golang:1.22 --workspace . --out ./holon-output-fix

## help: Display help information
help:
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -E '^##' Makefile | sed -e 's/## //'
