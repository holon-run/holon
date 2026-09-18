// Fixed illustration data for the real Holon GUI; no repository/CI execution.
export const timestamp = '2026-09-17T02:15:00Z';
export const objective = 'Follow PR #42: fix skipped pages';
const definitions = [
  ['reviewer', 'Code review', objective, ['code-review', 'github-review']],
  ['investigator', 'Investigation', 'Turn field evidence into an actionable issue', ['ops']],
  ['tester', 'Verification', 'Follow fixes through to verification', ['code-review']],
];
export const tourAgents = definitions.map(([id, name, goal, names]) => {
  const reviewing = id === 'reviewer';
  const ws = { workspace_id: 'demo-api', workspace_alias: 'Demo API project', workspace_anchor: '/demo/api', execution_root: '/demo/api', cwd: '/demo/api', projection_kind: 'workspace_root', is_active: true };
  const work = {
    id: `demo-${id}-work`, agent_id: id, objective: goal, revision: 3,
    state: reviewing ? 'open' : 'completed', readiness: reviewing ? 'blocked' : 'ready',
    scheduling_state: reviewing ? 'waiting_external' : 'completed',
    blocked_by: reviewing ? 'Waiting for CI on the current commit; resume when results arrive.' : null,
    created_at: '2026-09-17T02:00:00Z', updated_at: timestamp,
    plan_artifact: { path: `work-items/demo-${id}-work/plan.md`, preview_complete: true,
      preview: reviewing ? 'Verify the pagination fix without breaking existing clients.\n\nCheck the changes against earlier findings, verify CI for this commit, and summarize the review.' : 'A shared team role that receives work through events.' },
    todo_list: reviewing ? [
      { text: 'Check pagination and compatibility', state: 'completed' },
      { text: 'Review the fix and new tests', state: 'completed' },
      { text: 'Check CI for the current commit', state: 'pending' },
      { text: 'Summarize conclusions and evidence', state: 'pending' },
    ] : [],
  };
  const entry = {
    identity: { agent_id: id, name, visibility: 'public', ownership: 'self_owned', profile_preset: 'public_named' },
    status: 'awake_idle', pending: 0, scheduling_posture: { posture: reviewing ? 'waiting' : 'idle', reason: reviewing ? 'awaiting_external_change' : 'idle' },
    active_workspace_entry: ws, model: { source: 'runtime_default', effective_model: 'demo/review-model' },
  };
  return { id, entry, work, skills: names.map(name => ({ skill_id: name, name, scope: 'agent', path: `/demo/skills/${name}`, description: `Demo skill: ${name}` })),
    state: {
      agent: { identity: { ...entry.identity, kind: 'named', status: 'active' },
        agent: { id, status: entry.status, pending: 0, current_work_item_id: reviewing ? work.id : null, attached_workspaces: [ws.workspace_id], current_run_id: null, turn_index: 2 },
        scheduling_posture: entry.scheduling_posture, active_task_count: 0, lifecycle: { accepts_external_messages: true }, model: entry.model,
        closure: { outcome: reviewing ? 'waiting' : 'completed', runtime_posture: 'awake', waiting_reason: reviewing ? 'awaiting_external_change' : null } },
      session: { current_run_id: null, pending_count: 0, last_turn: null },
      tasks: [], timers: [], work_items: [work], external_triggers: [], workspace: { workspaces: [ws] },
    },
  };
});

export function tourApi(path) {
  const match = path.match(/^\/api\/agents\/([^/]+)\/(skills|work-items)(?:\/([^/]+))?$/);
  if (!match) return undefined;
  const agent = tourAgents.find(item => item.id === match[1]);
  if (!agent) return undefined;
  return match[2] === 'skills' ? { skills: agent.skills } : match[3] ? agent.work : [agent.work];
}

export function seedTour(session) {
  for (const { id, work } of tourAgents) {
    const brief = { id: `demo-${id}-brief`, agent_id: id, workspace_id: 'demo-api', created_at: timestamp, created_event_seq: 2, kind: 'result', content_source: { kind: 'inline' },
      text: id === 'reviewer' ? '## Changes reviewed; waiting for CI\n\nThe skipped-page issue is fixed. The latest commit adds a regression test for the final page.\n\n**Completed**\n- Checked the pagination change against earlier findings.\n- Confirmed tests cover the final page and empty lists.\n\n**Next step**\nResume when CI results arrive. Summarize the review if checks pass; investigate if they fail.\n\nFindings, check progress, and waiting conditions remain in the same work item.' : `## ${work.objective}\n\nNo work is pending.` };
    session.ledgerEnabledAgentIds.add(id);
    session.briefsById.set(brief.id, brief);
    session.eventsByAgentId.set(id, [{ id: `demo-${id}-event`, event_seq: 2, event_log_epoch: session.eventLogEpoch, contract_version: 2, ts: timestamp, agent_id: id, type: 'brief_created', payload_schema: 'holon.runtime_event.brief_created', payload_schema_version: 1, payload: { brief_id: brief.id } }]);
    if (id === 'reviewer') {
      const turn = { turn_id: 'review-new-commit', key: { turn_index: 2, turn_id: 'review-new-commit' }, revision: 1,
        presentation_class: 'external', inputs: [{ message_id: 'github-new-commit', presentation_class: 'external', preview: 'GitHub webhook · New commit on PR #42\nUpdated the pagination fix and added a final-page regression test.' }],
        started_at: '2026-09-17T02:12:00Z', ended_at: timestamp,
        execution: { kind: 'terminal', outcome: 'completed' }, result: { kind: 'available' }, settled: true, attention: null, detail_coverage: { kind: 'complete' }, brief_ids: [brief.id] };
      session.conversationData.set(id, { turns: [turn], activitiesByTurnId: { [turn.turn_id]: [] }, pending_inputs: [], head: 2 });
    }
  }
}
