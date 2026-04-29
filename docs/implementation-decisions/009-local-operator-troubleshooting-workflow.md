# Local Operator Troubleshooting Workflow

Decision:

- document one recommended troubleshooting order
- use `run --json`, then `daemon status`, then `daemon logs`, then
  agent-scoped inspection, then `tui`, then foreground `serve`

Reason:

- Holon has multiple operator entry points now
- runtime health, agent state, and live interaction are distinct debugging
  layers and should be inspected in that order
