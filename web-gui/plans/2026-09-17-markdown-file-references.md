# #3033 Markdown file references

Implement the accepted output contract with one shared Markdown consumer for
briefs, assistant activity and Explorer documents. Absolute Unix paths resolve
on the execution host; relative references require the actual document locator.
Historical workspace URIs remain compatible and never lose explicit roots.

- Classify links/images and whole inline-code paths before React Markdown URL
  sanitization; preserve HTTP/HTTPS/mailto and local fragments. Decode URL path
  components once, keep inline paths literal, and leave workspace URI decoding
  to the resolver. Reject unsupported local queries and protocols visibly.
- Batch at most 64 unique references. An identity-scoped, 512-entry/30-second
  memory cache deduplicates requests, not authorization. Drop stale responses.
- Plain clicks use Explorer; modified clicks use an authenticated `/files`
  preview route carrying workspace, root and relative path. No tokens in URLs.
- Shared heading slugs and per-document fragment scrolling; images use
  authenticated blob reads and release object URLs on cleanup/identity change.
- Keep existing Explorer navigation state, use complete locators at handoff,
  and update new-tab/copy-Web-link actions. Do not switch prompt defaults.

Validate encoding, roots, cache lifetime and rendering with unit/browser tests,
then use an isolated real daemon for canonical/worktree and removed-root cases.
Do not restart the user's daemon. Publish evidence in the PR; close #2923/#2933
only if their individual acceptance criteria are demonstrated.

## Implementation and acceptance (2026-09-17)

The three Markdown surfaces now share classification, AST transformation,
batched resolution, authenticated images and heading slugs. `/files` reuses
Explorer with an explicit workspace/root/path locator; file and directory
navigation updates its URL for refresh. The existing login flow needed a
same-document reload fix for return URLs with fragments. Explorer snapshots and
pending results are scoped to the current connection/auth identity.

Validation:

- Frontend unit suite: 58 files, 612 tests passed. Focused final reference,
  Explorer and panel-preference checks: 21 tests passed.
- TypeScript checks and production Vite build passed. Existing large-chunk
  advisory remains.
- `markdown-files.spec.ts`, `panel-layout.spec.ts`, `same-origin-access.spec.ts`:
  18 browser tests passed, including all three Markdown surfaces, Cmd/Ctrl and
  middle-click, fragments, streaming additions/late responses, active-agent
  changes, refresh, and panel/layout/auth regressions. An existing work-item test
  now waits for app restoration before deciding whether to toggle its panel.
- `real-daemon/markdown-files.spec.ts`: passed with the production bundle and an
  isolated authenticated daemon. A real Git canonical checkout/worktree pair is
  registered in that fixture's registry. Verified different same-path contents,
  worktree-only filenames containing spaces/Chinese/parentheses/percent/`#?`,
  relative links, legacy URIs with unescaped `:`/`/` root IDs, agent-home links,
  directories, authenticated images, Bearer and cookie downloads, login and
  refresh with a deep fragment, and removed-root tombstones without fallback.
- Existing Rust `http_file_references` (3) and `http_workspace` (16) tests passed,
  including path traversal, symlink containment and execution-root selection.

The evidence covers #2923 and #2933's file-reference acceptance cases. This
change does not alter backend APIs or enable #3031's prompt default switch.
Only temporary test daemons were started/stopped; the operator's service was
not restarted.
