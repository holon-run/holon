import http from "node:http";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { createServer as createViteServer } from "vite";
import { tourAgents, tourApi, seedTour } from "./tour/scenario.mjs";

const tour = process.argv.includes("--tour");
const production = process.env.HOLON_E2E_PRODUCTION === "1";
const productionRoot = new URL("../dist/", import.meta.url);

const requestedPort = Number(process.argv[process.argv.indexOf("--port") + 1]);
if (!Number.isInteger(requestedPort) || requestedPort <= 0) {
  throw new Error("fixture server requires --port <port>");
}

const sessions = new Map();
const EVENT_CONTRACT_VERSION = "3";
const EVENT_CONTRACT_VERSION_HEADER = "x-holon-event-contract-version";

const vite = production ? null : await createViteServer({
  appType: "spa",
  mode: "e2e",
  server: { hmr: false, middlewareMode: true, allowedHosts: ["holon-http.test"] },
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
      abortRequests: [],
      abortResponse: null,
      conversationStreams: new Set(),
      conversationData: new Map(),
      visibleAgentIds: tour ? tourAgents.map((agent) => agent.id) : ["bootstrap-agent"],
      ledgerEnabledAgentIds: new Set(),
      eventsByAgentId: new Map(),
      briefsById: new Map(),
      toolExecutionsById: new Map(),
      blockedBriefIds: new Set(),
      briefReadCursors: new Map(),
      failConversationByAgentId: new Set(),
      runtimeId: "e2e-runtime",
      eventLogEpoch: "e2e-epoch",
      visibilityScopeId: "e2e-scope",
      oldestRetainedSeqByAgentId: new Map(),
      currentRunIdByAgentId: new Map(),
      snapshotThroughSeqByAgentId: new Map(),
      projectionLatestBriefByAgentId: new Map(),
      rightPanelByAgentId: new Map(),
    };
    if (tour) seedTour(session);
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

function json(res, body, status = 200, extraHeaders = {}) {
  res.writeHead(status, {
    "Cache-Control": "no-store",
    "Content-Type": "application/json",
    ...extraHeaders,
  });
  res.end(JSON.stringify(body));
}

function agentResource(req, res, session, agentId, body, status = 200) {
  const identity = listEntry(agentId, session).identity;
  const expectedIncarnation = req.headers["x-holon-expected-incarnation"];
  if (expectedIncarnation !== undefined && expectedIncarnation !== String(identity.incarnation)) {
    json(res, { error: "conversation_identity_changed" }, 409);
    return;
  }
  json(res, body, status, {
    "x-holon-agent-id": identity.agent_id,
    "x-holon-agent-incarnation": String(identity.incarnation),
  });
}

function resourceFailure(fixture, resource) {
  return fixture?.failureStatusByResource?.[resource]
    ?? (fixture?.failResources?.includes(resource) ? 503 : undefined);
}

function openEventStream(req, res, clients) {
  res.writeHead(200, {
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "Content-Type": "text/event-stream",
    [EVENT_CONTRACT_VERSION_HEADER]: EVENT_CONTRACT_VERSION,
  });
  res.write(": connected\n\n");
  clients.add(res);
  req.on("close", () => clients.delete(res));
}

/**
 * Minimal turn derivation for the conversation read model: each
 * brief_created event becomes one settled operator turn carrying the brief,
 * so the GUI timeline can render brief results on demand.
 */
function conversationCursor(session, agentId, head) {
  return Buffer.from(JSON.stringify([
    session.runtimeId, session.visibilityScopeId, session.eventLogEpoch, agentId, head,
  ])).toString("base64url");
}

function conversationTurns(session, agentId) {
  return (session.eventsByAgentId.get(agentId) ?? [])
    .filter(
      (event) =>
        event.type === "brief_created"
        && typeof event.payload?.brief_id === "string"
        && Number.isFinite(event.event_seq),
    )
    .map((event, index) => ({
      turn_id: `turn-${event.event_seq}`,
      key: { turn_index: index + 1, turn_id: `turn-${event.event_seq}` },
      revision: 1,
      presentation_class: "operator",
      inputs: [],
      execution: { kind: "terminal", outcome: "completed" },
      result: { kind: "available" },
      settled: true,
      attention: null,
      detail_coverage: { kind: "complete" },
      brief_ids: [event.payload.brief_id],
    }));
}

function eventHead(session, agentId) {
  return Math.max(
    0,
    ...(session.eventsByAgentId.get(agentId) ?? []).map((event) => event.event_seq),
  );
}

function briefReadState(session, agentId) {
  const key = [
    session.runtimeId,
    session.visibilityScopeId,
    session.eventLogEpoch,
    agentId,
  ].join("\u0000");
  const readThroughEventSeq = session.briefReadCursors.get(key) ?? 0;
  const eventHeadSeq = eventHead(session, agentId);
  const oldestRetainedSeq = session.oldestRetainedSeqByAgentId.get(agentId) ?? 0;
  const retentionGap = oldestRetainedSeq > 0 && readThroughEventSeq < oldestRetainedSeq - 1;
  const unreadCount = (session.eventsByAgentId.get(agentId) ?? [])
    .filter((event) => event.type === "brief_created" && event.event_seq > readThroughEventSeq)
    .length;
  return {
    agent_id: agentId,
    event_head_seq: eventHeadSeq,
    event_log_epoch: session.eventLogEpoch,
    oldest_retained_seq: oldestRetainedSeq,
    read_through_event_seq: readThroughEventSeq,
    reset_required: false,
    retention_gap: retentionGap,
    revision: session.briefReadCursors.has(key) ? 1 : 0,
    unread_count: unreadCount,
    visibility_scope_id: session.visibilityScopeId,
  };
}

function listEntry(agentId, session) {
  if (tour) return tourAgents.find((agent) => agent.id === agentId)?.entry;
  const currentRunId = session ? (session.currentRunIdByAgentId.get(agentId) ?? null) : null;
  const fixture = session?.rightPanelByAgentId.get(agentId);
  return {
    identity: {
      agent_id: agentId,
      incarnation: fixture?.incarnation ?? 1,
      visibility: "public",
      ownership: "self_owned",
      profile_preset: "public_named",
    },
    status: currentRunId ? "awake_running" : "awake_idle",
    current_run_id: currentRunId,
    pending: 0,
    ...(fixture?.activeWorkspaceEntry ? { active_workspace_entry: fixture.activeWorkspaceEntry } : {}),
  };
}

function agentState(agentId, session) {
  if (tour) return tourAgents.find((agent) => agent.id === agentId)?.state;
  const rightPanel = session?.rightPanelByAgentId.get(agentId);
  const running = Boolean(session?.currentRunIdByAgentId.get(agentId));
  return {
    agent: {
      identity: {
        ...listEntry(agentId, session).identity,
        kind: "named",
        status: "active",
        is_default_agent: false,
      },
      agent: {
        id: agentId,
        status: running ? "awake_running" : "awake_idle",
        current_run_id: running ? (session.currentRunIdByAgentId.get(agentId) ?? null) : null,
        ...(rightPanel ? { current_work_item_id: rightPanel.currentWorkItemId } : {}),
        pending: 0,
        attached_workspaces: [],
        turn_index: 0,
      },
      scheduling_posture: { posture: running ? "active-turn" : "idle", reason: running ? "model_turn" : "idle" },
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
    session: { current_run_id: running ? (session.currentRunIdByAgentId.get(agentId) ?? null) : null, pending_count: 0, last_turn: null },
    tasks: rightPanel?.tasks ?? [],
    timers: [],
    work_items: rightPanel?.stateWorkItems ?? rightPanel?.workItems ?? [],
    external_triggers: [],
    workspace: { workspaces: rightPanel?.activeWorkspaceEntry ? [{ ...rightPanel.activeWorkspaceEntry, is_active: true }] : [] },
  };
}

function rosterSnapshot(session) {
  return {
    contract_version: 1,
    runtime_id: session.runtimeId,
    event_log_epoch: session.eventLogEpoch,
    visibility_scope_id: session.visibilityScopeId,
    agents: session.visibleAgentIds.map((agentId) => ({
          agent: listEntry(agentId, session),
          event_window: {
            event_head_seq: eventHead(session, agentId),
            oldest_retained_seq: session.oldestRetainedSeqByAgentId.get(agentId) ?? 0,
          },
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
    event_log_epoch: session.eventLogEpoch,
    contract_version: Number(EVENT_CONTRACT_VERSION),
    has_older: false,
    has_newer: false,
    order,
    limit,
    agent_id: agentId,
    cursor_seq: eventHead(session, agentId),
    newest_seq: eventHead(session, agentId),
    oldest_seq: session.oldestRetainedSeqByAgentId.get(agentId)
      ?? (all.length ? Math.min(...all.map((event) => event.event_seq)) : null),
  };
}

function projectionSnapshot(session, agentId) {
  const snapshotThroughSeq =
    session.snapshotThroughSeqByAgentId.get(agentId) ?? 0;
  return {
    contract_version: 1,
    runtime_id: session.runtimeId,
    visibility_scope_id: session.visibilityScopeId,
    event_log_epoch: session.eventLogEpoch,
    agent_id: agentId,
    snapshot_through_seq: snapshotThroughSeq,
    event_head_seq: eventHead(session, agentId),
    oldest_retained_seq: session.oldestRetainedSeqByAgentId.get(agentId) ?? 0,
    projection: {
      agent: listEntry(agentId, session),
      conversation: { latest_message_id: null, latest_transcript_entry_id: null },
      current_work_item: null,
      hydration_references: [],
      hydration_tombstones: [],
      latest_brief: session.projectionLatestBriefByAgentId.get(agentId) ?? null,
    },
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
  if (url.pathname === "/__e2e__/right-panel") {
    const session = sessionFor(req, url);
    if (req.method === "GET") {
      json(res, { streamAgentIds: [...session.conversationStreams].map((stream) => stream.conversationAgentId) });
      return true;
    }
    if (req.method !== "POST") {
      json(res, { error: "use GET or POST" }, 405);
      return true;
    }
    const { agentId, notify, ...update } = await requestBody(req);
    if (typeof agentId !== "string") {
      json(res, { error: "agentId is required" }, 400);
      return true;
    }
    session.rightPanelByAgentId.set(agentId, {
      workItems: [], currentWorkItemId: null, failResources: [],
      ...session.rightPanelByAgentId.get(agentId), ...update,
    });
    if (notify === "checkpoint") {
      const previous = session.conversationData.get(agentId);
      const fromSeq = previous?.head ?? eventHead(session, agentId);
      const head = fromSeq + 1000;
      // Advance coverage without fabricating a turn, activity, or raw event.
      session.conversationData.set(agentId, {
        turns: previous?.turns ?? conversationTurns(session, agentId),
        activitiesByTurnId: {}, pending_inputs: [], ...previous, head,
      });
      const scope = { event_log_epoch: session.eventLogEpoch, visibility_scope_id: session.visibilityScopeId };
      const batchId = `right-panel-${head}`;
      const changes = [
        { type: "batch_begin", batch_id: batchId, schema_version: 2, query_version: 2,
          runtime_id: session.runtimeId, ...scope, from_seq: fromSeq, through_seq: head },
        { type: "checkpoint", batch_id: batchId, ...scope, through_seq: head,
          checkpoint: conversationCursor(session, agentId, head) },
      ];
      for (const stream of session.conversationStreams) {
        if (stream.conversationAgentId !== agentId) continue;
        for (const change of changes) {
          const id = change.type === "checkpoint" ? `id: ${change.checkpoint}\n` : "";
          stream.write(`${id}event: ${change.type}\ndata: ${JSON.stringify(change)}\n\n`);
        }
      }
    }
    json(res, { updated: true });
    return true;
  }
  if (url.pathname === "/__e2e__/state") {
    const session = sessionFor(req, url);
    json(res, {
      requests: session.requests,
      abortRequests: session.abortRequests,
      visibleAgentIds: session.visibleAgentIds,
      blockedBriefIds: [...session.blockedBriefIds],
      runtimeId: session.runtimeId,
      eventLogEpoch: session.eventLogEpoch,
      visibilityScopeId: session.visibilityScopeId,
    });
    return true;
  }
  if (url.pathname === "/__e2e__/disconnect-streams" && req.method === "POST") {
    const session = sessionFor(req, url);
    // Raw global/agent event streams are retired (#3808); only the
    // conversation read-model stream remains for this control.
    for (const stream of session.conversationStreams) stream.end();
    session.conversationStreams.clear();
    json(res, { disconnected: true });
    return true;
  }
  if (url.pathname === "/__e2e__/reset-conversation" && req.method === "POST") {
    const session = sessionFor(req, url);
    const body = await requestBody(req);
    const reset = { type: "reset_required", reason: body.reason, hint: "fixture reset",
      oldest_retained_seq: 1, event_head_seq: 1000 };
    for (const stream of session.conversationStreams) {
      if (stream.conversationAgentId !== body.agentId) continue;
      stream.write(`event: reset_required\ndata: ${JSON.stringify(reset)}\n\n`);
    }
    json(res, { reset: true });
    return true;
  }
  if (url.pathname === "/__e2e__/conversation" && req.method === "POST") {
    const session = sessionFor(req, url);
    const body = await requestBody(req);
    const previous = session.conversationData.get(body.agentId);
    const data = { turns: [], activitiesByTurnId: {}, pending_inputs: [], ...body, head: (previous?.head ?? 0) + 1000 };
    session.conversationData.set(body.agentId, data);
    const scope = { event_log_epoch: session.eventLogEpoch, visibility_scope_id: session.visibilityScopeId };
    const batchId = `batch-${data.head}`;
    const changes = [
      { type: "batch_begin", batch_id: batchId, schema_version: 2, query_version: 2,
        runtime_id: session.runtimeId, ...scope, from_seq: previous?.head ?? 0, through_seq: data.head },
      ...data.turns.map((turn) => ({ type: "turn_summary_upsert", turn })),
      ...Object.entries(data.invalidateOnly ? {} : data.activitiesByTurnId).flatMap(([turn_id, activities]) =>
        activities.map((activity) => ({ type: "activity_upsert", turn_id, activity }))),
      ...data.turns.map((turn) => ({ type: "detail_invalidated", turn_id: turn.turn_id, detail_revision: data.head })),
      { type: "checkpoint", batch_id: batchId, ...scope, through_seq: data.head,
        checkpoint: conversationCursor(session, body.agentId, data.head) },
    ];
    for (const stream of session.conversationStreams) {
      if (stream.conversationAgentId !== body.agentId) continue;
      for (const change of changes) {
        const id = change.type === "checkpoint" ? `id: ${change.checkpoint}\n` : "";
        stream.write(`${id}event: ${change.type}\ndata: ${JSON.stringify(change)}\n\n`);
      }
    }
    json(res, { updated: true });
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
    if (body.toolExecutionsById) session.toolExecutionsById = new Map(Object.entries(body.toolExecutionsById));
    if (body.briefsById && typeof body.briefsById === "object") {
      session.briefsById = new Map(Object.entries(body.briefsById));
    }
    if (Array.isArray(body.blockedBriefIds)) {
      session.blockedBriefIds = new Set(body.blockedBriefIds);
    }
    if (Array.isArray(body.failConversationByAgentId)) {
      session.failConversationByAgentId = new Set(body.failConversationByAgentId);
    }
    if (body.abortResponse && typeof body.abortResponse === "object") {
      session.abortResponse = body.abortResponse;
    }
    if (typeof body.runtimeId === "string") session.runtimeId = body.runtimeId;
    if (typeof body.eventLogEpoch === "string") session.eventLogEpoch = body.eventLogEpoch;
    if (typeof body.visibilityScopeId === "string") {
      session.visibilityScopeId = body.visibilityScopeId;
    }
    if (body.oldestRetainedSeqByAgentId && typeof body.oldestRetainedSeqByAgentId === "object") {
      session.oldestRetainedSeqByAgentId = new Map(
        Object.entries(body.oldestRetainedSeqByAgentId),
      );
    }
    if (body.snapshotThroughSeqByAgentId && typeof body.snapshotThroughSeqByAgentId === "object") {
      session.snapshotThroughSeqByAgentId = new Map(
        Object.entries(body.snapshotThroughSeqByAgentId),
      );
    }
    if (body.projectionLatestBriefByAgentId && typeof body.projectionLatestBriefByAgentId === "object") {
      session.projectionLatestBriefByAgentId = new Map(
        Object.entries(body.projectionLatestBriefByAgentId),
      );
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
    if (envelope.type === "message_processing_started" && typeof envelope.payload?.run_id === "string") {
      session.currentRunIdByAgentId.set(envelope.agent_id, envelope.payload.run_id);
    }
    if (
      envelope.type === "turn_terminal"
      || envelope.type === "turn_terminal_aborted"
      || envelope.type === "message_processing_aborted"
      || envelope.type === "runtime_error"
    ) {
      session.currentRunIdByAgentId.delete(envelope.agent_id);
    }
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
    const { visibleAgentIds } = session;
    if (!visibleAgentIds.includes("e2e-agent")) visibleAgentIds.push("e2e-agent");
    const envelope = {
      id: "e2e-event-1",
      event_seq: 1,
      event_log_epoch: "e2e-epoch",
      ts: "2026-08-25T00:00:00Z",
      agent_id: "e2e-agent",
      type: "agent_state_changed",
      payload_schema: "holon.runtime_event.legacy",
      payload: {},
    };
    const events = session.eventsByAgentId.get("e2e-agent") ?? [];
    events.push(envelope);
    session.eventsByAgentId.set("e2e-agent", events);
    // Discovery is roster-snapshot polling now (#3808): the appended
    // canonical event only seeds server-side state for later reads.
    json(res, { emitted: true });
    return true;
  }
  return false;
}

async function handleApi(req, res, url) {
  record(req, url);
  const session = sessionFor(req, url);
  if (tour) {
    const body = tourApi(url.pathname);
    if (body !== undefined) {
      json(res, body);
      return true;
    }
  }
  if (url.pathname === "/api/auth/session/me") {
    json(res, {
      user_id: `e2e-${sessionId(req, url)}`,
      auth_method: "none",
    });
    return true;
  }
  if (url.pathname === "/api/handshake") {
    json(res, {
      ok: true,
      protocol: { name: "holon-control", version: 1 },
      auth: { mode: "none", required: false },
      runtime: {
        default_agent: "bootstrap-agent",
        workspace_dir: "/tmp/holon-e2e",
        home_dir: "/tmp/holon-e2e",
        listen: "127.0.0.1:0",
        advertise_url: `http://127.0.0.1:${requestedPort}`,
      },
      capabilities: [
        "agents.roster-snapshot.v1",
        "agents.projection-snapshot.v1",
        "events.projection-effect.v1",
        "briefs.atomic-created-event.v1",
        "agents.conversation-read.v1",
      ],
    });
    return true;
  }
  if (url.pathname === "/api/agents/list") {
    json(res, session.visibleAgentIds.map((agentId) => listEntry(agentId, session)));
    return true;
  }
  if (url.pathname === "/api/agents/snapshot") {
    json(res, rosterSnapshot(session));
    return true;
  }
  const identityMatch = url.pathname.match(/^\/api\/control\/agents\/([^/]+)\/detail$/);
  if (identityMatch) {
    const agentId = decodeURIComponent(identityMatch[1]);
    if (!session.visibleAgentIds.includes(agentId)) {
      json(res, { error: "agent_not_found" }, 404);
      return true;
    }
    json(res, {
      identity: { ...listEntry(agentId, session).identity, status: "active" },
      display_name: agentId,
      canonical_relations: {},
    });
    return true;
  }
  const tasksMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/tasks$/);
  if (tasksMatch) {
    const agentId = decodeURIComponent(tasksMatch[1]);
    const fixture = session.rightPanelByAgentId.get(agentId);
    const terminal = url.searchParams.get("status") === "terminal";
    const resource = terminal ? "recent-tasks" : "tasks";
    const failure = resourceFailure(fixture, resource);
    agentResource(req, res, session, agentId,
      failure ? { error: `right-panel ${resource} unavailable` } : (terminal ? fixture?.recentTasks : fixture?.tasks) ?? [],
      failure ?? 200);
    return true;
  }
  const timersMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/timers$/);
  if (timersMatch) {
    const agentId = decodeURIComponent(timersMatch[1]);
    const fixture = session.rightPanelByAgentId.get(agentId);
    const failure = resourceFailure(fixture, "timers");
    agentResource(req, res, session, agentId,
      failure ? { error: "right-panel timers unavailable" } : fixture?.timers ?? [], failure ?? 200);
    return true;
  }
  const skillsMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/skills$/);
  if (skillsMatch) {
    const fixture = session.rightPanelByAgentId.get(decodeURIComponent(skillsMatch[1]));
    const failure = resourceFailure(fixture, "skills");
    json(res, failure ? { error: "right-panel skills unavailable" } : { skills: fixture?.skills ?? [] }, failure ?? 200);
    return true;
  }
  if (url.pathname === "/api/skills/catalog") {
    json(res, { catalog: [] });
    return true;
  }
  const filesMatch = url.pathname.match(/^\/api\/workspaces\/([^/]+)\/files(?:\/(.*))?$/);
  if (filesMatch && req.method === "GET") {
    const workspaceId = decodeURIComponent(filesMatch[1]);
    const fixture = [...session.rightPanelByAgentId.values()]
      .find((entry) => entry.activeWorkspaceEntry?.workspace_id === workspaceId);
    const filePath = decodeURIComponent(filesMatch[2] ?? "");
    const record = fixture?.filesByPath?.[filePath];
    json(res, record ?? { error: "file not found" }, record ? 200 : 404);
    return true;
  }
  const stateMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/state$/);
  if (stateMatch) {
    const agentId = decodeURIComponent(stateMatch[1]);
    if (session.rightPanelByAgentId.get(agentId)?.failResources.includes("state")) {
      json(res, { error: "right-panel state unavailable" }, 503);
    } else {
      json(res, agentState(agentId, session));
    }
    return true;
  }
  const workItemsMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/work-items$/);
  if (workItemsMatch) {
    const agentId = decodeURIComponent(workItemsMatch[1]);
    const fixture = session.rightPanelByAgentId.get(agentId);
    const identity = listEntry(agentId, session).identity;
    const expectedIncarnation = req.headers["x-holon-expected-incarnation"];
    if (expectedIncarnation !== undefined && expectedIncarnation !== String(identity.incarnation)) {
      json(res, { error: "conversation_identity_changed" }, 409);
      return true;
    }
    if (fixture?.failResources.includes("work-items")) {
      json(res, { error: "right-panel work items unavailable" }, 503);
    } else {
      json(res, fixture?.workItems ?? [], 200, {
        "x-holon-agent-id": identity.agent_id,
        "x-holon-agent-incarnation": String(identity.incarnation),
      });
    }
    return true;
  }
  const workItemDetailMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/work-items\/([^/]+)$/);
  const taskDetailMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/tasks\/([^/]+)(\/output)?$/);
  if (workItemDetailMatch || taskDetailMatch) {
    const match = workItemDetailMatch ?? taskDetailMatch;
    const agentId = decodeURIComponent(match[1]);
    const id = decodeURIComponent(match[2]);
    const fixture = session.rightPanelByAgentId.get(agentId);
    const resource = workItemDetailMatch ? "work-item-detail" : match[3] ? "task-output" : "task-status";
    const records = workItemDetailMatch ? fixture?.workItemDetailsById
      : match[3] ? fixture?.taskOutputsById : fixture?.taskDetailsById;
    const failed = fixture?.failResources.includes(resource);
    const record = records?.[id];
    agentResource(req, res, session, agentId,
      failed ? { error: `right-panel ${resource} unavailable` } : record ?? { error: `${resource} not found` },
      failed ? 503 : record ? 200 : 404);
    return true;
  }
  if (url.pathname === "/api/agents/brief-read-states" && req.method === "GET") {
    json(res, session.visibleAgentIds.map((agentId) => briefReadState(session, agentId)));
    return true;
  }
  const briefReadCursorMatch = url.pathname.match(
    /^\/api\/agents\/([^/]+)\/brief-read-cursor$/,
  );
  if (briefReadCursorMatch && req.method === "POST") {
    const agentId = decodeURIComponent(briefReadCursorMatch[1]);
    const body = await requestBody(req);
    const candidate = Number(body.read_through_event_seq);
    if (!Number.isFinite(candidate) || candidate < 0) {
      json(res, { error: "read_through_event_seq must be a non-negative number" }, 400);
      return true;
    }
    const key = [
      session.runtimeId,
      session.visibilityScopeId,
      session.eventLogEpoch,
      agentId,
    ].join("\u0000");
    const current = session.briefReadCursors.get(key) ?? 0;
    session.briefReadCursors.set(key, Math.max(current, candidate));
    json(res, { state: briefReadState(session, agentId) });
    return true;
  }
  const abortMatch = url.pathname.match(
    /^\/api\/control\/agents\/([^/]+)\/current-run\/abort$/,
  );
  if (abortMatch && req.method === "POST") {
    const body = await requestBody(req);
    session.abortRequests.push({
      agentId: decodeURIComponent(abortMatch[1]),
      body,
    });
    const response = session.abortResponse ?? {
      status: 200,
      body: { ok: true, aborted: true },
    };
    if (!session.abortResponse && session.currentRunIdByAgentId.get(abortMatch[1]) === body.run_id) {
      // Turn-scoped abort: the run ends and the agent stays schedulable, so
      // the next roster/state projection no longer reports a current run.
      session.currentRunIdByAgentId.delete(abortMatch[1]);
    }
    json(res, response.body ?? {}, response.status ?? 200);
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
    json(
      res,
      eventPage(session, decodeURIComponent(eventsMatch[1]), url),
      200,
      { [EVENT_CONTRACT_VERSION_HEADER]: EVENT_CONTRACT_VERSION },
    );
    return true;
  }
  const conversationMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/conversation$/);
  if (conversationMatch) {
    const agentId = decodeURIComponent(conversationMatch[1]);
    if (session.failConversationByAgentId.has(agentId)) {
      json(res, { ok: false, error: "conversation unavailable" }, 503);
      return true;
    }
    const data = session.conversationData.get(agentId);
    const head = data?.head ?? eventHead(session, agentId);
    json(res, {
      schema_version: 2,
      query_version: 2,
      runtime_id: session.runtimeId,
      event_log_epoch: session.eventLogEpoch,
      visibility_scope_id: session.visibilityScopeId,
      snapshot_through_seq: head,
      event_head_seq: head,
      oldest_retained_seq: session.oldestRetainedSeqByAgentId.get(agentId) ?? 0,
      snapshot_cursor: conversationCursor(session, agentId, head),
      turns: data?.turns ?? conversationTurns(session, agentId),
      active_turns: data?.turns.filter((turn) => turn.execution.kind === "active") ?? [],
      pending_inputs: data?.pending_inputs ?? [],
      next_before_cursor: null,
      has_more: false,
    });
    return true;
  }
  const activityMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/turns\/([^/]+)\/activities$/);
  if (activityMatch) {
    const data = session.conversationData.get(decodeURIComponent(activityMatch[1]));
    const turnId = decodeURIComponent(activityMatch[2]);
    const turn = data?.turns.find((turn) => turn.turn_id === turnId);
    if (!turn) { json(res, { error: "turn missing" }, 404); return true; }
    json(res, {
      schema_version: 2, query_version: 2, runtime_id: session.runtimeId,
      event_log_epoch: session.eventLogEpoch, visibility_scope_id: session.visibilityScopeId,
      snapshot_through_seq: data.head, event_head_seq: data.head, oldest_retained_seq: 0,
      snapshot_cursor: conversationCursor(session, decodeURIComponent(activityMatch[1]), data.head),
      turn, detail_revision: data.head,
      activities: data.activitiesByTurnId[turnId] ?? [], coverage: { kind: "complete" },
      next_before_cursor: null, has_more: false,
    });
    return true;
  }
  const toolMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/tool-executions\/([^/]+)$/);
  if (toolMatch) {
    const agentId = decodeURIComponent(toolMatch[1]);
    const id = decodeURIComponent(toolMatch[2]);
    const fixture = session.rightPanelByAgentId.get(agentId);
    const tool = fixture?.toolExecutionsById?.[id] ?? session.toolExecutionsById.get(id);
    const failed = fixture?.failResources.includes("tool-execution");
    agentResource(req, res, session, agentId,
      failed ? { error: "right-panel tool execution unavailable" } : tool ?? { error: "tool not found" },
      failed ? 503 : tool ? 200 : 404);
    return true;
  }
  const briefMatch = url.pathname.match(/^\/api\/agents\/([^/]+)\/briefs\/([^/]+)$/);
  if (briefMatch && req.method === "GET") {
    const briefId = decodeURIComponent(briefMatch[2]);
    if (session.blockedBriefIds.has(briefId)) {
      return true;
    }
    const brief = session.briefsById.get(briefId);
    if (!brief) {
      json(res, { error: `brief not found: ${briefId}` }, 404);
      return true;
    }
    json(res, {
      attachments: null,
      related_message_id: null,
      related_task_id: null,
      ...brief,
    });
    return true;
  }
  if (/^\/api\/agents\/[^/]+\/conversation\/stream$/.test(url.pathname)) {
    const streamAgent = decodeURIComponent(url.pathname.split("/")[3] ?? "");
    if (session.failConversationByAgentId.has(streamAgent)) {
      json(res, { ok: false, error: "conversation stream unavailable" }, 503);
      return true;
    }
    const after = url.searchParams.get("after") ?? req.headers["last-event-id"];
    if (after) {
      let cursor;
      try { cursor = JSON.parse(Buffer.from(after, "base64url").toString()); } catch { cursor = null; }
      if (!Array.isArray(cursor) || cursor[0] !== session.runtimeId
          || cursor[1] !== session.visibilityScopeId || cursor[2] !== session.eventLogEpoch
          || cursor[3] !== streamAgent) {
        json(res, { ok: false, code: "conversation_reset_required", reason: "event_log_epoch_mismatch",
          error: "conversation cursor scope changed" }, 409);
        return true;
      }
    }
    // The control endpoint publishes atomic conversation batches to this stream.
    res.conversationAgentId = streamAgent;
    openEventStream(req, res, session.conversationStreams);
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
  if (production) {
    const file = new URL(`.${decodeURIComponent(url.pathname)}`, productionRoot);
    if (!file.href.startsWith(productionRoot.href)) {
      res.writeHead(403).end();
      return;
    }
    const extension = path.extname(file.pathname);
    const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css",
      ".svg": "image/svg+xml", ".png": "image/png", ".woff2": "font/woff2" };
    try {
      const body = await readFile(extension ? file : new URL("index.html", productionRoot));
      res.writeHead(200, { "content-type": types[extension || ".html"] ?? "application/octet-stream" });
      res.end(body);
    } catch {
      res.writeHead(404).end("not found");
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
    for (const stream of session.conversationStreams) stream.end();
  }
  await vite?.close();
  await new Promise((resolve) => server.close(resolve));
}

process.once("SIGINT", () => void shutdown());
process.once("SIGTERM", () => void shutdown());
