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
`../mdorigin.config.mjs`, adds the public mdorigin 0.7 `renderPage` plugin from
`../.theme/docs-layout.mjs`. It wraps the rendered body and calls `next(page)`;
mdorigin still owns routes, metadata, search, the header, language switching,
the footer, and Markdown negotiation. No mdorigin fork is needed.

`build-docs-navigation.mjs` reads title, order, and draft frontmatter from the
Markdown files directly inside Getting Started, Concepts, Guides, and Reference.
It writes `dist/content/assets/docs-navigation.json`; that generated data is
bundled with the code config, not loaded from the browser. Add sections in this
generator if the content structure changes. Section indexes, Blog, templates,
and other pages keep the default layout.

The plugin reuses mdorigin's H2/H3 IDs. `../assets/docs-layout.js` enhances the
server-rendered navigation with responsive disclosure controls and scroll
highlighting. Links remain usable without JavaScript. Mobile uses inline
disclosures rather than a modal drawer so keyboard and no-JavaScript access
do not depend on focus trapping.

`build:cloudflare` runs `check-docs-layout.mjs` against the actual Worker bundle.
The check covers both locales, every documentation detail, TOC targets, current
navigation, section indexes, Blog, alias redirects, and Markdown negotiation.
Browser checks should also cover 1440px, 1024px, 390px, and 320px viewports,
keyboard disclosure controls, heading jumps, and scroll highlighting.

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
  --out ./dist/cloudflare
npm run check:docs-layout
```

This fallback deliberately omits search. CI must run the normal full build.
