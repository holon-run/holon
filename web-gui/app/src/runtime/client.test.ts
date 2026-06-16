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

  it("fetches full tool execution detail for inspector hydration", async () => {
    const seen: string[] = [];
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith("/agents/agent%2Fone/tool-executions/tool%2F42")) {
        return Response.json({ id: "tool/42", tool_name: "ExecCommand", result: { stdout: "full output" } });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(client.getToolExecution("agent/one", "tool/42")).resolves.toEqual(
      expect.objectContaining({
        id: "tool/42",
        result: expect.objectContaining({ stdout: "full output" }),
      }),
    );
    expect(seen).toEqual(["http://example.test:7878/agents/agent%2Fone/tool-executions/tool%2F42"]);
  });

  it("fetches task output without blocking for inspector hydration", async () => {
    const seen: string[] = [];
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith("/agents/agent%2Fone/tasks/task%2F42/output?block=false")) {
        return Response.json({
          retrieval_status: "success",
          task: { task_id: "task/42", status: "completed", output_preview: "full task output" },
        });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(client.getTaskOutput("agent/one", "task/42")).resolves.toEqual(
      expect.objectContaining({
        retrieval_status: "success",
        task: expect.objectContaining({
          status: "completed",
          output_preview: "full task output",
        }),
      }),
    );
    expect(seen).toEqual(["http://example.test:7878/agents/agent%2Fone/tasks/task%2F42/output?block=false"]);
  });
});
