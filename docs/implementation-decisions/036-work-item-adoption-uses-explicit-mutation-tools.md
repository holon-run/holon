# Work-Item Adoption Uses Explicit Mutation Tools

Decision:

- add `update_work_item` and `update_work_plan` as the minimal trusted tool
  surface for explicit work-item adoption
- keep their semantics snapshot-based rather than incremental

Reason:

- higher-level work state should not be created implicitly from arbitrary text
- full-snapshot writes keep persistence and prompt projection simple
