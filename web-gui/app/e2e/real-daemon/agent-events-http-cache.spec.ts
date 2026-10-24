import http from "node:http";

import { expect, test } from "./daemon-fixture";

// Playwright routing disables HTTP caching. Use a real counting proxy instead,
// retaining the daemon's status/body and, normally, its response headers.
for (const legacyCache of [false, true]) {
  for (const auth of ["bearer", "cookie"] as const) {
    test(`events survive same-name recreation with ${auth} (${legacyCache ? "pre-upgrade cache" : "no-store server"})`, async ({
      daemonFactory,
      page,
    }) => {
      const daemon = await daemonFactory({ webDist: "dist-e2e" });
      const agentId = "http-cache-probe";
      const eventsPath = `/api/agents/${agentId}/events?limit=100&order=desc`;
      let eventRequests = 0;
      let seedLegacyCache = legacyCache;
      const proxy = http.createServer((request, response) => {
        if (request.url === "/cache-probe.html") {
          response.writeHead(200, { "Content-Type": "text/html" });
          response.end("<!doctype html><title>HTTP cache probe</title>");
          return;
        }
        if (request.url === eventsPath) eventRequests += 1;
        const upstream = http.request(`${daemon.baseUrl}${request.url}`, {
          method: request.method,
          headers: request.headers,
        }, (result) => {
          const headers = { ...result.headers };
          // Simulate an old server's missing header using a real deletion
          // response, not a mocked 410 or an artificially fresh cache entry.
          if (seedLegacyCache && request.url === eventsPath) {
            delete headers["cache-control"];
          }
          response.writeHead(result.statusCode ?? 502, headers);
          result.pipe(response);
        });
        upstream.on("error", () => {
          response.writeHead(502);
          response.end();
        });
        request.pipe(upstream);
      });
      await new Promise<void>((resolve, reject) => {
        proxy.once("error", reject);
        proxy.listen(0, "127.0.0.1", resolve);
      });
      try {
        const address = proxy.address();
        if (!address || typeof address === "string") throw new Error("missing proxy port");
        await page.goto(`http://127.0.0.1:${address.port}/cache-probe.html`);
        if (auth === "cookie") {
          const status = await page.evaluate(async (token) => {
            const response = await fetch("/api/auth/session/exchange", {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ credential: token }),
            });
            return response.status;
          }, daemon.token);
          expect(status).toBe(200);
        }
        const probe = async (cache?: RequestCache) => {
          const before = eventRequests;
          const result = await page.evaluate(async ({ url, token, auth, cache }) => {
            const response = await fetch(url, {
              headers: auth === "bearer" ? { Authorization: `Bearer ${token}` } : {},
              ...(cache ? { cache } : {}),
            });
            const body = await response.json();
            return {
              status: response.status,
              cacheControl: response.headers.get("cache-control"),
              code: body.code as string | undefined,
            };
          }, { url: eventsPath, token: daemon.token, auth, cache });
          return { ...result, requests: eventRequests - before };
        };
        const create = async () => {
          const response = await daemon.api(`/control/agents/${agentId}/create`, {
            method: "POST",
            body: JSON.stringify({}),
          });
          expect(response.ok).toBe(true);
          return await response.json() as { identity: { incarnation: number } };
        };
        expect((await create()).identity.incarnation).toBe(1);
        const deletion = await daemon.api(`/control/agents/${agentId}`, {
          method: "DELETE",
          body: JSON.stringify({}),
        });
        expect(deletion.ok).toBe(true);
        await expect.poll(async () => {
          const response = await daemon.api(`/control/agents/${agentId}/delete-status`);
          const body = await response.json() as { job: { status: string } };
          return body.job.status;
        }, { timeout: 15_000 }).toBe("completed");

        const first = await probe();
        expect(first).toMatchObject({ status: 410, code: "agent_deleted", requests: 1 });
        expect(first.cacheControl).toBe(legacyCache ? null : "no-store");
        expect(await probe()).toMatchObject({
          status: 410,
          requests: legacyCache ? 0 : 1,
        });

        // Upgrade the server without changing the browser URL/cache contents.
        seedLegacyCache = false;
        expect((await create()).identity.incarnation).toBe(2);
        expect(await probe()).toMatchObject({
          status: legacyCache ? 410 : 200,
          requests: legacyCache ? 0 : 1,
        });
        // The client policy must bypass responses cached before the upgrade.
        expect(await probe("no-store")).toMatchObject({
          status: 200,
          cacheControl: "no-store",
          requests: 1,
        });
      } finally {
        proxy.closeAllConnections();
        await new Promise<void>((resolve, reject) =>
          proxy.close((error) => error ? reject(error) : resolve()));
      }
    });
  }
}
