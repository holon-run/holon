.PHONY: help web build all test test-live fmt fmt-check lint check ci run clean

WEB_DIR := web-gui/app

help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

web: ## Build the web GUI (requires Node.js). Produces web-gui/app/dist
	@if [ -s "$$HOME/.nvm/nvm.sh" ]; then . "$$HOME/.nvm/nvm.sh" && nvm use; fi; \
	cd $(WEB_DIR) && npm ci && npm run build

build: ## Build all Rust targets (cargo build --all-targets)
	cargo build --all-targets

all: web build ## Build everything: web GUI then Rust

test:
	cargo test --all-targets -- --test-threads=1

test-live:
	cargo test live_ -- --nocapture

fmt:
	cargo fmt

fmt-check: ## Check formatting without modifying files
	cargo fmt --all -- --check

lint: ## Run clippy
	cargo clippy --all-targets

check: ## Quick local check (formatting + clippy + compile check)
	RUSTFLAGS="-D warnings" cargo check --all-targets

ci: fmt-check lint build test ## Run the full CI checks locally

run:
	cargo run -- serve

clean: ## Remove Rust build artifacts
	cargo clean
