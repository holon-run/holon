# Google Play reviewer access preparation

Prepared October 7, 2026. This is a handoff checklist and draft Console text,
**not an active review service or proof of completed review**. Audience: 18+
only. The screenshot daemon was loopback-only and has been shut down; its
scripted provider does not prove live AI functionality or policy compliance.

## Access arrangement

Use a dedicated review host with synthetic agents, conversations, work items
and files, isolated from personal/production data and credentials. A token
with broad host access must only reach this isolated environment; do not
assume the client provides finer-grained review permissions.

- Provide a public HTTPS base URL with a valid certificate, reachable without
  a LAN, VPN, Tailscale account or regional allowlist. Test from an external
  network, not just the host itself.
- Use the existing manual address/access-token login. Reviewers must not need
  to self-host, obtain a model subscription, register an account, scan a QR
  code, or contact the developer for each sign-in.
- Keep a dedicated revocable token and the host available throughout review
  and re-review. Do not require an expiring one-time invitation, OTP or MFA
  interaction. Deliver the actual URL/token only in private Console access
  fields, never in Git, PR bodies, screenshots or public docs.
- Use a functioning provider and the normal runtime for live generation;
  do not substitute canned screenshot responses for advertised capabilities.
  Record the actual host operator, model/provider, logging access,
  retention/deletion rules and safety configuration before submission.
- Seed harmless English sample agents, one work item with a plan/checklist,
  and readable workspace files. Use only synthetic attachments.
- Do not expose production tools, filesystem roots or secrets. Test every
  advertised feature that the review instructions describe.

Provisioning this host and private credentials requires separate operational
authorization. Neither the release default address nor an existing private
host is assumed to be a usable reviewer service.

## Private handoff fields — incomplete

| Field | Required evidence |
| --- | --- |
| HTTPS host URL | Actual externally tested base URL; not assigned yet |
| Access token | Private Console field only; not issued yet |
| Host operator and contact | Actual responsible operator; not assigned yet |
| Model/provider and safety controls | Actual configured provider and verification; not recorded yet |
| Review availability | Owner keeps host/token usable during review and re-review |
| Data handling | Host/provider logging, access, retention and deletion procedures |
| Device verification | Fresh install of the submitted Play build on an external network |

## Draft Console instructions

Select the app-access answer indicating that functionality is restricted by
sign-in. Supply the host URL and token in the private access fields. If Console
only offers username/password fields, explain that login uses a host address
and access token, not a username/password account.

The text below is **not ready to submit**: replace `HOST_URL` with the tested
URL, supply the real token privately, and verify the UI steps in the submitted
build. Never submit placeholders. Google requires the details needed to access
restricted functionality; see [the official app-content instructions](https://support.google.com/googleplay/android-developer/answer/9859455).

> Holon is an Android client for a separately operated Holon host. All agent
> features require sign-in. A dedicated review host is provided; you do not
> need to deploy a server or purchase an AI subscription.
>
> On the connection screen, enter HOST_URL in "Holon address" and the supplied
> private credential in "Access token", then use the login button. Use this
> manual token flow, not the QR scanner or organization browser sign-in.
>
> Open a sample agent, read its conversation and send a harmless prompt to
> receive a new response. Open a work item to inspect its plan and checklist.
> Use the folder entry in the agent conversation to browse and preview sample
> workspace files. Send a synthetic attachment and save a sample artifact.
> Settings provides language, appearance and the privacy-policy entry.
>
> The sample data is synthetic. For access problems, contact hello@holon.run.

## Pre-submission checks

- [ ] Host/operator/provider and data-handling fields above are complete.
- [ ] URL and token work from a fresh installation outside the developer network.
- [ ] Agent interaction, work-item details, file preview/download and attachment
      upload work with the submitted build and the review credential.
- [ ] English instructions match the submitted UI; privacy URLs are publicly
      accessible without authentication.
- [ ] Real credentials and verified instructions are saved privately in Console.
- [ ] [AI reporting and safety release gates](PLAY_COMPLIANCE.md) are resolved,
      or an explicit policy determination changes the applicable requirements.

Do not conflate this access checklist with content-rating or AI-policy approval.
No deployment, Console edit, new package upload or release is performed here.
