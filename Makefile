.PHONY: build test test-live fmt check run

build:
	cargo build

test:
	cargo test

test-live:
	cargo test live_ -- --nocapture

fmt:
	cargo fmt

check:
	cargo check

run:
	cargo run -- serve
