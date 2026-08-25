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
    };
    sessions.set(id, session);
  }
  return session;
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

function rosterSnapshot(visibleAgentIds) {
  return {
    contract_version: 1,
    runtime_id: "e2e-runtime",
    event_log_epoch: "e2e-epoch",
    visibility_scope_id: "e2e-scope",
    agents: visibleAgentIds.map((agentId) => ({
          agent: listEntry(agentId),
          event_window: { event_head_seq: agentId === "e2e-agent" ? 1 : 0, oldest_retained_seq: 0 },
          latest_brief: null,
        })),
  };
}

function emptyEventPage(agentId) {
  return {
    events: [],
    event_log_epoch: "e2e-epoch",
    has_older: false,
    has_newer: false,
    order: "asc",
    limit: 100,
    agent_id: agentId,
  };
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
  if (url.pathname === "/__e2e__/emit-agent" && req.method === "POST") {
    const { globalStreams, visibleAgentIds } = sessionFor(req, url);
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
    const frame = `data: ${JSON.stringify(envelope)}\n\n`;
    for (const stream of globalStreams) stream.write(frame);
    json(res, { emitted: true });
    return true;
  }
  return false;
}

function handleApi(req, res, url) {
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
    json(res, rosterSnapshot(session.visibleAgentIds));
    return true;
  }
  if (url.pathname === "/api/events/stream") {
    openEventStream(req, res, session.globalStreams);
    return true;
  }
  const projectionMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/projection-snapshot$/);
  if (projectionMatch) {
    json(res, { error: "capability unavailable", code: "capability_unavailable" }, 503);
    return true;
  }
  const eventsMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/events$/);
  if (eventsMatch) {
    json(res, emptyEventPage(decodeURIComponent(eventsMatch[1])));
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
    if (!handleApi(req, res, url)) json(res, { error: `unexpected fixture request: ${url.pathname}` }, 404);
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
