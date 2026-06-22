import { describe, expect, it } from "vitest";

import { formatSearchPreview, searchOptionsForSelection } from "./SearchPage";
import type { AgentSummary } from "../../runtime/types";

function agent(id: string): AgentSummary {
  return {
    id,
    badge: id.slice(0, 1),
    profile: "default",
    lifecycle: "idle",
    focusSummary: "idle",
    workspace: "workspace",
    attention: "none",
    model: "model",
    footer: "footer",
    subtitle: "subtitle",
    lastBrief: "",
    lastTurnTime: "",
    pending: 0,
    activeTaskCount: 0,
    waitingCount: 0,
    posture: "idle",
    postureReason: "idle",
  };
}

describe("searchOptionsForSelection", () => {
  it("searches all visible agents and all workspaces for the All agents option", () => {
    expect(searchOptionsForSelection("all", [agent("holon-pm"), agent("worker")], "25")).toEqual({
      agentIds: ["holon-pm", "worker"],
      includeAllWorkspaces: true,
      limit: 25,
    });
  });

  it("keeps a selected agent scoped to that agent", () => {
    expect(searchOptionsForSelection("worker", [agent("holon-pm"), agent("worker")], "bad")).toEqual({
      agentIds: ["worker"],
      includeAllWorkspaces: false,
      limit: 20,
    });
  });
});

describe("formatSearchPreview", () => {
  it("summarizes indexed message documents from their body section", () => {
    expect(formatSearchPreview([
      "message_ref: message:msg_0577e13c42a52bc",
      "message_id: msg_0577e13c42a52bc",
      "turn_ref: turn:turn_4be859224d5f055",
      "message_seq: 691",
      "kind: OperatorPrompt",
      "body:",
      "你分析一下，当前索引的更新方式是如何的？",
    ].join("\n"))).toEqual({
      title: "Message body",
      summary: "你分析一下，当前索引的更新方式是如何的？",
      meta: [],
      isFormatted: true,
    });
  });

  it("summarizes compacted indexed message documents from inline body sections", () => {
    expect(formatSearchPreview(
      "message_ref: message:msg_0577e13c42a52bc message_id: msg_0577e13c42a52bc turn_ref: turn:turn_4be859224d5f055 message_seq: 691 kind: OperatorPrompt body: 你分析一下，当前索引的更新方式是如何的？",
    )).toEqual({
      title: "Message body",
      summary: "你分析一下，当前索引的更新方式是如何的？",
      meta: [],
      isFormatted: true,
    });
  });
});
