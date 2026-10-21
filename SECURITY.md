# Security Policy

## Reporting a vulnerability

Please report suspected security vulnerabilities privately through
[GitHub Security Advisories](https://github.com/holon-run/holon/security/advisories).
Choose **Report a vulnerability** to submit a private report to the maintainers.
Do not disclose vulnerability details in public issues, discussions, or pull
requests.

If the reporting button is unavailable, open an issue asking maintainers to
enable private vulnerability reporting or provide a private contact channel.
Do not include vulnerability details or sensitive information in that issue.

Include the following in your private report, where available:

- A concise description and the potential security impact.
- The affected release or commit, component, and relevant configuration.
- The expected security boundary and the observed behavior.
- Relevant source locations and sanitized evidence sufficient for assessment.
- Whether the issue is already publicly known or affects a deployed instance.

Never include live credentials, private keys, session cookies, complete callback
capability URLs, or other users' private data. Use placeholders and redact logs
and screenshots. If a credential has been exposed, revoke or rotate it promptly;
reporting the issue does not invalidate the credential.

## Supported versions

Holon is under active development. Security fixes target the current `main`
branch and the latest release. Older releases do not have guaranteed security
backports; users should upgrade to a release containing the fix.

Reports affecting older versions are welcome. Please indicate whether the issue
also affects the latest release or `main`, if known.

## Scope and security boundaries

Reports may concern the runtime, HTTP control plane, authentication and
authorization, callbacks, clients, Local Apps, integrations, dependencies, or
build and release workflows maintained in this repository.

Holon's host-local execution backend is not a general-purpose sandbox.
Workspace and worktree boundaries do not imply operating-system isolation.
See [Security and execution boundaries](docs/website/concepts/security-and-execution-boundaries.md)
for the documented guarantees and limitations. Reports of bypasses of enforced
boundaries are welcome.

Only investigate systems and data you own or have explicit permission to test.
Avoid accessing others' data, disrupting services, or retaining sensitive data.

## Coordinated disclosure

Maintainers will assess reports and coordinate any fix and public advisory
through the private GitHub advisory thread. Please allow time for assessment
and remediation, and coordinate public disclosure with maintainers.

Response and remediation times depend on severity and maintainer availability;
this policy does not promise a fixed response time or a bounty.
