// Synthetic coordinator scenario for the homepage Web GUI showcase (zh-CN).
export const timestamp = "2026-09-17T02:15:00Z";
export const focusAgentId = "holonbot";
export const objective = "统筹 v0.62 发布：代码、文档与宣发";

export const labels = {
  heading: "文档与宣发草稿已就绪，等待 PR #128 CI",
  ciTodo: "等待 Leo 回传 PR #128 CI 通过结论",
  source: "Maya · 技术文档 · 已完成安装与配对指南更新；Nora 已提交发布文章初稿",
  finalTodo: "汇总发布包并请你确认对外宣发",
};

const ws = {
  workspace_id: "holon-release",
  workspace_alias: "Holon v0.62 发布工作区",
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
    name: "Leo · 代码审阅",
    isDefault: false,
    waiting: true,
    readCursor: 2,
    goal: "跟进 PR #128：等待构建与回归测试 CI",
    skills: ["code-review", "github-review"],
    model: "review-model",
  },
  {
    id: "maya-docs",
    name: "Maya · 技术文档",
    isDefault: false,
    waiting: false,
    readCursor: 2,
    goal: "同步安装指南、移动端配对文档与 CLI 参考",
    skills: ["doc-coauthoring", "mdorigin"],
    model: "docs-model",
  },
  {
    id: "nora-growth",
    name: "Nora · 内容与增长",
    isDefault: false,
    waiting: false,
    readCursor: 0,
    goal: "起草 v0.62 发布博客与社媒宣发摘要",
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
      ? "等待 Leo 回传 PR #128 的 CI 结果；对外发布宣发内容前需你最终确认。"
      : def.waiting
        ? "等待 GitHub Actions 完成当前提交的回归检查。"
        : null,
    created_at: "2026-09-17T01:40:00Z",
    updated_at: timestamp,
    plan_artifact: {
      path: coordinating ? "work-items/launch-v062/plan.md" : `work-items/${def.id}/plan.md`,
      preview_complete: true,
      preview: coordinating
        ? "串联代码审阅、文档更新与发布内容制作，所有对外发布动作先汇总给你拍板。\n\n收集 Maya 与 Nora 的产出，等 Leo 确认 PR #128 CI 通过后生成最终核对包。"
        : def.goal,
    },
    todo_list: coordinating
      ? [
          { text: "安排 Maya 同步安装与移动端配对文档", state: "completed" },
          { text: "安排 Nora 起草发布博客与社媒更新亮点", state: "completed" },
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
      description: `演示技能：${skill}`,
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
          ? `## ${labels.heading}\n\nMaya 已同步完新版指南，Nora 的发布文章与社媒摘要草稿也已备好；目前仅剩 Leo 跟进的 PR #128 正在等待 CI。\n\n**已完成分工**\n- **Maya · 技术文档**：已核对并更新快速安装与手机扫码配对说明。\n- **Nora · 内容与增长**：已生成 v0.62 发布博客初稿与版本亮点。\n\n**下一步**\n收到 **Leo · 代码审阅** 的 PR #128 CI 通过回传后，汇总最终核对包，并在对外发布前请你拍板确认。`
          : `## ${work.objective}\n\n阶段结果已同步至 Holon Bot。`,
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
            preview: `${labels.source}\nLeo 正在等待 PR #128 的回归检查完成。`,
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
          summary: "汇总 Maya 的文档更新与 Nora 的宣发初稿；继续等待 Leo 回传 PR #128 的 CI 结果。",
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
