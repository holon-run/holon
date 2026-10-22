#!/usr/bin/env node
// Exercise the actual generated Worker, not only the intermediate Markdown.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";
import worker from "./dist/cloudflare/worker.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const catalog = JSON.parse(
  await readFile(path.join(here, "dist/content/assets/agent-templates.json"), "utf8"),
);
const documents = JSON.parse(
  await readFile(path.join(here, "dist/search/documents.json"), "utf8"),
);
const chunks = JSON.parse(
  await readFile(path.join(here, "dist/search/chunks.json"), "utf8"),
);
const env = {
  ASSETS: {
    async fetch(request) {
      const url = new URL(typeof request === "string" ? request : request.url);
      const root = path.join(here, "dist/cloudflare/assets");
      const target = path.resolve(root, "." + url.pathname);
      assert.ok(target.startsWith(root + path.sep), "asset path must stay in bundle");
      try {
        return new Response(await readFile(target));
      } catch (error) {
        if (error.code === "ENOENT") return new Response("Not found", { status: 404 });
        throw error;
      }
    },
  },
};

async function fetchText(route, accept = "text/html") {
  const response = await worker.fetch(
    new Request(`https://holon.invalid${route}`, {
      headers: { accept, "accept-language": route.startsWith("/zh-CN/") ? "zh-CN" : "en" },
    }),
    env,
    { waitUntil: (promise) => promise.catch((error) => { throw error; }) },
  );
  assert.equal(response.status, 200, `${route}: expected successful rendered response`);
  return response.text();
}

// The renderer normalizes equivalent HTML entities; compare visible text.
const visibleText = (value) => value.replace(/<[^>]*>/g, "").replace(
  /&(#x[0-9a-f]+|#\d+|amp|lt|gt|quot|apos);/gi,
  (_, entity) => entity[0] === "#"
    ? String.fromCodePoint(Number.parseInt(entity.slice(entity[1].toLowerCase() === "x" ? 2 : 1),
      entity[1].toLowerCase() === "x" ? 16 : 10))
    : ({ amp: "&", lt: "<", gt: ">", quot: '"', apos: "'" })[entity.toLowerCase()],
);
const checked = new Map();
for (const prefix of ["", "/zh-CN"]) {
  const locale = prefix ? "zh-CN" : "en";
  const document = documents.find((entry) => entry.docId === `${prefix}/templates/`);
  assert.ok(document?.chunkCount > 0, `${locale}: template catalog must be indexed`);
  const indexedText = visibleText(chunks.filter((entry) => entry.docId === document.docId)
    .map((entry) => entry.chunkText).join("\n"));
  const home = await fetchText(`${prefix}/`);
  const directory = await fetchText(`${prefix}/templates/`);
  const homeText = visibleText(home);
  const directoryText = visibleText(directory);
  checked.set(`${prefix}/templates/`, directory);
  assert.equal((home.match(/class="template-card"/g) ?? []).length, 6);
  assert.equal(
    (directory.match(/class="template-entry"/g) ?? []).length,
    catalog.templates.length,
  );
  assert.ok(directory.includes(catalog.source.sha), "directory must show source SHA");
  assert.ok(directory.includes('src="/assets/template-catalog.js"'),
    "directory must load progressive enhancement from a CSP-compatible asset");
  assert.ok(!directory.includes('class="template-details"'),
    "directory cards must not expand inline");
  assert.ok(directory.includes('id="template-query"'), "directory must include search control");
  let details = "";
  for (const entry of catalog.templates) {
    assert.ok(directory.includes(`id="${entry.id}"`), `${locale}: missing ${entry.id}`);
    assert.ok(directoryText.includes(entry.localized[locale].name), `${locale}: missing name for ${entry.id}`);
    assert.ok(directoryText.includes(entry.localized[locale].summary), `${locale}: missing summary for ${entry.id}`);
    assert.ok(indexedText.includes(entry.localized[locale].name), `${locale}: unindexed ${entry.id}`);
    const route = `${prefix}/templates/${entry.id}/`;
    assert.ok(directory.includes(`href="${route}"`), "card must link to its detail page");
    const detail = await fetchText(route);
    checked.set(route, detail);
    details += detail.match(/<div class="template-detail">[\s\S]*?<\/main>/)?.[0] ?? detail;
    assert.ok(detail.includes(entry.createSelector), "creation selector must match snapshot");
    assert.ok(detail.includes(entry.contractUrl), "role contract link must match snapshot");
    assert.ok(detail.includes(`href="${prefix}/templates/"`), "detail must link back to catalog");
    assert.ok(detail.includes('data-template-copy'), "detail must offer command copy");
    assert.ok(detail.includes('src="/assets/template-catalog.js"'), "detail must load command enhancement");
    const detailDocument = documents.find((item) => item.docId === route);
    assert.ok(detailDocument?.chunkCount > 0, `${route}: detail page must be indexed`);
    const detailText = visibleText(chunks.filter((item) => item.docId === route)
      .map((item) => item.chunkText).join("\n"));
    assert.ok(detailText.includes(entry.localized[locale].name), `${route}: indexed name`);
    assert.ok(detailText.includes(entry.localized[locale].summary), `${route}: indexed summary`);
    const otherPrefix = prefix ? "" : "/zh-CN";
    assert.ok(detail.includes(`href="${otherPrefix}/templates/${entry.id}/?lang=${otherPrefix ? "zh-CN" : "en"}"`),
      "language switch must target the same template");
    if (entry.featuredRank) {
      assert.ok(home.includes(route));
      assert.ok(homeText.includes(entry.localized[locale].name));
    }
  }
  const section = home.match(/<section class="home-templates"[\s\S]*?<\/section>/)?.[0];
  assert.ok(section, "homepage must contain the rendered section without client-side fetch");
  for (const match of (section + directory + details).matchAll(/href="(\/[^"]*)"/g)) {
    const url = new URL(match[1], "https://holon.invalid");
    // Validate generated section links and the directory's navigation, including anchors.
    if (!checked.has(url.pathname)) {
      checked.set(url.pathname, await fetchText(url.pathname));
    }
    if (url.hash) {
      const id = decodeURIComponent(url.hash.slice(1));
      assert.ok(checked.get(url.pathname).includes(`id="${id}"`),
        `${url.pathname}: missing anchor ${id}`);
    }
  }
}
const served = JSON.parse(await fetchText("/assets/agent-templates.json", "application/json"));
assert.deepEqual(served, catalog, "public catalog must be the same staged snapshot");
assert.equal(await fetchText("/assets/template-catalog.js", "text/javascript"),
  await readFile(path.join(here, "../assets/template-catalog.js"), "utf8"),
  "gallery interaction script must be served unchanged");
assert.ok(checked.has("/reference/agent-templates"), "English usage entry point must resolve");
assert.ok(checked.has("/zh-CN/reference/agent-templates"), "Chinese usage entry point must resolve");
console.log(`Template bundle: bilingual homepages, ${catalog.templates.length} role detail pages per locale, links, anchors, search and JSON verified at ${catalog.source.sha}.`);
