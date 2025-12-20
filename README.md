# Holon

Holon is a standardized, atomic execution unit for AI-driven software engineering. It bridges the gap between AI agent probability and engineering determinism by providing a "Brain-in-a-Sandbox" environment.

## Status: v0.1 (Atomic Execution Unit)
v0.1 implements the core "Adapter-in-Container" architecture using Golang and Docker.

### Key Features
- **Dynamic Injection**: Standard Docker images as execution environments, with the Holon Adapter injected at runtime.
- **Spec-Driven**: Declarative task definitions (`spec.yaml`).
- **AI Agent (ReAct)**: Built-in agent loop using Anthropic's Claude 3.5 Sonnet.
- **Sandboxed Tools**: File I/O and shell execution within the container.

## Getting Started

### Prerequisites
- Docker installed and running
- Go 1.22+
- Anthropic API Key (`ANTHROPIC_API_KEY`)

### Build
```bash
make build
```

### Run Tests
```bash
make test
```

Integration tests live in `tests/integration` and are implemented with `testscript`. They require a working Docker daemon; the Docker-dependent cases will be skipped if Docker is unavailable.

### Run an Example
```bash
export ANTHROPIC_API_KEY=your_key_here
make run-example
```

## Architecture
Holon follows the **"Brain-in-Body"** principle where the AI logic (Brain) runs inside the same container (Body) as the code it is working on, ensuring atomicity and perfect context.

For more details, see [RFC-0001 (Atomic Execution Unit)](rfc/0001-holon-atomic-execution-unit.md).
