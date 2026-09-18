# Lite paper GUI capture

`scenario.mjs` defines the Chinese scene and `scenario-en.mjs` its English counterpart: a new commit arrived through a
webhook, review finished for this turn, and the same PR work item waits for CI.
It includes three shared roles, the external message, brief, work item and
checklist. It is synthetic presentation data, not evidence of a real review or
of the runtime executing a webhook. No repository, daemon or credentials are
accessed by the scenario.

`capture.mjs` serves the real frontend from an explicitly selected GUI checkout.
It reuses that checkout's E2E fixture transport, replacing only its scenario
module in a temporary copy and binding its dev transport to loopback. No GUI
component, CSS or rendered DOM is changed for the picture. It opens the external
message and work-item detail through the real UI.

## Reproduce

Use a checkout with the GUI dependencies and Playwright Chromium installed
(`npm ci` and `npx playwright install chromium` in `web-gui/app`). Run from the
lite-paper repository root, using an absolute path to the chosen GUI checkout:

```bash
node docs/lite-paper/tools/gui-tour/capture.mjs \
  --gui-root /path/to/current-holon/web-gui/app \
  --output-dir build/lite-paper/gui-tour/zh-CN
```

Optional `--port` defaults to 43137. Do not run concurrent captures on the same
port. Both the fixture server and the browser are stopped on exit; temporary
fixture files are removed. Browser requests outside the fixture origin fail.

The script writes:

- `web-gui-review-zh-CN.png`: unedited 3000 × 1880 browser screenshot (1500 × 940,
  2x pixel density), light theme, Chinese locale, Asia/Shanghai timezone.
- `capture-info.json`: GUI commit and dirty-state indicator, viewport, fixed
  timestamp, scenario/script/transport/image hashes and successful checks.
- `page-text.txt`: extracted visible text for review, excluded from publication.

Inspect the screenshot after generation. Checks verify Chinese content, the
expanded webhook source, checklist visibility, waiting state and lack of page
errors; they do not verify real CI execution, runtime persistence or product
acceptance. Copy a reviewed image to `docs/lite-paper/assets/` and its metadata
to `web-gui-review-zh-CN.capture.json` beside it. Keep the synthetic-data caption
in the language Markdown.

## Current source

The first capture used GUI commit `a8eaf9815bf430f1555ff517067e902ec90364b7`
from the canonical repository on 2026-09-17. This is newer than the paper
branch's GUI. The explicit `--gui-root` avoids accidentally capturing an older
frontend. Use this commit to reproduce this version, or select a newer checkout
deliberately and review the resulting image. The capture metadata is authoritative.

For English, pass `--lang en` and use an English output directory:

```bash
node docs/lite-paper/tools/gui-tour/capture.mjs --lang en \
  --gui-root /path/to/current-holon/web-gui/app \
  --output-dir build/lite-paper/gui-tour/en
```

The default is `--lang zh-CN`. The output image is `web-gui-review-<lang>.png`.
Locale selection changes both the actual GUI language and synthetic scenario;
it does not edit the DOM or image. The English capture uses GUI commit
`7b96b94a89b8ef1e67a96c8d349c60eee2eb4b99`; see its capture metadata for details.
