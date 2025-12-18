.PHONY: build build-adapter build-host test clean run-example help

# Project variables
BINARY_NAME=holon
BIN_DIR=bin
GO_FILES=$(shell find . -type f -name '*.go')

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
	@echo "Building Claude adapter image..."
	docker build -t holon-adapter-claude ./images/adapter-claude

## test: Run all project tests
test:
	@echo "Running tests..."
	go test ./... -v

## clean: Remove build artifacts
clean:
	@echo "Cleaning up..."
	rm -rf $(BIN_DIR)
	rm -rf holon-out*

## run-example: Run the fix-bug example (requires ANTHROPIC_API_KEY)
run-example: build
	@echo "Running fix-bug example..."
	./$(BIN_DIR)/$(BINARY_NAME) run --spec examples/fix-bug.yaml --image golang:1.22 --workspace . --out ./holon-out-fix

## help: Display help information
help:
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -E '^##' Makefile | sed -e 's/## //'
