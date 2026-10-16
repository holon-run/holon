---
title: Local apps
summary: Agent-hosted interfaces, their request and event flow, and the limits of shared-session access.
order: 120
---

# Local apps

A Local App is a static HTML and JavaScript interface hosted by an agent.
It can present a focused view of work, collect input, or offer controls for
a particular workflow. The app's files live under that agent's `apps/`
directory; users open it through a logical app route.

For example, a documentation agent could host a review queue with a button to
request a fresh link check. The app presents the queue and sends the request.
The agent performs the authorized work and reports the result.

## How apps fit with other surfaces

The Web GUI is a general client for managing Holon. A Local App is an
agent-owned interface for a narrower purpose. Its static files are separate
from the GUI implementation.

A skill tells an agent how to carry out a workflow. A tool executes an
operation. An app can provide a human interface to that workflow, but it
does not replace either the skill or the tool. Hosting a "Run checks" button
does not create a new runtime capability.

## Requests and events

The browser SDK exposes the app's context, a request method, and an event
stream. Context identifies the app, its owning agent, and session state.
A request submits structured input to the agent. Events expose app-related
runtime activity.

In the review-queue example, a successful request response confirms that the
request was accepted. It does not mean the link check passed, or even that
the agent has finished handling it. The interface must distinguish accepted,
pending, and completed work and display actual results when available.

The current event stream reports requests being enqueued and processing
starting. It does not stream completed business results or all agent activity.
An app must obtain the outcome through the result surface appropriate to its
workflow rather than displaying "complete" when processing starts. Replay is
bounded, so after a disconnect, check work state rather than assuming a missing
event means nothing happened. See [Results and artifacts](./results-and-artifacts.md).

## Identity, sessions, and authority

App routes identify an agent and app without exposing the Agent Home path.
They are logical addresses, not independent access credentials.

Hosted apps share the runtime's origin and authenticated browser session.
The SDK uses that existing session; it grants no extra authority. Requests
preserve their App provenance. Neither a button label nor the app owner's
agent ID makes submitted text an operator instruction.

The host applies path checks and a Content Security Policy to served assets.
These measures do not provide separate browser origins for each app or a
general isolation boundary between mutually untrusted apps. Review app code
before hosting it in a session that can access the control plane.

Cross-agent actions remain subject to the runtime's authority checks. The SDK
does not supply a separate permission system or bypass those checks.

## When to use one

Use a Local App when a small persistent interface makes an agent's work easier
to inspect or request. For general agent administration, the Web GUI or CLI may
already provide what you need. For unattended integration, use the HTTP
control plane rather than requiring a browser app to stay open.

## Next steps

- [Create and host a Local App](../guides/host-local-apps.md)
- [Local App manifest and SDK reference](../reference/local-apps.md)
- [Use the Web GUI](../guides/use-web-gui.md)
- [HTTP automation](../guides/automate-over-http.md)
- [Execution and access boundaries](./security-and-execution-boundaries.md)
