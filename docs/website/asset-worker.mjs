// Serve the docs site's binary assets from their own origin.
//
// `mdorigin build cloudflare --binary-mode external` stages every image,
// diagram and PDF under `.tools/dist/cloudflare/assets`. Those files are
// deployed twice: with the page Worker (a fallback for anything still using a
// relative path) and with this Worker, which owns `assets.holon.run`. A
// separate origin gives binaries their own TCP/TLS connection, so a slow or
// lossy download cannot stall the page connection, and HTTP/1.1 clients get a
// second connection pool instead of sharing the six-connection limit.
//
// Asset paths are stable (no content hashes), so the cache policy is a middle
// ground: one day fresh, then a week of stale-while-revalidate. See
// `.tools/rewrite-asset-host.mjs` for the reference rewrite that points pages
// here.
const CACHE_CONTROL = "public, max-age=86400, stale-while-revalidate=604800";

export default {
  async fetch(request, env) {
    const { pathname } = new URL(request.url);
    // Wrangler exposes the entry module under its own path; only asset paths
    // belong on this origin.
    if (!pathname.startsWith("/assets/")) {
      return new Response("Not Found", { status: 404 });
    }
    const response = await env.ASSETS.fetch(request);
    const result = new Response(response.body, response);
    if (result.status === 200 || result.status === 304) {
      result.headers.set("cache-control", CACHE_CONTROL);
    }
    return result;
  },
};
