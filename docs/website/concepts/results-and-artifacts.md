---
title: Results, artifacts, and observable state
summary: How to distinguish accepted work, execution progress, completion reports, task output, and delivered files.
order: 130
---

# Results, artifacts, and observable state

A useful result tells you what was accomplished, where the deliverables are,
and what was verified. Holon separates that user-facing delivery from the
execution records that explain how the agent worked.

For example, a request to update documentation might produce an edited file,
a link-check result, and a short completion report. The command output alone
does not explain whether the requested documentation is complete.

## Which surface answers which question?

| Surface | Use it to understand |
|---|---|
| User-facing brief or final response | The outcome, caveats, and next action for the current interaction. |
| WorkItem completion report | What was delivered and verified for a tracked objective. |
| Progress message | What the agent is doing now; it may be transient or compressed by the client. |
| Task status and output | Whether a command or delegated invocation ended, and what it returned. |
| Transcript and tool evidence | The detailed execution history needed for diagnosis or audit. |
| Artifact | The actual file, patch, image, or other deliverable. |

The final report should stand on its own. A blocker or required operator action
must not appear only in an intermediate progress message. A file link helps
you inspect a deliverable, but it does not replace the outcome and verification
summary.

## Acceptance, execution, and completion

An accepted message means the runtime received a request. A terminal execution
means that execution stopped, whether successfully, unsuccessfully, or by
cancellation. A completed objective means the requested outcome was checked
and explicitly closed.

Consider a report-generation app. Its request may be accepted immediately;
a task may later finish writing a PDF; the agent must still check the PDF and
report any missing sections. The first successful receipt is not the final
result.

Similarly, a final response can say that work is waiting for approval. It ends
the interaction without completing the WorkItem. Use the objective's lifecycle
and report to understand acceptance, not the presence of any final message.

## Where artifacts live

Artifacts have concrete locations. A file created in a linked worktree belongs
to that execution root, even if a file with the same name exists in the
canonical checkout. A handoff should preserve the actual location.

A host filesystem path is not automatically accessible from a remote browser
or another machine. A logical file route or a runtime-supplied locator may
offer access through a client, subject to authentication and file access rules.
It does not turn the artifact into a public download.

When publishing documentation or sharing a result outside the runtime, use
portable relative references or an approved published location. Do not expose
private host paths or callback capabilities merely to make a link clickable.

## Reading incomplete results

Check the layer that can answer the question. If a command failed, inspect its
bounded output. If the command passed but the objective is open, inspect the
remaining acceptance criteria. If the agent is waiting, inspect the recorded
wait and the external state it depends on.

Output can be truncated, and some runtime evidence is subject to retention.
Use available artifact or source references when the exact content matters.
The fact that a report exists does not guarantee every temporary file or
historical log will remain available indefinitely.

## Next steps

- [Observe agent progress and results](../guides/observe-agent-work.md)
- [Troubleshoot failed or stalled work](../guides/troubleshooting.md)
- [Work item reference](../reference/work-items.md)
- [CLI reference](../reference/cli.md)
- [Work items and tasks](./work-items-and-tasks.md)
