// Verify the actual deployment artifact, including mdorigin's code-config bundling.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import worker from "./dist/cloudflare/worker.mjs";
import { documentationLayout } from "../.theme/docs-layout.mjs";

const navigation = JSON.parse(await readFile(new URL("./dist/content/assets/docs-navigation.json", import.meta.url)));
const env = {
  ASSETS: {
    async fetch(request) {
      const pathname = new URL(typeof request === "string" ? request : request.url).pathname;
      try {
        return new Response(await readFile(new URL(`./dist/cloudflare/assets${pathname}`, import.meta.url)));
      } catch (error) {
        if (error.code === "ENOENT") return new Response("Not found", { status: 404 });
        throw error;
      }
    },
  },
};
async function fetchPage(route, accept = "text/html") {
  const response = await worker.fetch(new Request(`https://holon.invalid${route}`, {
    headers: { accept, "accept-language": route.startsWith("/zh-CN/") ? "zh-CN" : "en" },
  }), env, { waitUntil: (promise) => promise.catch((error) => { throw error; }) });
  assert.equal(response.status, 200, route);
  return response.text();
}

let pages = 0;
for (const [locale, groups] of Object.entries(navigation)) {
  for (const group of groups) {
    for (const entry of group.pages) {
      assert.equal(entry.href.startsWith("/zh-CN/"), locale === "zh-CN");
      const html = await fetchPage(entry.href);
      if (entry.href.endsWith("/")) {
        assert.ok(!html.includes('<div class="docs-layout'), `Index remains unchanged: ${entry.href}`);
        continue;
      }
      assert.ok(html.includes('<div class="docs-layout'), `Document shell: ${entry.href}`);
      assert.ok(html.includes('id="docs-content" tabindex="-1"'), `Skip target: ${entry.href}`);
      const sidebar = html.match(/<details class="docs-sidebar"[\s\S]*?<\/details>/)?.[0];
      assert.ok(sidebar, `Server-rendered navigation: ${entry.href}`);
      assert.equal((sidebar.match(/aria-current="page"/g) || []).length, 1, entry.href);
      assert.ok(sidebar.includes(`href="${entry.href}" aria-current="page"`), entry.href);
      for (const [, href] of sidebar.matchAll(/href="([^"]+)"/g)) {
        assert.equal(href.startsWith("/zh-CN/"), locale === "zh-CN", href);
      }
      const toc = html.match(/<details class="docs-toc"[\s\S]*?<\/details>/)?.[0] || "";
      for (const [, id] of toc.matchAll(/href="#([^"]+)"/g)) {
        assert.ok(html.includes(`id="${id}"`), `TOC target exists: ${entry.href}#${id}`);
      }
      const markdown = await fetchPage(entry.href, "text/markdown");
      assert.ok(!markdown.includes('class="docs-layout'), "Markdown negotiation remains unchanged");
      pages++;
    }
  }
}
for (const route of ["/", "/zh-CN/", "/blog/", "/zh-CN/blog/", "/templates/", "/zh-CN/templates/",
  "/privacy"]) {
  assert.ok(!(await fetchPage(route)).includes('<div class="docs-layout'), route);
}
const blogIndex = await fetchPage("/blog/");
const blogRoute = [...blogIndex.matchAll(/href="(\/blog\/[^"#?]+)"/g)]
  .map((match) => match[1]).find((href) => !href.endsWith("/") && !href.endsWith(".md"));
assert.ok(blogRoute, "Blog has at least one article");
assert.ok(!(await fetchPage(blogRoute)).includes('<div class="docs-layout'), blogRoute);
const redirect = await worker.fetch(new Request("https://holon.invalid/guides/quick-examples"),
  env, {});
assert.equal(redirect.status, 308);
assert.equal(new URL(redirect.headers.get("location"), "https://holon.invalid").pathname,
  "/getting-started/first-agent");
const alias = await fetchPage("/getting-started/first-agent");
assert.ok(alias.includes('href="/getting-started/first-agent" aria-current="page"'),
  "Aliases select navigation using the canonical page");

// Exercise headings that are rare in current content and routes without a TOC.
const plugin = documentationLayout(navigation);
const base = {
  kind: "page", sourcePath: "guides/example.md", locale: "en",
  requestPath: "/guides/example", canonicalPath: "/guides/example",
};
const rendered = plugin.renderPage({
  ...base,
  bodyHtml: '<h2 id="a">A &amp; <code>B</code></h2><h3 id="a-2">Again</h3><pre>&lt;h2 id="fake"&gt;Example&lt;/h2&gt;</pre>',
}, {}, (page) => page.bodyHtml);
assert.ok(rendered.includes('href="#a">A &amp; B</a>'));
assert.ok(rendered.includes('href="#a-2"'));
assert.ok(!rendered.includes('href="#fake"'));
assert.ok(plugin.renderPage({ ...base, bodyHtml: "<p>No headings</p>" }, {},
  (page) => page.bodyHtml).includes("docs-layout--no-toc"));
console.log(`Documentation layout verified for ${pages} bilingual details, indexes, Blog, aliases and Markdown.`);
