import { describe, expect, it, vi } from "vitest";
import {
  classifyConversationError,
  ConversationClient,
  ConversationHttpError,
  type ConversationClientLike,
  type ConversationStreamItem,
} from "@holon/conversation-sdk";

import { RuntimeHttpError } from "./client";
import { pinConversationClient } from "./incarnation-conversation-client";

const decoded = { decoded: true };
const streamItem: ConversationStreamItem = {
  type: "reset_required",
  reset: {
    type: "reset_required", reason: "retention_expired",
    oldest_retained_seq: 1, event_head_seq: 2, hint: "expired",
  },
};

function fixture() {
  const operation = vi.fn(async () => decoded as never);
  const source: ConversationClientLike = {
    baseUrl: "/api",
    requireCapability: operation,
    summary: operation,
    activities: operation,
    brief: operation,
    locateSource: operation,
    async *stream() { yield streamItem; yield streamItem; },
  };
  const identity = vi.fn(async (_agentId: string) => ({
    identity: { incarnation: 1, status: "active" },
  }));
  const client = pinConversationClient(source, {
    agentId: "child", incarnation: 1, getAgentIdentity: identity,
  });
  return { source, client, identity, operation };
}

const jsonOperations = [
  ["capability", (client: ConversationClientLike) => client.requireCapability()],
  ["summary", (client: ConversationClientLike) => client.summary("child")],
  ["activities", (client: ConversationClientLike) => client.activities("child", "turn")],
  ["brief", (client: ConversationClientLike) => client.brief("child", "brief")],
  ["locateSource", (client: ConversationClientLike) => client.locateSource!("child", "turn:turn")],
] as const;

describe("incarnation-pinned conversation client", () => {
  it("negotiates the SDK validator once and uses zero identity requests for N batches and JSON/304", async () => {
    const handshake = {
      ok: true, protocol: { name: "holon-control", version: 1 },
      auth: { mode: "none", required: false }, runtime: {},
      capabilities: ["agents.conversation-read.v1", "agents.conversation-incarnation.v1"],
    };
    const headers = { "x-holon-agent-id": "child", "x-holon-agent-incarnation": "1" };
    const frames = Array.from({ length: 50 }, (_, index) => {
      const common = { agent_id: "child", agent_incarnation: 1,
        event_log_epoch: "epoch", visibility_scope_id: "scope",
        batch_id: `batch-${index}`, through_seq: index + 1 };
      return [
        { ...common, type: "batch_begin", schema_version: 2, query_version: 2, runtime_id: "runtime", from_seq: index },
        { ...common, type: "checkpoint", checkpoint: `checkpoint-${index}` },
      ].map((frame) => `event: ${frame.type}\n${
        "checkpoint" in frame ? `id: ${frame.checkpoint}\n` : ""
      }data: ${JSON.stringify(frame)}\n\n`).join("");
    }).join("");
    const fetcher = vi.fn(async (url: RequestInfo | URL, init?: RequestInit) => {
      if (String(url).endsWith("/handshake")) return Response.json(handshake);
      expect(new Headers(init?.headers).get("x-holon-expected-incarnation")).toBe("1");
      if (String(url).includes("/stream")) return new Response(frames, { headers });
      return new Response(null, { status: 304, headers });
    });
    const identity = vi.fn();
    const client = pinConversationClient(new ConversationClient({
      baseUrl: "http://runtime.test/api", fetch: fetcher,
    }), { agentId: "child", incarnation: 1, getAgentIdentity: identity });
    await client.requireCapability();
    await client.summary("child", { ifNoneMatch: '"etag"' });
    const items = [];
    for await (const item of client.stream("child")) items.push(item);
    expect(items).toHaveLength(50);
    expect(identity).not.toHaveBeenCalled();
    expect(fetcher).toHaveBeenCalledTimes(3);
  });

  it("does not trust an advertised binding without SDK validation support", async () => {
    const { source, identity } = fixture();
    source.requireCapability = async () => ({ capabilities: ["agents.conversation-incarnation.v1"] });
    const client = pinConversationClient(source, { agentId: "child", incarnation: 1, getAgentIdentity: identity });
    await expect(client.summary("child")).rejects.toMatchObject({ body: { retryable: false } });
    expect(identity).not.toHaveBeenCalled();
  });

  it("an unknown capability retains strict legacy per-item validation", async () => {
    const { source, identity } = fixture();
    source.requireCapability = async () => ({ capabilities: ["agents.conversation-incarnation.v999"] });
    const client = pinConversationClient(source, { agentId: "child", incarnation: 1, getAgentIdentity: identity });
    for await (const _item of client.stream("child")) { /* consume */ }
    expect(identity).toHaveBeenCalledTimes(2);
  });

  it("aborts capability negotiation promptly and a later run does not inherit the canceled request", async () => {
    const { source, identity } = fixture();
    let release!: () => void;
    source.requireCapability = vi.fn()
      .mockImplementationOnce(() => new Promise((resolve) => { release = () => resolve({}); }))
      .mockResolvedValue({});
    const client = pinConversationClient(source, { agentId: "child", incarnation: 1, getAgentIdentity: identity });
    const abort = new AbortController();
    const pending = client.requireCapability(abort.signal);
    abort.abort();
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    await expect(client.summary("child")).resolves.toBe(decoded);
    release();
    expect(identity).toHaveBeenCalledOnce();
    expect(source.requireCapability).toHaveBeenCalledTimes(2);
  });

  it.each(jsonOperations)("returns %s only after checking the decoded result's identity", async (_name, invoke) => {
    const { client, identity, operation } = fixture();
    operation.mockImplementationOnce(async () => {
      expect(identity).not.toHaveBeenCalled();
      return decoded as never;
    });
    await expect(invoke(client)).resolves.toBe(decoded);
    expect(identity).toHaveBeenCalledExactlyOnceWith("child");
  });

  it.each(jsonOperations)("rejects a replacement while %s is in flight", async (_name, invoke) => {
    const { client, identity, operation } = fixture();
    operation.mockImplementationOnce(async () => {
      identity.mockResolvedValue({ identity: { incarnation: 2, status: "active" } });
      return decoded as never;
    });
    await expect(invoke(client)).rejects.toMatchObject({
      status: 403,
      body: {
        code: "conversation_identity_changed",
        error: "The conversation agent identity is no longer available.",
        retryable: false,
      },
    });
  });

  it.each(["deleting", "deleted"])("rejects the pinned incarnation when %s", async (status) => {
    const { client, identity } = fixture();
    identity.mockResolvedValue({ identity: { incarnation: 1, status } });
    await expect(client.summary("child")).rejects.toBeInstanceOf(ConversationHttpError);
  });

  it.each([401, 403, 404])("preserves identity access denial %s as a non-auto-retry SDK error", async (status) => {
    const { client, identity } = fixture();
    identity.mockRejectedValue(new RuntimeHttpError("GET", "/detail", status));
    const error = await client.summary("child").catch((error: unknown) => error);
    expect(error).toMatchObject({ status, body: { retryable: false } });
    expect(classifyConversationError(error)).toBe("recoverable");
  });

  it.each(jsonOperations)("does not release %s when identity lookup temporarily fails", async (_name, invoke) => {
    const { client, identity } = fixture();
    const error = new TypeError("Failed to fetch agent identity");
    identity.mockRejectedValueOnce(error);
    await expect(invoke(client)).rejects.toBe(error);
    await expect(invoke(client)).resolves.toBe(decoded);
    expect(identity).toHaveBeenCalledTimes(2);
  });

  it("does not release a stream item when identity lookup temporarily fails", async () => {
    const { client, identity } = fixture();
    const error = new TypeError("Failed to fetch agent identity");
    const stream = client.stream("child");
    await expect(stream.next()).resolves.toEqual({ done: false, value: streamItem });
    identity.mockRejectedValueOnce(error);
    await expect(stream.next()).rejects.toBe(error);
    await expect(client.stream("child").next()).resolves.toEqual({ done: false, value: streamItem });
  });

  it("rechecks every stream item without a reload or another admission check", async () => {
    const { client, identity } = fixture();
    const stream = client.stream("child");
    await expect(stream.next()).resolves.toEqual({ done: false, value: streamItem });
    identity.mockResolvedValue({ identity: { incarnation: 2, status: "active" } });
    await expect(stream.next()).rejects.toMatchObject({ status: 403 });
    expect(identity).toHaveBeenCalledTimes(2);
  });

  it("closes the source stream when the identity fence rejects an item", async () => {
    const { source, identity } = fixture();
    const closed = vi.fn();
    source.stream = async function* () {
      try { yield streamItem; } finally { closed(); }
    };
    identity.mockResolvedValue({ identity: { incarnation: 2, status: "active" } });
    const client = pinConversationClient(source, { agentId: "child", incarnation: 1, getAgentIdentity: identity });
    await expect(client.stream("child").next()).rejects.toMatchObject({ status: 403 });
    expect(closed).toHaveBeenCalledOnce();
  });

  it("does not invent an optional locate interface", () => {
    const { source, identity } = fixture();
    delete source.locateSource;
    const client = pinConversationClient(source, { agentId: "child", incarnation: 1, getAgentIdentity: identity });
    expect(client.locateSource).toBeUndefined();
  });

  it("does not release a concurrent response after another request detected replacement", async () => {
    const { client, identity } = fixture();
    let release!: (value: { identity: { incarnation: number; status: string } }) => void;
    identity.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    const pending = client.summary("child");
    await vi.waitFor(() => expect(identity).toHaveBeenCalledOnce());
    identity.mockResolvedValue({ identity: { incarnation: 2, status: "active" } });
    await expect(client.brief("child", "brief")).rejects.toMatchObject({ status: 403 });
    const rejected = expect(pending).rejects.toMatchObject({ status: 403 });
    release({ identity: { incarnation: 1, status: "active" } });
    await rejected;
  });

  it("checks after the real SDK finishes JSON decoding, not when fetch resolves", async () => {
    let incarnation = 1;
    const identity = vi.fn(async () => ({ identity: { incarnation, status: "active" } }));
    // Admission succeeds, then the first response arrives for that identity.
    await identity();
    let release!: () => void;
    const response = new Response();
    response.json = () => new Promise((resolve) => {
      release = () => resolve({
        schema_version: 2, query_version: 2,
        runtime_id: "runtime", event_log_epoch: "epoch", visibility_scope_id: "scope",
        snapshot_through_seq: 1, event_head_seq: 1, oldest_retained_seq: 1,
        snapshot_cursor: "checkpoint", turns: [], active_turns: [], pending_inputs: [],
        next_before_cursor: null, has_more: false,
      });
    });
    const client = pinConversationClient(new ConversationClient({
      baseUrl: "http://runtime.test/api",
      fetch: vi.fn(async (url) => String(url).endsWith("/handshake")
        ? Response.json({
          ok: true, protocol: { name: "holon-control", version: 1 },
          auth: { mode: "none", required: false }, runtime: {},
          capabilities: ["agents.conversation-read.v1"],
        })
        : response),
    }), { agentId: "child", incarnation: 1, getAgentIdentity: identity });
    const pending = client.summary("child");
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    expect(identity).toHaveBeenCalledOnce();
    incarnation = 2;
    const rejected = expect(pending).rejects.toMatchObject({ status: 403 });
    release();
    await rejected;
    expect(identity).toHaveBeenCalledTimes(2);
  });
});
