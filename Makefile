.PHONY: build build-adapter build-host test test-all clean run-example ensure-adapter-image test-adapter venv-adapter help

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

## ensure-adapter-image: Ensure the Claude adapter Docker image exists
ensure-adapter-image:
	@echo "Checking for holon-adapter-claude image..."
	@if ! docker image inspect holon-adapter-claude >/dev/null 2>&1; then \
		echo "Image not found, building holon-adapter-claude..."; \
		$(MAKE) build-adapter-image; \
	else \
		echo "holon-adapter-claude image found."; \
	fi

# Adapter variables
ADAPTER_DIR=images/adapter-claude
ADAPTER_VENV=$(ADAPTER_DIR)/venv
ADAPTER_PYTHON=$(ADAPTER_VENV)/bin/python3
ADAPTER_PIP=$(ADAPTER_VENV)/bin/pip

## venv-adapter: Create Python virtual environment for adapter tests
venv-adapter:
	@echo "Setting up Python virtual environment for adapter..."
	@if [ ! -d "$(ADAPTER_VENV)" ]; then \
		python3 -m venv $(ADAPTER_VENV); \
	fi
	@$(ADAPTER_PIP) install -q -r $(ADAPTER_DIR)/requirements.txt

## test-adapter: Run adapter Python tests
test-adapter: venv-adapter
	@echo "Running Claude adapter tests..."
	@$(ADAPTER_PYTHON) $(ADAPTER_DIR)/run_tests.py
	@$(ADAPTER_PYTHON) -m pytest $(ADAPTER_DIR)/test_adapter.py -v

## test: Run all project tests
test: test-adapter
	@echo "Running Go tests..."
	go test ./... -v

## clean: Remove build artifacts
clean:
	@echo "Cleaning up..."
	rm -rf $(BIN_DIR)
	rm -rf holon-output*

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
