# Shell Exposure Model

Decision:

- expose shell tools only to `TrustedOperator` and `TrustedSystem`
- persist shell output through the same tool execution log as other tools

Reason:

- shell is necessary for real coding loops but is the sharpest local tool
- a shared execution log keeps context assembly and auditing coherent
