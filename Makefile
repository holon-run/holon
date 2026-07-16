.PHONY: help web web-ci transport-types transport-types-check build all test test-concurrent test-concurrent-repeat test-live fmt fmt-check lint check ci run clean

WEB_DIR := web-gui/app
OPENAPI_TOOLS_DIR := web-gui/openapi-tools
CONCURRENT_REPEATS ?= 3
CONCURRENT_LIFECYCLE_TESTS := \
	runtime_tasks \
	runtime_waiting_and_reactivation \
	runtime_waiting_and_delivery_regressions \
	http_events \
	http_tasks
CONCURRENT_TESTS := $(CONCURRENT_LIFECYCLE_TESTS) wt204_parallel_worktree_workflow

help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

web: ## Build the web GUI (requires Node.js 24). Produces web-gui/app/dist
	@if [ -s "$$HOME/.nvm/nvm.sh" ]; then . "$$HOME/.nvm/nvm.sh" && nvm use; fi; \
	cd $(WEB_DIR) && npm ci && npm run build

web-ci: ## Test and build the web GUI with one clean dependency install
	@if [ -s "$$HOME/.nvm/nvm.sh" ]; then . "$$HOME/.nvm/nvm.sh" && nvm use; fi; \
	cd $(OPENAPI_TOOLS_DIR) && npm ci && npm run check && \
	cd ../../$(WEB_DIR) && npm ci && npm test && npm run build

transport-types: ## Refresh OpenAPI and generated TypeScript transport types
	cargo test --test openapi_snapshot refresh_openapi_snapshot -- --ignored
	@if [ -s "$$HOME/.nvm/nvm.sh" ]; then . "$$HOME/.nvm/nvm.sh" && nvm use; fi; \
	cd $(OPENAPI_TOOLS_DIR) && npm ci && npm run generate

transport-types-check: ## Check OpenAPI and generated TypeScript transport type drift
	cargo test --test openapi_snapshot
	@if [ -s "$$HOME/.nvm/nvm.sh" ]; then . "$$HOME/.nvm/nvm.sh" && nvm use; fi; \
	cd $(OPENAPI_TOOLS_DIR) && npm ci && npm run check

build: ## Build all Rust targets (cargo build --all-targets)
	cargo build --all-targets

all: web build ## Build everything: web GUI then Rust

test: ## Run the full Rust test suite serially
	cargo test --all-targets -- --test-threads=1

test-concurrent: ## Run runtime lifecycle integration tests with Rust's default test threads
	@set -eu; \
	for test_target in $(CONCURRENT_TESTS); do \
		cargo test --test "$$test_target"; \
	done

test-concurrent-repeat: ## Repeat core concurrent lifecycle tests (CONCURRENT_REPEATS=3)
	@set -eu; \
	repeat=1; \
	while [ "$$repeat" -le "$(CONCURRENT_REPEATS)" ]; do \
		echo "Concurrent lifecycle test pass $$repeat/$(CONCURRENT_REPEATS)"; \
		for test_target in $(CONCURRENT_LIFECYCLE_TESTS); do \
			cargo test --test "$$test_target"; \
		done; \
		repeat=$$((repeat + 1)); \
	done

test-live: ## Run live-provider tests
	cargo test live_ -- --nocapture

fmt:
	cargo fmt

fmt-check: ## Check formatting without modifying files
	cargo fmt --all -- --check

lint: ## Run clippy
	cargo clippy --all-targets

check: ## Quick local check (formatting + clippy + compile check)
	RUSTFLAGS="-D warnings" cargo check --all-targets

ci: web-ci fmt-check lint build test ## Run the full CI checks locally

run:
	cargo run -- serve

clean: ## Remove Rust build artifacts
	cargo clean
