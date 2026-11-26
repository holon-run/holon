# Website build

Run commands from this directory with Node.js 22+ and Python 3.11+:

```sh
npm ci
npm run build
```

`prepare:content` copies source content into `dist/content`, renders the template
catalog, and generates bilingual documentation navigation. Build and preview
commands use that isolated root; do not run mdorigin directly on the source tree.

## Documentation layout

`../mdorigin.config.json` keeps shared site settings. The code entry point,
`../mdorigin.config.mjs`, adds the public mdorigin 0.9 `renderPage` plugin from
`../.theme/docs-layout.mjs`. It wraps the rendered body and calls `next(page)`;
mdorigin still owns routes, metadata, search, the header, language switching,
the footer, and Markdown negotiation. No mdorigin fork is needed.

`build-docs-navigation.mjs` reads title, order, and draft frontmatter from the
Markdown files directly inside Getting Started, Concepts, Guides, and Reference.
It writes `dist/content/assets/docs-navigation.json`; that generated data is
bundled with the code config, not loaded from the browser. Add sections in this
generator if the content structure changes. The four section indexes and their
detail pages share the documentation shell. Pages without headings reserve the
right column without rendering an empty TOC, keeping the body in the same
position when navigating. The homepage, Blog, templates, and other pages keep
the default layout.

The plugin reuses mdorigin's H2/H3 IDs. `../assets/docs-layout.js` enhances the
server-rendered navigation with responsive disclosure controls and scroll
highlighting. Links remain usable without JavaScript. Mobile uses inline
disclosures rather than a modal drawer so keyboard and no-JavaScript access
do not depend on focus trapping.

`build:cloudflare` runs `check-docs-layout.mjs` against the actual Worker bundle.
The check covers both locales, every documentation index and detail, TOC targets, current
navigation, section indexes, Blog, alias redirects, and Markdown negotiation.
Browser checks should also cover 1440px, 1024px, 390px, and 320px viewports,
keyboard disclosure controls, heading jumps, and scroll highlighting.

## Image weight

`../assets/` is published byte-for-byte, so an oversized export is an oversized
page. `python3 optimize-assets.py` re-encodes covers (WebP, quality 82, width
capped), screenshots (lossless WebP, width capped), and the logo in place, and
skips files that would not shrink by 10%. It also derives `assets/logo.webp`
from `assets/logo.png`: the page header loads the WebP on every page view, while
the PNG stays for structured data and social crawlers that expect a raster icon.
Run it after adding or replacing an image, and use
`python3 optimize-assets.py --check` to see remaining work without writing.

Keep screenshots lossless: they contain UI text, and lossy WebP blurs it.

## Static asset caching

`build:cloudflare` uses `--binary-mode external`, which stages images, PDFs, and
other binaries into `dist/cloudflare/assets` and emits an assets `_headers` rule
(`public, max-age=86400, stale-while-revalidate=604800`). The Worker then carries
text content only. Inline mode instead base64-embeds every binary in the Worker,
serves it with `content-type` alone — no `cache-control`, no `etag`, no edge
cache — so every visit re-downloads every image. Keep the flag; the assets
directory must also stay in `../wrangler.jsonc`.

## Asset host

Pages are served by the `holon.run` Worker; media is also served by a second
Worker on `https://assets.holon.run` (`../wrangler.assets.jsonc`,
`../asset-worker.mjs`). A separate origin gives media its own TCP/TLS
connection, so a slow or lossy image download cannot stall the page connection,
and HTTP/1.1 clients get a second connection pool instead of the six-per-host
limit.

`rewrite-asset-host.mjs` runs inside `build:cloudflare` and rewrites media
references (images, PDFs, video, fonts) in `dist/content` to the asset host
before mdorigin builds the bundle; its `--check` form then fails the build if
the emitted bundle still has same-origin media references. Scripts,
stylesheets and JSON fetched by page code stay on the page origin: they are on
the critical path, CSP expects them same-origin, and they reuse the page
connection anyway. Set `ASSET_HOST=none` to skip the rewrite: that is the
rollback switch, and pages return to same-origin media after the page Worker is
rebuilt and redeployed.

`stage-referenced-assets.mjs` runs right after the bundle is built. mdorigin
counts `image/svg+xml` as text, so diagrams stay inside the page Worker bundle
and are never staged into the asset directory; the script copies every media
file that pages now reference on `assets.holon.run` into
`dist/cloudflare/assets` when mdorigin has not staged it, and its `--check` form
fails the build when a rewritten reference has no staged file. Adding a new
text-based media type therefore needs no extra step, but removing this script
brings back `404` responses for diagrams on the asset host.

The zone needs one proxied placeholder record, `assets.holon.run AAAA 100::`:
a Worker route only matches a hostname that already resolves through
Cloudflare. The `assets.holon.run/*` route itself is configured once in the
Cloudflare dashboard, like the page Worker's route, because the CI deploy token
has no zone route access. CI deploys the asset Worker first and the page Worker
second, so page HTML never points at a host that is not serving yet. The page
Worker keeps the same binaries as a fallback for anything still using a
relative path.

## Local native-search limitation

If indexbind's native addon cannot load on an older Linux host, do not report
the full build as passing. The layout can still be verified independently:

```sh
npm run prepare:content
npm run build:index
npm run check:content
npm run check:blog
npm run check:directories
./node_modules/.bin/mdorigin build cloudflare \
  --root ./dist/content --config ./dist/content/mdorigin.config.mjs \
  --binary-mode external \
  --out ./dist/cloudflare
npm run check:docs-layout
```

This fallback deliberately omits search. CI must run the normal full build.
