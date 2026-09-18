// Fixed illustration data for the real Holon GUI; no repository/CI execution.
export const timestamp = '2026-09-17T02:15:00Z';
export const objective = '跟进 PR #42：修复分页遗漏';
const definitions = [
  ['reviewer', '代码审阅', objective, ['code-review', 'github-review']],
  ['investigator', '问题调查', '整理现场线索，形成可接手的问题', ['ops']],
  ['tester', '测试验收', '跟进修复版本与验收结果', ['code-review']],
];
export const tourAgents = definitions.map(([id, name, goal, names]) => {
  const reviewing = id === 'reviewer';
  const ws = { workspace_id: 'demo-api', workspace_alias: '示例 API 项目', workspace_anchor: '/demo/api', execution_root: '/demo/api', cwd: '/demo/api', projection_kind: 'workspace_root', is_active: true };
  const work = {
    id: `demo-${id}-work`, agent_id: id, objective: goal, revision: 3,
    state: reviewing ? 'open' : 'completed', readiness: reviewing ? 'blocked' : 'ready',
    scheduling_state: reviewing ? 'waiting_external' : 'completed',
    blocked_by: reviewing ? '等待当前提交的 CI 结果；收到事件后继续核对。' : null,
    created_at: '2026-09-17T02:00:00Z', updated_at: timestamp,
    plan_artifact: { path: `work-items/demo-${id}-work/plan.md`, preview_complete: true,
      preview: reviewing ? '确认分页修复有效，且不影响现有调用。\n\n对照原问题复核修改，核对当前版本的测试结果，再汇总审阅结论。' : '团队共享角色，按事件接收工作。' },
    todo_list: reviewing ? [
      { text: '检查分页逻辑与兼容性', state: 'completed' },
      { text: '复核修改与新增测试', state: 'completed' },
      { text: '核对当前提交的 CI 结果', state: 'pending' },
      { text: '汇总审阅结论与验证依据', state: 'pending' },
    ] : [],
  };
  const entry = {
    identity: { agent_id: id, name, visibility: 'public', ownership: 'self_owned', profile_preset: 'public_named' },
    status: 'awake_idle', pending: 0, scheduling_posture: { posture: reviewing ? 'waiting' : 'idle', reason: reviewing ? 'awaiting_external_change' : 'idle' },
    active_workspace_entry: ws, model: { source: 'runtime_default', effective_model: 'demo/review-model' },
  };
  return { id, entry, work, skills: names.map(name => ({ skill_id: name, name, scope: 'agent', path: `/demo/skills/${name}`, description: `演示技能：${name}` })),
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
      text: id === 'reviewer' ? '## 已复核新提交，等待 CI\n\n此前发现的分页遗漏已修复，新提交补充了末页回归测试。\n\n**已完成**\n- 对照原问题检查分页修改。\n- 核对新增测试覆盖末页与空列表。\n\n**下一步**\n收到当前提交的 CI 结果后继续核对；通过后汇总审阅结论，失败则调查原因。\n\n问题、检查进度与等待条件已保存在同一个工作项中。' : `## ${work.objective}\n\n当前没有待处理工作。` };
    session.ledgerEnabledAgentIds.add(id);
    session.briefsById.set(brief.id, brief);
    session.eventsByAgentId.set(id, [{ id: `demo-${id}-event`, event_seq: 2, event_log_epoch: session.eventLogEpoch, contract_version: 2, ts: timestamp, agent_id: id, type: 'brief_created', payload_schema: 'holon.runtime_event.brief_created', payload_schema_version: 1, payload: { brief_id: brief.id } }]);
    if (id === 'reviewer') {
      const turn = { turn_id: 'review-new-commit', key: { turn_index: 2, turn_id: 'review-new-commit' }, revision: 1,
        presentation_class: 'external', inputs: [{ message_id: 'github-new-commit', presentation_class: 'external', preview: 'GitHub webhook · PR #42 有新提交\n已更新分页修复，并补充末页回归测试。' }],
        started_at: '2026-09-17T02:12:00Z', ended_at: timestamp,
        execution: { kind: 'terminal', outcome: 'completed' }, result: { kind: 'available' }, settled: true, attention: null, detail_coverage: { kind: 'complete' }, brief_ids: [brief.id] };
      session.conversationData.set(id, { turns: [turn], activitiesByTurnId: { [turn.turn_id]: [] }, pending_inputs: [], head: 2 });
    }
  }
}
