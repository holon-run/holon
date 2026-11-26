// Synthetic coordinator scenario for the homepage Web GUI showcase (en).
export const timestamp = "2026-09-17T02:15:00Z";
export const focusAgentId = "holonbot";
export const objective = "Coordinate v0.62 launch: code, docs & content";

export const labels = {
  heading: "Docs and launch draft ready; waiting on PR #128 CI",
  ciTodo: "Wait for Leo to confirm PR #128 CI passes",
  source: "Maya · Docs · Updated setup & pairing guides; Nora shared the launch post draft",
  finalTodo: "Assemble release packet & ask for publish approval",
};

const ws = {
  workspace_id: "holon-release",
  workspace_alias: "Holon v0.62 release workspace",
  workspace_anchor: "/workspace/holon",
  execution_root: "/workspace/holon",
  cwd: "/workspace/holon",
  projection_kind: "workspace_root",
  is_active: true,
};

const definitions = [
  {
    id: "holonbot",
    name: "Holon Bot",
    isDefault: true,
    waiting: true,
    readCursor: 2,
    goal: objective,
    skills: ["release-coordinator", "github-review"],
    model: "coordinator-model",
  },
  {
    id: "leo-reviewer",
    name: "Leo · Code Review",
    isDefault: false,
    waiting: true,
    readCursor: 2,
    goal: "Follow PR #128: waiting on release CI checks",
    skills: ["code-review", "github-review"],
    model: "review-model",
  },
  {
    id: "maya-docs",
    name: "Maya · Docs",
    isDefault: false,
    waiting: false,
    readCursor: 2,
    goal: "Update setup guide, mobile pairing docs & CLI reference",
    skills: ["doc-coauthoring", "mdorigin"],
    model: "docs-model",
  },
  {
    id: "nora-growth",
    name: "Nora · Growth",
    isDefault: false,
    waiting: false,
    readCursor: 0,
    goal: "Draft v0.62 launch blog post & social summary",
    skills: ["content-creator", "brand-guidelines"],
    model: "content-model",
  },
];

export const tourAgents = definitions.map((def) => {
  const coordinating = def.id === "holonbot";
  const work = {
    id: coordinating ? "launch-v062" : `demo-${def.id}-work`,
    agent_id: def.id,
    objective: def.goal,
    revision: 4,
    state: def.waiting ? "open" : "completed",
    readiness: def.waiting ? "blocked" : "ready",
    scheduling_state: def.waiting ? "waiting_external" : "completed",
    blocked_by: coordinating
      ? "Waiting for Leo's CI result on PR #128; ask for your approval before publishing."
      : def.waiting
        ? "Waiting for GitHub Actions to finish checks on the latest commit."
        : null,
    created_at: "2026-09-17T01:40:00Z",
    updated_at: timestamp,
    plan_artifact: {
      path: coordinating ? "work-items/launch-v062/plan.md" : `work-items/${def.id}/plan.md`,
      preview_complete: true,
      preview: coordinating
        ? "Coordinate code review, docs updates, and launch content; hold external publishing for your sign-off.\n\nCollect Maya and Nora's deliverables, then assemble the release packet once Leo confirms CI."
        : def.goal,
    },
    todo_list: coordinating
      ? [
          { text: "Maya: update setup & mobile pairing docs", state: "completed" },
          { text: "Nora: draft launch post & release highlights", state: "completed" },
          { text: labels.ciTodo, state: "pending" },
          { text: labels.finalTodo, state: "pending" },
        ]
      : [],
  };

  const entry = {
    identity: {
      agent_id: def.id,
      incarnation: 1,
      name: def.name,
      visibility: "public",
      ownership: "self_owned",
      profile_preset: "public_named",
      is_default_agent: def.isDefault,
    },
    status: "awake_idle",
    pending: 0,
    waiting_reason: def.waiting ? "awaiting_external_change" : undefined,
    scheduling_posture: {
      posture: def.waiting ? "waiting" : "idle",
      reason: def.waiting ? "awaiting_external_change" : "idle",
    },
    active_workspace_entry: ws,
    model: { source: "runtime_default", effective_model: def.model },
  };

  return {
    id: def.id,
    readCursor: def.readCursor,
    entry,
    work,
    skills: def.skills.map((skill) => ({
      skill_id: skill,
      name: skill,
      scope: "agent",
      path: `/demo/skills/${skill}`,
      description: `Demo skill: ${skill}`,
    })),
    state: {
      agent: {
        identity: { ...entry.identity, kind: "named", status: "active" },
        agent: {
          id: def.id,
          status: entry.status,
          pending: 0,
          current_work_item_id: def.waiting ? work.id : null,
          attached_workspaces: [ws.workspace_id],
          current_run_id: null,
          turn_index: 3,
        },
        scheduling_posture: entry.scheduling_posture,
        active_task_count: 0,
        lifecycle: { accepts_external_messages: true },
        model: entry.model,
        closure: {
          outcome: def.waiting ? "waiting" : "completed",
          runtime_posture: "awake",
          waiting_reason: def.waiting ? "awaiting_external_change" : null,
        },
      },
      session: { current_run_id: null, pending_count: 0, last_turn: null },
      tasks: [],
      timers: [],
      work_items: [work],
      external_triggers: [],
      workspace: { workspaces: [ws] },
    },
  };
});

export function tourApi(path) {
  const match = path.match(/^\/api\/agents\/([^/]+)\/(skills|work-items)(?:\/([^/]+))?$/);
  if (!match) return undefined;
  const agent = tourAgents.find((item) => item.id === match[1]);
  if (!agent) return undefined;
  return match[2] === "skills" ? { skills: agent.skills } : match[3] ? agent.work : [agent.work];
}

export function seedTour(session) {
  for (const { id, readCursor, work } of tourAgents) {
    const brief = {
      id: `demo-${id}-brief`,
      agent_id: id,
      workspace_id: ws.workspace_id,
      created_at: timestamp,
      created_event_seq: 2,
      kind: "result",
      content_source: { kind: "inline" },
      text:
        id === "holonbot"
          ? `## ${labels.heading}\n\nMaya finished updating the guides and Nora drafted the launch post and social copy. Only PR #128 with Leo is still waiting on CI.\n\n**Completed across roles**\n- **Maya · Docs**: Updated quickstart and mobile QR pairing guides.\n- **Nora · Growth**: Prepared the v0.62 launch blog draft and release highlights.\n\n**Next step**\nOnce **Leo · Code Review** confirms PR #128 CI passes, assemble the final release packet and ask for your approval before publishing.`
          : `## ${work.objective}\n\nDeliverables synced to Holon Bot.`,
    };
    session.ledgerEnabledAgentIds.add(id);
    session.briefsById.set(brief.id, brief);
    session.eventsByAgentId.set(id, [
      {
        id: `demo-${id}-event`,
        event_seq: 2,
        event_log_epoch: session.eventLogEpoch,
        contract_version: 2,
        ts: timestamp,
        agent_id: id,
        type: "brief_created",
        payload_schema: "holon.runtime_event.brief_created",
        payload_schema_version: 1,
        payload: { brief_id: brief.id },
      },
    ]);
    if (readCursor > 0) {
      const cursorKey = [
        session.runtimeId,
        session.visibilityScopeId,
        session.eventLogEpoch,
        id,
      ].join("\u0000");
      session.briefReadCursors.set(cursorKey, readCursor);
    }
    if (id === "holonbot") {
      const turn = {
        turn_id: "coord-sync-turn",
        key: { turn_index: 3, turn_id: "coord-sync-turn" },
        revision: 1,
        presentation_class: "external",
        inputs: [
          {
            message_id: "team-role-handoff",
            presentation_class: "external",
            preview: `${labels.source}\nLeo is waiting on GitHub Actions CI for PR #128.`,
          },
        ],
        started_at: "2026-09-17T02:12:00Z",
        ended_at: timestamp,
        execution: { kind: "terminal", outcome: "completed" },
        result: { kind: "available" },
        settled: true,
        attention: null,
        detail_coverage: { kind: "complete" },
        brief_ids: [brief.id],
      };
      const activities = [
        {
          kind: "assistant",
          id: "assistant:3",
          key: { event_seq: 3, activity_id: "assistant:3" },
          revision: 1,
          summary: "Synced Maya's doc updates and Nora's launch draft; waiting for Leo's CI result on PR #128.",
        },
      ];
      session.conversationData.set(id, {
        turns: [turn],
        activitiesByTurnId: { [turn.turn_id]: activities },
        pending_inputs: [],
        head: 2,
      });
    }
  }
}
