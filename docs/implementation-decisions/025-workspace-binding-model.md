# Workspace Binding Model

Decision:

- model workspace attachment explicitly with host-owned workspace entries
- keep `workspace_anchor`, active workspace entry, `execution_root`, and `cwd`
  separate
- split `attach_workspace` from `enter_workspace` and `exit_workspace`

Reason:

- daemon cwd and shell `cd` are not reliable sources of project identity
- instruction and skill loading need a stable workspace anchor
