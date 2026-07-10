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
            supports_reasoning: true,
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
        baseUrl: "http://example.test:7878/api",
        hasToken: true,
      }),
    );
  });

  it("uses the same-origin /api base for local runtime connections in production builds", async () => {
    const seen: string[] = [];
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith("/handshake")) {
        return Response.json({ auth: { mode: "local" } });
      }
      if (url.endsWith("/agents/list")) {
        return Response.json([]);
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "local",
      fetchImpl: fetchImpl as typeof fetch,
    });

    const bootstrap = await client.getBootstrap();

    expect(bootstrap.connection).toEqual(
      expect.objectContaining({
        mode: "local",
        source: "http",
        baseUrl: "/api",
      }),
    );
    expect(seen).toEqual(["/api/handshake", "/api/agents/list"]);
  });

  it("reports structured auth failures before fetching runtime data", async () => {
    const seen: string[] = [];
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith("/handshake")) {
        return Response.json({ ok: false, error: "missing bearer token", code: "auth_required" }, { status: 403 });
      }
      return Response.json([]);
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    const bootstrap = await client.getBootstrap();

    expect(bootstrap.connection).toEqual(
      expect.objectContaining({
        mode: "remote",
        source: "fixture",
        baseUrl: "http://example.test:7878/api",
        authRequired: true,
        error: "GET /handshake failed with 403: missing bearer token",
      }),
    );
    expect(seen).toEqual(["http://example.test:7878/api/handshake"]);
  });

  it("fetches workspace file blobs with bearer token headers", async () => {
    const seen: Array<{ url: string; authorization: string | null; accept: string | null }> = [];
    const fetchImpl = async (input: RequestInfo | URL, init?: RequestInit) => {
      const headers = new Headers(init?.headers);
      seen.push({
        url: String(input),
        authorization: headers.get("Authorization"),
        accept: headers.get("Accept"),
      });
      return new Response(new Blob(["png"], { type: "image/png" }), {
        headers: { "Content-Type": "image/png" },
      });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      token: "secret-token",
      fetchImpl: fetchImpl as typeof fetch,
    });

    const blob = await client.fetchWorkspaceFileBlob("ws/one", "outputs/chart 1.png", "root:ws");

    expect(blob.type).toBe("image/png");
    expect(seen).toEqual([
      {
        url: "http://example.test:7878/api/workspaces/ws%2Fone/files/outputs/chart%201.png?execution_root_id=root%3Aws",
        authorization: "Bearer secret-token",
        accept: "*/*",
      },
    ]);
  });

  it("sends generic file attachments in operator prompts", async () => {
    let requestBody: unknown;
    const fetchImpl = async (_input: RequestInfo | URL, init?: RequestInit) => {
      requestBody = init?.body ? JSON.parse(String(init.body)) : undefined;
      return new Response(null, { status: 204 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      token: "secret-token",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await client.sendOperatorPrompt("agent-one", "see attached", [
      {
        kind: "file",
        name: "report.pdf",
        mediaType: "application/pdf",
        dataBase64: "JVBERi0xLjc=",
      },
    ]);

    expect(requestBody).toEqual({
      text: "see attached",
      attachments: [
        {
          kind: "file",
          name: "report.pdf",
          media_type: "application/pdf",
          data_base64: "JVBERi0xLjc=",
        },
      ],
    });
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
    expect(seen).toEqual(["http://example.test:7878/api/agents/agent%2Fone/tool-executions/tool%2F42"]);
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
    expect(seen).toEqual(["http://example.test:7878/api/agents/agent%2Fone/tasks/task%2F42/output?block=false"]);
  });

  it("fetches an agent event window with pagination parameters", async () => {
    const seen: string[] = [];
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith("/agents/agent%2Fone/events?after_seq=739&limit=80&order=asc&max_level=info")) {
        return Response.json({
          events: [{ agent_id: "agent/one", event_seq: 740, ts: "2026-06-22T00:00:00Z", type: "message_enqueued" }],
          has_older: true,
          cursor_seq: 819,
        });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(
      client.getAgentEvents("agent/one", {
        afterSeq: 739,
        limit: 80,
        order: "asc",
        displayLevel: "info",
      }),
    ).resolves.toEqual(
      expect.objectContaining({
        events: [expect.objectContaining({ event_seq: 740 })],
        cursor_seq: 819,
      }),
    );
    expect(seen).toEqual(["http://example.test:7878/api/agents/agent%2Fone/events?after_seq=739&limit=80&order=asc&max_level=info"]);
  });

  it("installs skills through the generic job API", async () => {
    const seen: Array<{ url: string; body?: unknown }> = [];
    const fetchImpl = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      seen.push({ url, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url.endsWith("/jobs") && init?.method === "POST") {
        return Response.json({ job: { id: "job_123", kind: "skill.install", status: "queued" } }, { status: 202 });
      }
      if (url.endsWith("/jobs/job_123")) {
        return Response.json({ job: { id: "job_123", kind: "skill.install", status: "completed" } });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(client.addSkillToCatalog({ kind: "remote", package: "owner/repo" })).resolves.toBe("job_123");
    expect(seen).toEqual([
      {
        url: "http://example.test:7878/api/jobs",
        body: {
          kind: "skill.install",
          params: { kind: { kind: "remote", package: "owner/repo" } },
        },
      },
    ]);
  });

  it("posts runtime search filters for cross-agent all-workspace search", async () => {
    const seen: Array<{ url: string; body: unknown }> = [];
    const fetchImpl = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      seen.push({ url, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url.endsWith("/search")) {
        return Response.json({ query: "needle", limit: 10, results: [] });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(
      client.search("needle", {
        agentIds: ["holon-pm", "worker"],
        includeAllWorkspaces: true,
        limit: 10,
      }),
    ).resolves.toEqual({ query: "needle", limit: 10, results: [] });
    expect(seen).toEqual([
      {
        url: "http://example.test:7878/api/search",
        body: {
          query: "needle",
          agent_ids: ["holon-pm", "worker"],
          include_all_workspaces: true,
          limit: 10,
          types: ["message"],
        },
      },
    ]);
  });

  it("projects runtime search snippets as result previews", async () => {
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/search")) {
        return Response.json({
          query: "needle",
          limit: 1,
          results: [
            {
              kind: "message",
              source_ref: "message:msg-1",
              agent_id: "holon-pm",
              title: "Operator prompt",
              snippet: "needle appears in the message body",
              updated_at: "2026-06-21T00:00:00Z",
              metadata: {
                message_id: "msg-1",
                turn_id: "turn-1",
                message_seq: 42,
              },
            },
          ],
        });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(client.search("needle", { limit: 1 })).resolves.toEqual({
      query: "needle",
      limit: 1,
      results: [
        expect.objectContaining({
          kind: "message",
          preview: "needle appears in the message body",
          createdAt: "2026-06-21T00:00:00Z",
          locator: expect.objectContaining({
            evidenceId: "message:msg-1",
            sourceRef: "message:msg-1",
            messageId: "msg-1",
            turnId: "turn-1",
            eventSeq: 42,
          }),
        }),
      ],
    });
  });

  it("fetches full memory source content by source_ref", async () => {
    const seen: Array<{ url: string; body: unknown }> = [];
    const fetchImpl = async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      seen.push({ url, body: init?.body ? JSON.parse(String(init.body)) : undefined });
      if (url.endsWith("/memory/get")) {
        return Response.json({
          kind: "message",
          source_ref: "message:msg-1",
          title: "Operator prompt",
          content: "message_ref: message:msg-1\nbody:\nfull body",
          truncated: false,
          updated_at: "2026-06-21T00:00:00Z",
        });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(client.getMemorySource("message:msg-1", 1000)).resolves.toEqual({
      kind: "message",
      sourceRef: "message:msg-1",
      title: "Operator prompt",
      content: "message_ref: message:msg-1\nbody:\nfull body",
      truncated: false,
      updatedAt: "2026-06-21T00:00:00Z",
    });
    expect(seen).toEqual([
      {
        url: "http://example.test:7878/api/memory/get",
        body: { source_ref: "message:msg-1", max_chars: 1000 },
      },
    ]);
  });

  it("fetches agent work items from the scoped work-items endpoint", async () => {
    const seen: string[] = [];
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith("/agents/agent%2Fone/work-items?limit=25")) {
        return Response.json([
          { id: "work-current", objective: "Current", state: "open", plan_status: "ready" },
          { id: "work-done", objective: "Done", state: "completed" },
        ]);
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(client.getAgentWorkItems("agent/one", { limit: 25 })).resolves.toEqual([
      expect.objectContaining({ id: "work-current", objective: "Current", state: "open", planStatus: "ready" }),
      expect.objectContaining({ id: "work-done", objective: "Done", state: "completed" }),
    ]);
    expect(seen).toEqual(["http://example.test:7878/api/agents/agent%2Fone/work-items?limit=25"]);
  });

  it("fetches agent work item details from the scoped detail endpoint", async () => {
    const seen: string[] = [];
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith("/agents/agent%2Fone/work-items/work%2Fdetail")) {
        return Response.json({
          id: "work/detail",
          objective: "Inspect details",
          state: "open",
          plan_status: "ready",
          revision: 7,
          plan_artifact: { path: "/agent/work-items/work-detail/plan.md", preview: "1. Ship it" },
          todo_list: [{ text: "verify", state: "pending" }],
          result_summary: "not done yet",
        });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    await expect(client.getAgentWorkItem("agent/one", "work/detail")).resolves.toEqual(
      expect.objectContaining({
        id: "work/detail",
        objective: "Inspect details",
        state: "open",
        planStatus: "ready",
        revision: 7,
        planArtifact: expect.objectContaining({ path: "/agent/work-items/work-detail/plan.md", preview: "1. Ship it" }),
        todoList: [{ text: "verify", state: "pending" }],
        resultSummary: "not done yet",
      }),
    );
    expect(seen).toEqual(["http://example.test:7878/api/agents/agent%2Fone/work-items/work%2Fdetail"]);
  });

  it("hydrates persisted brief text even when the associated transcript is available", async () => {
    const seen: string[] = [];
    const fetchImpl = async (input: RequestInfo | URL) => {
      const url = String(input);
      seen.push(url);
      if (url.endsWith("/agents/list")) {
        return Response.json([{ identity: { agent_id: "agent-one" } }]);
      }
      if (url.endsWith("/agents/agent-one/state")) {
        return Response.json({});
      }
      if (url.includes("/agents/agent-one/events?")) {
        return Response.json({
          events: [
            {
              id: "brief-event",
              agent_id: "agent-one",
              event_seq: 23,
              ts: "2026-07-10T00:00:00Z",
              type: "brief_created",
              payload: {
                brief_id: "brief-123",
                kind: "result",
                finalizes_assistant_round_id: "round-123",
              },
            },
          ],
          has_older: false,
        });
      }
      if (url.endsWith("/agents/agent-one/work-items?limit=50")) {
        return Response.json([]);
      }
      if (url.endsWith("/agents/agent-one/transcript:batchGet")) {
        return Response.json({
          entries: [
            {
              id: "round-123",
              data: {
                blocks: [
                  { type: "thinking", text: "Internal reasoning must not be visible." },
                  { type: "text", text: "Transcript final text." },
                ],
              },
            },
          ],
          missing_entry_ids: [],
        });
      }
      if (url.endsWith("/agents/agent-one/briefs/brief-123")) {
        return Response.json({
          id: "brief-123",
          text: "Canonical persisted brief.",
          kind: "result",
          created_at: "2026-07-10T00:00:00Z",
        });
      }
      return new Response("not found", { status: 404 });
    };

    const client = createRuntimeClient({
      mode: "remote",
      baseUrl: "http://example.test:7878",
      fetchImpl: fetchImpl as typeof fetch,
    });

    const detail = await client.getAgentDetail("agent-one");

    expect(detail.agent.lastBrief).toBe("Canonical persisted brief.");
    expect(detail.timeline[0]).toEqual(
      expect.objectContaining({
        id: "brief-event",
        body: "Canonical persisted brief.",
      }),
    );
    expect(seen).toContain("http://example.test:7878/api/agents/agent-one/briefs/brief-123");
  });
});
