import assert from "node:assert/strict";
import { test } from "node:test";
import { ApiClient, ApiHttpError } from "../dist/index.js";

test("binary requests share URL, fetch, asynchronous headers, token, and cancellation", async () => {
  const bytes = new Uint8Array([0, 255, 137, 80, 78, 71]);
  const controller = new AbortController();
  const calls = [];
  const client = new ApiClient({
    baseUrl: "https://holon.example/prefix/api/",
    headers: async () => ({ "x-client": "test", accept: "application/json" }),
    bearerToken: async () => "secret",
    fetch: async (url, init) => {
      calls.push({ url, init });
      return new Response(bytes, { headers: { "content-type": "image/png", etag: '"avatar"' } });
    },
  });
  const response = await client.requestBinary("agents/a/avatar", {
    headers: { accept: "image/png" }, signal: controller.signal,
  });
  assert.equal(String(calls[0].url), "https://holon.example/prefix/api/agents/a/avatar");
  assert.equal(calls[0].url.searchParams.has("token"), false);
  assert.equal(calls[0].init.headers.get("authorization"), "Bearer secret");
  assert.equal(calls[0].init.headers.get("x-client"), "test");
  assert.equal(calls[0].init.headers.get("accept"), "image/png");
  assert.equal(calls[0].init.signal, controller.signal);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("etag"), '"avatar"');
  assert.equal(response.blob.type, "image/png");
  assert.deepEqual(new Uint8Array(await response.blob.arrayBuffer()), bytes);
});

test("binary errors preserve the JSON error envelope and retry headers", async () => {
  const body = { error: { message: "Avatar renderer busy" } };
  const client = new ApiClient({
    baseUrl: "https://holon.example/api/",
    fetch: async () => new Response(JSON.stringify(body), {
      status: 503, headers: { "content-type": "application/json", "retry-after": "1" },
    }),
  });
  await assert.rejects(client.requestBinary("agents/a/avatar"), (error) => {
    assert.ok(error instanceof ApiHttpError);
    assert.equal(error.status, 503);
    assert.equal(error.message, body.error.message);
    assert.deepEqual(error.body, body);
    assert.equal(error.headers.get("retry-after"), "1");
    return true;
  });
});

test("JSON requests retain existing decoding and error behavior", async () => {
  const client = new ApiClient({
    baseUrl: "https://holon.example/api/",
    fetch: async () => new Response('{"ok":true}'),
  });
  assert.deepEqual(await client.request("handshake"), { ok: true });
});

test("binary requests pass aborts through without remapping", async () => {
  const controller = new AbortController();
  controller.abort();
  const client = new ApiClient({
    baseUrl: "https://holon.example/api/",
    fetch: async (_url, init) => { init.signal.throwIfAborted(); },
  });
  await assert.rejects(client.requestBinary("agents/a/avatar", { signal: controller.signal }), {
    name: "AbortError",
  });
});
