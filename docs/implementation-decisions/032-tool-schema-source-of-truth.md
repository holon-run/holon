# Tool Schema Source Of Truth

Decision:

- derive built-in tool input schemas from typed Rust argument structs using
  `schemars`
- keep `ToolSpec.input_schema` as the runtime-neutral representation for now
- default provider emission to `strict: false`

Reason:

- hand-written JSON schemas had drifted from the actual argument contract
- Holon needs one stable source of truth without a custom schema engine
