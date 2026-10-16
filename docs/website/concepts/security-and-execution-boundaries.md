---
title: Security and execution boundaries
summary: Host-local execution, workspace limits, authenticated access, and capability-secret risks.
order: 150
---

# Security and execution boundaries

An agent that can run shell commands can affect the machine and services those
commands can reach. Holon's workspace and trust rules organize that work, but
the host-local backend does not provide a general sandbox.

## The host-local runtime

Commands and file operations use the host environment and its permissions.
A command may be able to read files outside the project, reach network services,
or access credentials available to the process. Do not assume that starting an
agent creates a container, virtual machine, or separate operating-system user.

For example, a reviewer asked to inspect an unfamiliar repository may encounter
a test script that reads files from the user's home. The review scope forbids
unrelated access, but that instruction is not an OS-level block on the script.
Review the script and use an appropriately isolated environment before running
untrusted code.

## Execution environment summary

The agent receives a summary of the current execution environment and policy.
Read it as a description of specific enforced or unenforced boundaries, not a
blanket safety guarantee.

A runtime can enforce message ingress or control-plane ownership while leaving
filesystem and network access to the host. Enforcement of one boundary does
not imply enforcement of the others. Exact guarantee levels and workspace
operations are documented in the
[workspace reference](../reference/workspaces.md) and
[execution specification](../spec/workspace-and-execution.md).

## Workspace binding

The active workspace sets project context and default locations. Relative
patches resolve against its execution root; commands start in the selected
working directory. Explicit paths and shell commands can still reach elsewhere
when the backend and host permissions allow it.

A worktree separates working copies, not host credentials or network access.
Use it to avoid accidental file conflicts. Use host permissions, dedicated
accounts, containers, or virtual machines when you need stronger confinement.

<a id="remote-access-holon-serve"></a>

## Local and remote access

Local daemon access and network serving have different exposure. A local Unix
socket relies on host access controls. Serving over a network makes the control
plane reachable by other clients and requires deliberate authentication and
transport protection.

Authentication establishes a client's identity or accepted credential.
Authorization determines what that caller can do. A valid login does not
make every target agent or operation available, and it does not turn arbitrary
payload content into a trusted instruction.

Keep access as narrow as the workflow permits. A tunnel changes reachability;
it does not replace authentication. Bearer tokens and browser sessions need
protection even when the connection is encrypted. See the
[remote connection guide](../guides/connect-remote-runtime.md) and
[OIDC guide](../guides/configure-oidc-authentication.md) for setup and verification.

## Capability-secret URLs

External callbacks use URLs containing secret tokens. Possession of a URL
allows its corresponding wake or enqueue action. That is narrower than full
control-plane access, but exposure can still cause unwanted activations or
inject external messages.

Treat the complete URL as a credential. Keep it out of source control, public
reports, screenshots, and unnecessary logs. Use a protected transport and
rotate or revoke the capability if it leaks. Do not assume that control-plane
login protection replaces the callback's own capability check.

## Trust and provenance

[Trust boundaries](./trust-boundaries.md) govern how the agent interprets input.
They do not restrict what a shell process can access. Conversely, an isolated
process can still receive misleading instructions. Source classification and
execution isolation address different risks.

## Practical recommendations

Before running unfamiliar code, inspect both the requested actions and the
host access they would have. Limit credentials and filesystem access to what
the work needs. For remote operation, verify authentication, transport, and
caller authorization separately.

After work ends, clean up only the resources the agent owns and no longer
needs. Preserve deliverables before removing a worktree, child agent, or
external integration. Finishing one objective does not imply that every shared
resource should be deleted.

## See also

- [Workspaces](./workspaces.md)
- [Connect to a remote runtime](../guides/connect-remote-runtime.md)
- [Configure OIDC authentication](../guides/configure-oidc-authentication.md)
- [HTTP control plane reference](../reference/http-control-plane.md)
- [Configuration reference](../reference/configuration.md)
