// Synthetic, deterministic website demo. No runtime state or credentials are read.
const definitions = [
  ["reviewer", "holon", "waiting", "Review event-ingress trust boundaries", ["code-review", "github-review"]],
  ["developer", "holon", "running", "Implement replay-safe event delivery", ["github-issue-solve", "ghx"]],
  ["release", "holon", "waiting", "Wait for release checks before tagging", ["ghx", "code-review"]],
  ["docs", "website", "idle", "Publish the runtime quickstart", ["docx", "ghx"]],
  ["growth", "website", "running", "Prepare the first product walkthrough", ["ghx", "sview"]],
  ["ops", "infrastructure", "idle", "Complete the daily runtime health check", ["ops", "holon-runtime-ops"]],
];
const timestamp = "2026-09-01T09:00:00Z";

export const tourAgents = definitions.map(([id, workspace, posture, objective, names]) => {
  const root = `/demo/${workspace}`;
  const ws = {
    workspace_id: `demo-${workspace}`, workspace_alias: workspace,
    workspace_anchor: root, execution_root: root, cwd: root,
    projection_kind: "workspace_root", is_active: true,
  };
  const work = {
    id: `demo-${id}-work`, agent_id: id, objective, revision: 1,
    state: posture === "idle" ? "completed" : "open",
    readiness: posture === "waiting" ? "blocked" : "ready",
    scheduling_state: posture === "waiting" ? "waiting_for_operator" : posture === "idle" ? "completed" : "runnable",
    plan_status: id === "reviewer" ? "needs_input" : "ready",
    blocked_by: posture === "waiting" ? (id === "reviewer"
      ? "Operator confirmation: approve the proposed trust-boundary review scope."
      : "Operator confirmation: authorize tagging after checks pass.") : null,
    created_at: timestamp, updated_at: timestamp,
    plan_artifact: {
      path: `work-items/demo-${id}-work/plan.md`, relative_path: `work-items/demo-${id}-work/plan.md`,
      workspace_id: `agent_home:${id}`, preview_complete: true,
      preview: `# ${objective}\n\n${id === "reviewer"
        ? "## Decision needed\nApprove this review scope before implementation proceeds.\n\n- Verify external events never inherit operator authority.\n- Check replay and duplicate-event handling.\n- Require regression tests for rejected trust elevation.\n\nNo merge or release is authorized by this review."
        : "Synthetic demonstration work. No production changes or external actions."}`,
    },
    todo_list: [
      { text: "Inspect the contract and affected code paths", state: "completed" },
      { text: id === "reviewer" ? "Confirm review scope with the operator" : objective, state: posture === "idle" ? "completed" : "in_progress" },
      { text: "Record verification evidence and handoff", state: posture === "idle" ? "completed" : "pending" },
    ],
  };
  const entry = {
    identity: { agent_id: id, visibility: "public", ownership: "self_owned", profile_preset: "public_named" },
    status: posture === "running" ? "running" : "awake_idle", pending: 0,
    scheduling_posture: { posture, reason: work.blocked_by ?? objective },
    active_workspace_entry: ws,
    model: { source: "runtime_default", effective_model: "demo/review-model" },
  };
  const skills = names.map((name) => ({
    skill_id: name, name, scope: "agent", path: `/demo/skills/${name}`,
    description: `Demo capability: ${name}`,
  }));
  return {
    id, entry, work, skills,
    state: {
      agent: {
        identity: { ...entry.identity, kind: "named", status: "active" },
        agent: { id, status: entry.status, pending: 0, current_work_item_id: work.id,
          attached_workspaces: [ws.workspace_id], current_run_id: null, turn_index: 4 },
        scheduling_posture: entry.scheduling_posture,
        active_task_count: 0, lifecycle: { accepts_external_messages: true },
        model: entry.model,
        closure: { outcome: posture === "waiting" ? "waiting" : "completed", runtime_posture: "awake",
          waiting_reason: work.blocked_by },
      },
      session: { current_run_id: null, pending_count: 0, last_turn: null },
      tasks: [], timers: [], work_items: [work], external_triggers: [],
      workspace: { workspaces: [ws] },
    },
  };
});

export function tourApi(path) {
  const match = path.match(/^\/api\/agents\/([^/]+)\/(skills|work-items)(?:\/([^/]+))?$/);
  if (!match) return undefined;
  const agent = tourAgents.find((item) => item.id === match[1]);
  if (!agent) return undefined;
  if (match[2] === "skills") return { skills: agent.skills };
  return match[3] ? agent.work : [agent.work];
}

export function seedTour(session) {
  for (const { id, work } of tourAgents) {
    const brief = {
      id: `demo-${id}-brief`, agent_id: id, workspace_id: `demo-${id}`,
      created_at: timestamp, created_event_seq: 1, kind: "result",
      content_source: { kind: "inline" },
      text: id === "reviewer"
        ? "## Review scope ready for confirmation\n\nI have mapped the event-ingress contract and prepared a focused review plan.\n\n### What I will check\n- **Authority:** external callbacks remain untrusted input.\n- **Replay safety:** duplicate events cannot repeat operator actions.\n- **Evidence:** rejected trust elevation has a regression test.\n\n### Your decision\nPlease confirm this scope before I continue. This does **not** authorize a merge or release.\n\nThe plan and checklist are saved in the current WorkItem. I am waiting for operator input.\n\n*Synthetic demo — no production repository or runtime is connected.*"
        : `## ${work.objective}\n\n${work.blocked_by ?? "Verification evidence recorded in the work item."}\n\n*Synthetic demonstration data.*`,
    };
    session.ledgerEnabledAgentIds.add(id);
    session.briefsById.set(brief.id, brief);
    session.eventsByAgentId.set(id, [{
      id: `demo-${id}-event`, event_seq: 1, event_log_epoch: session.eventLogEpoch,
      contract_version: 2, ts: timestamp, agent_id: id, type: "brief_created",
      payload_schema: "holon.runtime_event.brief_created", payload_schema_version: 1,
      payload: { brief_id: brief.id },
    }]);
  }
}
