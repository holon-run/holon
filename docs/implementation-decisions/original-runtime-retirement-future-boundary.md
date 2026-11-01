# Original Runtime Retirement Future Boundary

Decision:

- return a boxed, type-erased `Send` future from the shared
  `RuntimeHandle::join_retiring_resources_inner` boundary
- keep mutation draining, posture reconciliation, producer draining and exit
  proof collection inside that future in their original order

Reason:

- Rust 1.99.1's full layout query stack reaches the default depth limit through
  CLI startup recovery, TaskStop supervision, original-child retirement and
  both the resource-observer join and posture-reconciliation branches of
  original Runtime retirement
- erasing only the mutation-drain branch leaves the sibling reconciliation
  branch inline; the shared physical-retirement boundary covers both without
  changing unrelated input-commit futures or increasing the recursion limit

Preserved boundary:

- the future still borrows the same Runtime instance and remains `Send`
- no detached task, new concurrency or lifecycle authority is introduced
- supervisors retain original join handles across cancellation, and a later
  drainer can resume joining them
- the tradeoff is one allocation for each cold physical-retirement operation
