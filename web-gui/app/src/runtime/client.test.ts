import { describe, expect, it } from "vitest";

import { createRuntimeClient, projectModelOptions } from "./client";

describe("projectModelOptions", () => {
  it("detects reasoning effort support from runtime available model capabilities", () => {
    const options = projectModelOptions({
      available_models: [
        {
          model: "openai-codex/gpt-5.5",
          provider: "openai-codex",
          capabilities: {
            reasoning_summaries: true,
          },
        },
      ],
    });

    expect(options).toEqual([
      expect.objectContaining({
        model: "openai-codex/gpt-5.5",
        provider: "openai-codex",
        supportsReasoningEffort: true,
      }),
    ]);
  });
});

describe("createRuntimeClient", () => {
  it("preserves the configured remote connection mode even when the runtime auth mode is local", async () => {
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/handshake")) {
        return Response.json({ auth: { mode: "local" } });
      }
      if (url.endsWith("/agents/list")) {
        return Response.json([]);
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      token: "secret-token",
      fetchImpl: fetchImpl as typeof fetch,
    });

    const bootstrap = await client.getBootstrap();

    expect(bootstrap.connection).toEqual(
      expect.objectContaining({
        mode: "remote",
        source: "http",
        baseUrl: "http://example.test:7878",
        hasToken: true,
      }),
    );
  });
});
