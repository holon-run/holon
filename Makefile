.PHONY: help web web-ci transport-types transport-types-check snapshots-check snapshots-refresh build all test test-resource-lint test-concurrent test-concurrent-repeat test-live test-live-openai test-live-anthropic test-live-codex test-live-xai test-live-images test-live-runtime docker-build docker-smoke docker-live-acceptance fmt fmt-check lint check ci run clean

WEB_DIR := web-gui/app
OPENAPI_TOOLS_DIR := web-gui/openapi-tools
CONCURRENT_REPEATS ?= 3
DOCKER_IMAGE ?= holon:dev
CONCURRENT_LIFECYCLE_TESTS := \
	runtime_tasks \
	runtime_waiting_and_reactivation \
	runtime_waiting_and_delivery_regressions \
	http_events \
	http_tasks
CONCURRENT_TESTS := $(CONCURRENT_LIFECYCLE_TESTS) wt204_parallel_worktree_workflow
ANTHROPIC_COMPATIBLE_LIVE_TESTS := \
	live_deepseek_anthropic_accepts_context_management \
	live_xiaomi_token_plan_accepts_context_management \
	live_xiaomi_anthropic_accepts_context_management \
	live_xiaomi_token_plan_anthropic_accepts_context_management \
	live_zai_anthropic_accepts_context_management \
	live_zai_anthropic_builtin_web_search_reports_prime_backend \
	live_bigmodel_anthropic_accepts_context_management \
	live_bigmodel_anthropic_builtin_web_search_reports_backend \
	live_minimax_anthropic_accepts_context_management

help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2}'

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

snapshots-check: ## Check CLI, OpenAPI, HTTP route, runtime status, and model tool schema snapshots
	cargo test --test cli_snapshot
	cargo test --test openapi_snapshot
	cargo test --test http_route_snapshot
	cargo test --test runtime_status_inventory_snapshot
	cargo test --test tool_schema_inventory_snapshot

snapshots-refresh: ## Refresh CLI, OpenAPI, HTTP route, runtime status, and model tool schema snapshots
	cargo test --test cli_snapshot refresh_cli_snapshot -- --ignored
	cargo test --test openapi_snapshot refresh_openapi_snapshot -- --ignored
	cargo test --test http_route_snapshot refresh_http_route_inventory_snapshot -- --ignored
	cargo test --test runtime_status_inventory_snapshot refresh_runtime_status_enum_inventory_snapshot -- --ignored
	cargo test --test tool_schema_inventory_snapshot refresh_tool_schema_inventory_snapshot -- --ignored

build: ## Build all Rust targets (cargo build --all-targets)
	cargo build --all-targets

all: web build ## Build everything: web GUI then Rust

test: ## Run the full Rust test suite serially
	cargo test --all-targets -- --test-threads=1

test-resource-lint: ## Audit permanent test temp directories against the reasoned allowlist
	python3 scripts/check-test-temp-resources.py

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

test-live: ## Run the baseline and configured-provider live smoke tests
	@printf '%s\n' \
		'Requires configured provider credentials and network access.' \
		'Runs two configured-chain baseline probes plus one smoke request per configured provider.' \
		'Test binaries: live_llm_baseline (selected tests), live_provider_smoke'
	cargo test --test live_llm_baseline live_llm_baseline_configured_chain_smoke -- --ignored --nocapture
	cargo test --test live_llm_baseline live_llm_baseline_tool_roundtrip -- --ignored --nocapture
	cargo test --test live_provider_smoke -- --ignored --nocapture

test-live-openai: ## Run OpenAI Responses and Chat Completions live tests
	@printf '%s\n' \
		'Requires configured OpenAI credentials and network access.' \
		'Test binaries: live_openai, live_openai_chat_completions'
	cargo test --test live_openai -- --ignored --nocapture
	cargo test --test live_openai_chat_completions -- --ignored --nocapture

test-live-anthropic: ## Run Anthropic and Anthropic-compatible live tests
	@printf '%s\n' \
		'Requires Anthropic/Claude auth plus any provider-specific credentials exercised by the compatible suite.' \
		'Compatible probes include DeepSeek, Xiaomi, Z.ai, BigModel, and MiniMax; the cache matrix may be high cost.' \
		'Test binaries: live_llm_baseline (Anthropic cache test), live_anthropic, live_anthropic_cache, live_anthropic_compatible'
	cargo test --test live_llm_baseline live_llm_baseline_anthropic_prompt_cache_hit -- --ignored --nocapture
	cargo test --test live_anthropic -- --ignored --nocapture
	cargo test --test live_anthropic_cache -- --ignored --nocapture
	@set -eu; \
	for live_test in $(ANTHROPIC_COMPATIBLE_LIVE_TESTS); do \
		cargo test --test live_anthropic_compatible "$$live_test" -- --exact --ignored --nocapture; \
	done

test-live-codex: ## Run OpenAI Codex live tests
	@printf '%s\n' \
		'Requires Codex CLI ChatGPT auth state, network access, and image support for the image probe.' \
		'Test binaries: live_codex, live_openai_codex_compact'
	cargo test --test live_codex -- --ignored --nocapture
	cargo test --test live_openai_codex_compact -- --ignored --nocapture

test-live-xai: ## Run xAI live tests
	@printf '%s\n' \
		'Requires configured xAI credentials and network access.' \
		'Test binary: live_xai'
	cargo test --test live_xai -- --ignored --nocapture

test-live-images: ## Run provider-backed image and vision live tests
	@printf '%s\n' \
		'Requires an OpenAI-compatible vision credential and a Volcengine Ark image credential, plus network access.' \
		'Test binaries: live_view_image, live_volcengine_image'
	cargo test --test live_view_image -- --ignored --nocapture
	cargo test --test live_volcengine_image -- --ignored --nocapture

test-live-runtime: ## Run end-to-end runtime and workspace-tool live tests
	@printf '%s\n' \
		'Requires the selected continuity/workspace model credentials, network access, and git.' \
		'Defaults: deepseek-anthropic/deepseek-v4-pro for continuity; configured default model for workspace tools.' \
		'Test binaries: live_prompt_continuity, live_workspace_tools'
	cargo test --test live_prompt_continuity -- --ignored --nocapture
	cargo test --test live_workspace_tools -- --ignored --nocapture

docker-build: ## Build the local Holon runtime image
	docker build --tag "$(DOCKER_IMAGE)" .

docker-smoke: docker-build ## Start the image and verify the real service readiness boundary
	scripts/docker-smoke.sh "$(DOCKER_IMAGE)"

docker-live-acceptance: docker-build ## Run manual Docker acceptance with a real LLM (requires HOLON_LIVE_MODEL and credentials)
	python3 scripts/docker-live-acceptance.py --image "$(DOCKER_IMAGE)" --skip-build

fmt:
	cargo fmt

fmt-check: ## Check formatting without modifying files
	cargo fmt --all -- --check

lint: ## Run clippy
	cargo clippy --all-targets

check: ## Quick local check (formatting + clippy + compile check)
	RUSTFLAGS="-D warnings" cargo check --all-targets

ci: web-ci fmt-check lint build snapshots-check test-resource-lint test ## Run the full CI checks locally

run:
	cargo run -- serve

clean: ## Remove Rust build artifacts
	cargo clean
