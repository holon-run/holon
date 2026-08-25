import http from "node:http";
import { createServer as createViteServer } from "vite";

const requestedPort = Number(process.argv[process.argv.indexOf("--port") + 1]);
if (!Number.isInteger(requestedPort) || requestedPort <= 0) {
  throw new Error("fixture server requires --port <port>");
}

const sessions = new Map();

const vite = await createViteServer({
  appType: "spa",
  mode: "e2e",
  server: { middlewareMode: true },
});

function record(req, url) {
  if (url.pathname.startsWith("/api/")) {
    sessionFor(req, url).requests.push(`${req.method} ${url.pathname}${url.search}`);
  }
}

function sessionId(req, url) {
  const controlSession = url.searchParams.get("session");
  if (controlSession) return controlSession;
  const cookie = req.headers.cookie
    ?.split(";")
    .map((entry) => entry.trim().split("="))
    .find(([name]) => name === "holon_e2e_session");
  return cookie ? decodeURIComponent(cookie.slice(1).join("=")) : "default";
}

function sessionFor(req, url) {
  const id = sessionId(req, url);
  let session = sessions.get(id);
  if (!session) {
    session = {
      requests: [],
      globalStreams: new Set(),
      agentStreams: new Set(),
      visibleAgentIds: ["bootstrap-agent"],
      ledgerEnabledAgentIds: new Set(),
      eventsByAgentId: new Map(),
      briefsById: new Map(),
      blockedBriefIds: new Set(),
      streamGeneration: 0,
    };
    sessions.set(id, session);
  }
  return session;
}

async function requestBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  if (!chunks.length) return {};
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function json(res, body, status = 200) {
  res.writeHead(status, {
    "Cache-Control": "no-store",
    "Content-Type": "application/json",
  });
  res.end(JSON.stringify(body));
}

function openEventStream(req, res, clients) {
  res.writeHead(200, {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "Content-Type": "text/event-stream",
  });
  res.write(": connected\n\n");
  clients.add(res);
  req.on("close", () => clients.delete(res));
}

function eventHead(session, agentId) {
  return Math.max(
    0,
    ...(session.eventsByAgentId.get(agentId) ?? []).map((event) => event.event_seq),
  );
}

function listEntry(agentId) {
  return {
    identity: {
      agent_id: agentId,
      visibility: "public",
      ownership: "self_owned",
      profile_preset: "public_named",
    },
    status: "awake_idle",
    pending: 0,
  };
}

function agentState(agentId) {
  return {
    agent: {
      identity: {
        ...listEntry(agentId).identity,
        kind: "named",
        status: "active",
        is_default_agent: false,
      },
      agent: {
        id: agentId,
        status: "awake_idle",
        current_run_id: null,
        pending: 0,
        attached_workspaces: [],
        turn_index: 0,
      },
      scheduling_posture: { posture: "idle", reason: "idle" },
      active_task_count: 0,
      lifecycle: { accepts_external_messages: true },
      model: {
        source: "runtime_default",
        runtime_default_model: "e2e/model",
        effective_model: "e2e/model",
        fallback_active: false,
      },
      closure: { outcome: "completed", runtime_posture: "awake" },
    },
    session: { current_run_id: null, pending_count: 0, last_turn: null },
    tasks: [],
    timers: [],
    work_items: [],
    external_triggers: [],
    workspace: { workspaces: [] },
  };
}

function rosterSnapshot(session) {
  return {
    contract_version: 1,
    runtime_id: "e2e-runtime",
    event_log_epoch: "e2e-epoch",
    visibility_scope_id: "e2e-scope",
    agents: session.visibleAgentIds.map((agentId) => ({
          agent: listEntry(agentId),
          event_window: { event_head_seq: eventHead(session, agentId), oldest_retained_seq: 0 },
          latest_brief: null,
        })),
  };
}

function eventPage(session, agentId, url) {
  const order = url.searchParams.get("order") === "desc" ? "desc" : "asc";
  const afterSeq = Number(url.searchParams.get("after_seq") ?? 0);
  const limit = Number(url.searchParams.get("limit") ?? 100);
  const all = [...(session.eventsByAgentId.get(agentId) ?? [])]
    .filter((event) => order === "desc" || event.event_seq > afterSeq)
    .sort((left, right) =>
      order === "desc"
        ? right.event_seq - left.event_seq
        : left.event_seq - right.event_seq
    );
  const events = all.slice(0, limit);
  return {
    events,
    event_log_epoch: "e2e-epoch",
    has_older: false,
    has_newer: false,
    order,
    limit,
    agent_id: agentId,
    cursor_seq: eventHead(session, agentId),
    newest_seq: eventHead(session, agentId),
    oldest_seq: all.length ? Math.min(...all.map((event) => event.event_seq)) : null,
  };
}

function projectionSnapshot(session, agentId) {
  return {
    contract_version: 1,
    runtime_id: "e2e-runtime",
    visibility_scope_id: "e2e-scope",
    event_log_epoch: "e2e-epoch",
    agent_id: agentId,
    snapshot_through_seq: 0,
    event_head_seq: eventHead(session, agentId),
    oldest_retained_seq: 0,
    projection: {
      agent: listEntry(agentId),
      conversation: { latest_message_id: null, latest_transcript_entry_id: null },
      current_work_item: null,
      hydration_references: [],
      hydration_tombstones: [],
      latest_brief: null,
    },
  };
}

function writeEvent(streams, envelope) {
  const frame = `data: ${JSON.stringify(envelope)}\n\n`;
  for (const stream of streams) stream.write(frame);
}

async function handleControl(req, res, url) {
  if (url.pathname === "/__e2e__/health") {
    json(res, { ok: true });
    return true;
  }
  if (url.pathname === "/__e2e__/requests") {
    json(res, { requests: sessionFor(req, url).requests });
    return true;
  }
  if (url.pathname === "/__e2e__/state") {
    const session = sessionFor(req, url);
    json(res, {
      requests: session.requests,
      visibleAgentIds: session.visibleAgentIds,
      globalStreamCount: session.globalStreams.size,
      agentStreamCount: session.agentStreams.size,
      streamGeneration: session.streamGeneration,
      blockedBriefIds: [...session.blockedBriefIds],
    });
    return true;
  }
  if (url.pathname === "/__e2e__/disconnect-streams" && req.method === "POST") {
    const session = sessionFor(req, url);
    for (const stream of [...session.globalStreams, ...session.agentStreams]) {
      stream.end();
    }
    session.globalStreams.clear();
    session.agentStreams.clear();
    json(res, { disconnected: true });
    return true;
  }
  if (url.pathname === "/__e2e__/configure" && req.method === "POST") {
    const session = sessionFor(req, url);
    const body = await requestBody(req);
    if (Array.isArray(body.visibleAgentIds)) {
      session.visibleAgentIds = [...body.visibleAgentIds];
    }
    if (Array.isArray(body.ledgerEnabledAgentIds)) {
      session.ledgerEnabledAgentIds = new Set(body.ledgerEnabledAgentIds);
    }
    if (body.eventsByAgentId && typeof body.eventsByAgentId === "object") {
      session.eventsByAgentId = new Map(
        Object.entries(body.eventsByAgentId).map(([agentId, events]) => [
          agentId,
          Array.isArray(events) ? [...events] : [],
        ]),
      );
    }
    if (body.briefsById && typeof body.briefsById === "object") {
      session.briefsById = new Map(Object.entries(body.briefsById));
    }
    if (Array.isArray(body.blockedBriefIds)) {
      session.blockedBriefIds = new Set(body.blockedBriefIds);
    }
    json(res, { configured: true });
    return true;
  }
  if (url.pathname === "/__e2e__/append-event" && req.method === "POST") {
    const session = sessionFor(req, url);
    const body = await requestBody(req);
    const envelope = body.envelope;
    if (!envelope?.agent_id || !Number.isFinite(envelope.event_seq)) {
      json(res, { error: "append-event requires an envelope" }, 400);
      return true;
    }
    const events = session.eventsByAgentId.get(envelope.agent_id) ?? [];
    if (!events.some((event) => event.event_seq === envelope.event_seq)) {
      events.push(envelope);
      session.eventsByAgentId.set(envelope.agent_id, events);
    }
    if (body.broadcast !== false) writeEvent(session.globalStreams, envelope);
    json(res, { appended: true });
    return true;
  }
  if (url.pathname === "/__e2e__/release-briefs" && req.method === "POST") {
    const session = sessionFor(req, url);
    const body = await requestBody(req);
    for (const briefId of body.briefIds ?? []) session.blockedBriefIds.delete(briefId);
    json(res, { released: true });
    return true;
  }
  if (url.pathname === "/__e2e__/emit-agent" && req.method === "POST") {
    const session = sessionFor(req, url);
    const { globalStreams, visibleAgentIds } = session;
    if (!visibleAgentIds.includes("e2e-agent")) visibleAgentIds.push("e2e-agent");
    const envelope = {
      id: "e2e-event-1",
      event_seq: 1,
      event_log_epoch: "e2e-epoch",
      contract_version: 1,
      ts: "2026-08-25T00:00:00Z",
      agent_id: "e2e-agent",
      type: "agent_state_changed",
      payload_schema: "holon.runtime_event.legacy",
      payload_schema_version: 1,
      payload: {},
    };
    const events = session.eventsByAgentId.get("e2e-agent") ?? [];
    events.push(envelope);
    session.eventsByAgentId.set("e2e-agent", events);
    writeEvent(globalStreams, envelope);
    json(res, { emitted: true });
    return true;
  }
  return false;
}

async function handleApi(req, res, url) {
  record(req, url);
  const session = sessionFor(req, url);
  if (url.pathname === "/api/handshake") {
    json(res, { auth: { mode: "none" }, capabilities: ["agents.list", "agents.roster-snapshot.v1"] });
    return true;
  }
  if (url.pathname === "/api/agents/list") {
    json(res, session.visibleAgentIds.map(listEntry));
    return true;
  }
  if (url.pathname === "/api/agents/snapshot") {
    json(res, rosterSnapshot(session));
    return true;
  }
  const stateMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/state$/);
  if (stateMatch) {
    json(res, agentState(decodeURIComponent(stateMatch[1])));
    return true;
  }
  if (url.pathname === "/api/events/stream") {
    session.streamGeneration += 1;
    openEventStream(req, res, session.globalStreams);
    return true;
  }
  const projectionMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/projection-snapshot$/);
  if (projectionMatch) {
    const agentId = decodeURIComponent(projectionMatch[1]);
    if (session.ledgerEnabledAgentIds.has(agentId)) {
      json(res, projectionSnapshot(session, agentId));
      return true;
    }
    json(res, { error: "capability unavailable", code: "capability_unavailable" }, 503);
    return true;
  }
  const eventsMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/events$/);
  if (eventsMatch) {
    json(res, eventPage(session, decodeURIComponent(eventsMatch[1]), url));
    return true;
  }
  const briefsMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/briefs:batchGet$/);
  if (briefsMatch && req.method === "POST") {
    const body = await requestBody(req);
    const briefIds = Array.isArray(body.brief_ids) ? body.brief_ids : [];
    if (briefIds.some((briefId) => session.blockedBriefIds.has(briefId))) {
      return true;
    }
    const briefs = briefIds.flatMap((briefId) => {
      const brief = session.briefsById.get(briefId);
      return brief ? [brief] : [];
    });
    json(res, {
      briefs,
      missing_brief_ids: briefIds.filter((briefId) => !session.briefsById.has(briefId)),
    });
    return true;
  }
  if (/^\/api\/agents\/[^/]+\/events\/stream$/.test(url.pathname)) {
    openEventStream(req, res, session.agentStreams);
    return true;
  }
  return false;
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host}`);
  if (await handleControl(req, res, url)) return;
  if (url.pathname.startsWith("/api/")) {
    if (!(await handleApi(req, res, url))) {
      json(res, { error: `unexpected fixture request: ${url.pathname}` }, 404);
    }
    return;
  }
  vite.middlewares(req, res, () => {
    res.writeHead(404);
    res.end("not found");
  });
});

await new Promise((resolve, reject) => {
  server.once("error", reject);
  server.listen(requestedPort, "127.0.0.1", resolve);
});

async function shutdown() {
  for (const session of sessions.values()) {
    for (const stream of [...session.globalStreams, ...session.agentStreams]) stream.end();
  }
  await vite.close();
  await new Promise((resolve) => server.close(resolve));
}

process.once("SIGINT", () => void shutdown());
process.once("SIGTERM", () => void shutdown());
