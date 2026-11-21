# iOS store and article assets

These assets are prepared for the `en-US` App Store listing of Holon iOS
and can also be reused on the website or in articles. They are native,
unretouched simulator captures taken on November 21, 2026 from current
`main` (commit `a6cd86c6`), which includes the UI improvements released
in the TestFlight 0.1.2 (16) build (input preview, unread states,
navigation, avatar caching, and layout stability). They replace the
0.1.0 captures from October 2026 and are the candidate files for the
next App Store screenshot upload.

## Files and provenance

- `en-US/screenshots/iphone-light/`: four 1206 x 2622 PNG captures for the
  iPhone (Dynamic Island / medium display) screenshot slot.
- `en-US/screenshots/iphone-dark/`: the same four screens in dark mode,
  kept as alternates.
- `en-US/screenshots/ipad-13-light/`: four 2064 x 2752 PNG captures for
  the iPad Pro 13-inch slot.
- `en-US/screenshots/ipad-13-dark/`: the same four iPad screens in dark
  mode, kept as alternates.
- `en-US/SHA256SUMS`: checksums for every PNG.

Each set shows the same four screens in order: the agents home
(`01-agents`), a task conversation with a structured Markdown result
(`02-conversation`), a work item with its checklist (`03-work-item`), and
a rendered plan document (`04-plan`).

The captures come from a dedicated iPhone 17 Pro simulator (1206 x 2622)
and a dedicated iPad Pro 13-inch (M5) simulator (2064 x 2752) running the
`main` client (`a6cd86c6`, simulator Debug build) on iOS 27.0. The client
was connected to an isolated, local-only demo Holon service with a
disposable home, serving a fictional "Fieldnotes" project with four
example agents. The visible content states it is fictional and
illustrative, and every PNG was individually reviewed to confirm there are
no error, loading, or cache banners and no credentials, tokens, or
personal data. On iOS 27 the iPad status bar shows the frontmost app
name, so the iPad frames read "Holon" next to the date while the iPhone
frames do not.
