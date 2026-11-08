import { describe, expect, it, vi } from "vitest";
import { createRuntimeClient, RuntimeHttpError } from "./client";

describe("avatar binary transport", () => {
  it("preserves API path prefixes, cookie auth, bearer headers, and custom fetch", async () => {
    const fetchImpl = vi.fn(async () => new Response(new Uint8Array([137, 80, 78, 71]), {
      headers: { "content-type": "image/png", etag: '"avatar"' },
    }));
    const client = createRuntimeClient({
      mode: "remote", baseUrl: "https://holon.example/prefix/api/", token: " secret ", fetchImpl,
    });
    const result = await client.getAgentAvatar("中文/a", { expression: "focused", size: 64 });
    const [input, init] = fetchImpl.mock.calls[0] as unknown as [URL, RequestInit];
    const url = new URL(input);
    expect(url.pathname).toBe("/prefix/api/agents/%E4%B8%AD%E6%96%87%2Fa/avatar");
    expect(Object.fromEntries(url.searchParams)).toEqual({
      version: "light-mech-1-vivid-2", expression: "focused", size: "64", format: "png",
    });
    expect(url.search).not.toContain("secret");
    expect(init.credentials).toBe("include");
    expect(new Headers(init.headers).get("authorization")).toBe("Bearer secret");
    expect(new Headers(init.headers).get("accept")).toBe("image/png");
    expect(result.blob.type).toBe("image/png");
    expect(result.headers.get("etag")).toBe('"avatar"');
    expect(new Uint8Array(await result.blob.arrayBuffer())).toEqual(new Uint8Array([137, 80, 78, 71]));
  });

  it("uses cookie authentication without putting credentials in the same-origin URL", async () => {
    const fetchImpl = vi.fn(async () => new Response("png", { headers: { "content-type": "image/png" } }));
    await createRuntimeClient({ mode: "local", fetchImpl }).getAgentAvatar("a");
    const [input, init] = fetchImpl.mock.calls[0] as unknown as [URL, RequestInit];
    expect(input.pathname).toBe("/api/agents/a/avatar");
    expect(input.searchParams.get("size")).toBe("128");
    expect(init.credentials).toBe("include");
    expect(new Headers(init.headers).has("authorization")).toBe(false);
  });

  it.each([401, 404, 503])("retains runtime HTTP error mapping for %s", async (status) => {
    const fetchImpl = vi.fn(async () => new Response(JSON.stringify({
      error: "avatar unavailable", code: status === 401 ? "auth_required" : "unavailable",
      hint: "retry later",
    }), {
      status, headers: { "content-type": "application/json", "retry-after": "2" },
    }));
    const client = createRuntimeClient({ mode: "local", fetchImpl });
    await expect(client.getAgentAvatar("a")).rejects.toMatchObject({
      name: "RuntimeHttpError", status, hint: "retry later", retryAfterSeconds: 2,
    });
    await expect(client.getAgentAvatar("a")).rejects.toBeInstanceOf(RuntimeHttpError);
  });

  it("forwards cancellation to the authenticated request", async () => {
    const controller = new AbortController();
    controller.abort();
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      init?.signal?.throwIfAborted();
      return new Response("unused");
    });
    await expect(createRuntimeClient({ mode: "local", fetchImpl }).getAgentAvatar("a", {
      signal: controller.signal,
    })).rejects.toMatchObject({ name: "AbortError" });
  });
});
