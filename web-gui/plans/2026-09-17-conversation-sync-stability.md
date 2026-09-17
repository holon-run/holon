# Conversation sync stability

## Problem

A temporary pause/reconnect changed an active turn's presentation to `syncing`.
Automatic expansion depended on that presentation being `running`, so an active
process folded and unfolded on connection changes. Inline connection banners also
changed transcript height, fighting bottom-follow and reading-anchor restoration.
Protocol resets cleared the transcript before a replacement summary arrived.

## Decision

- Preserve execution expansion from the actual turn lifecycle, independently of
  transport freshness. Retain the `Syncing` label for unconfirmed active turns;
  do not fetch missing execution details until the connection is ready.
- Render connection notices and retry in a permanent 32px row above the transcript.
  Neither status text nor error actions participate in transcript scroll geometry.
- Keep a separate presentation view in the identity-scoped GUI store during
  retention/replay/slow-consumer/stream-recovery resets. Keep the SDK view and
  checkpoint authoritative. Disable history pagination and detail loading against
  the retained frame. Replace it when bootstrap supplies the new snapshot.
- Never retain across explicit identity/version/epoch resets, unknown cursor
  rejection, agent removal, or access-error status. A new identity scope has its
  own store. Read markers require a valid protocol scope with no pending reset.

## Boundaries and verification

No API, Rust runtime, persistent cache format, or SSE recovery contract changes.
Normal resume/reconnect still uses the retained SDK cursor; recovery still obtains
an authoritative summary. Tests cover active/manual expansion, fixed transcript
geometry, summary request count, delayed reset bootstrap, and identity/error
boundaries. Existing presentation and reset-scope browser suites remain required.
