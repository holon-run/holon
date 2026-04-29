# Background Task V1

Decision:

- implement the first real task primitive as an in-process sleep job
- route task lifecycle back through normal `task_status` and `task_result`
  queue events

Reason:

- this proves the runtime contract without committing early to distributed
  workers
- the key property is that background work rejoins the same agent loop
