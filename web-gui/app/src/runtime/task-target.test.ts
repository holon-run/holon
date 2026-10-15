import { describe, expect, it } from "vitest";

import { taskConversationPath } from "./task-target";

describe("taskConversationPath", () => {
  it("encodes the structured target and preserves its incarnation", () => {
    expect(taskConversationPath({ kind: "child_agent", agent_id: "child /#?", incarnation: 42 }))
      .toBe("/agents/child%20%2F%23%3F/conversation?incarnation=42");
    expect(taskConversationPath({ kind: "agent_message_wait", agent_id: "peer", incarnation: 1 }))
      .toBe("/agents/peer/conversation?incarnation=1");
  });

  it("does not associate identity-free legacy targets with a current instance", () => {
    expect(taskConversationPath()).toBeUndefined();
    expect(taskConversationPath(null)).toBeUndefined();
    expect(taskConversationPath({ kind: "child_agent", agent_id: "old" })).toBeUndefined();
    expect(taskConversationPath({ kind: "child_agent", agent_id: "", incarnation: 1 })).toBeUndefined();
    expect(taskConversationPath({ kind: "child_agent", agent_id: "old", incarnation: NaN })).toBeUndefined();
    expect(taskConversationPath({ kind: "child_agent", agent_id: "old", incarnation: -1 })).toBeUndefined();
  });

  it.each([0, -1, 1.5, Infinity, Number.MAX_SAFE_INTEGER + 1])(
    "does not link an invalid incarnation %s",
    (incarnation) => {
      expect(taskConversationPath({ kind: "child_agent", agent_id: "child", incarnation })).toBeUndefined();
      expect(taskConversationPath({ kind: "agent_message_wait", agent_id: "peer", incarnation })).toBeUndefined();
    },
  );
});
