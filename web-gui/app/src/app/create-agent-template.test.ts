import { describe, expect, it } from "vitest";

import {
  BLANK_AGENT_TEMPLATE,
  canSubmitCreateAgent,
  parseCreateAgentTemplate,
  resolveCreateAgentTemplate,
} from "./create-agent-template";

const ready = { agentId: "new-agent", busy: false, uncertain: false };

describe("create agent template selection", () => {
  it("treats an explicit blank selection as the blank agent path", () => {
    expect(parseCreateAgentTemplate(BLANK_AGENT_TEMPLATE)).toEqual({ kind: "blank" });
    expect(resolveCreateAgentTemplate(BLANK_AGENT_TEMPLATE)).toBeUndefined();
  });

  it("keeps an empty selection distinct from the blank option", () => {
    expect(parseCreateAgentTemplate("")).toEqual({ kind: "unselected" });
    expect(parseCreateAgentTemplate("   ")).toEqual({ kind: "unselected" });
    expect(resolveCreateAgentTemplate("")).toBeUndefined();
  });

  it("sends the trimmed template for a template selection", () => {
    expect(parseCreateAgentTemplate(" default ")).toEqual({ kind: "template", template: "default" });
    expect(resolveCreateAgentTemplate(" default ")).toBe("default");
  });
});

describe("canSubmitCreateAgent", () => {
  it("allows submitting a blank agent without a template catalog", () => {
    expect(canSubmitCreateAgent({ ...ready, templateSelection: BLANK_AGENT_TEMPLATE })).toBe(true);
  });

  it("allows submitting a chosen template", () => {
    expect(canSubmitCreateAgent({ ...ready, templateSelection: "default" })).toBe(true);
  });

  it("blocks submission until a selection is made", () => {
    expect(canSubmitCreateAgent({ ...ready, templateSelection: "" })).toBe(false);
  });

  it("blocks submission without an agent id or while busy or uncertain", () => {
    expect(canSubmitCreateAgent({ ...ready, agentId: "   ", templateSelection: BLANK_AGENT_TEMPLATE })).toBe(false);
    expect(canSubmitCreateAgent({ ...ready, busy: true, templateSelection: BLANK_AGENT_TEMPLATE })).toBe(false);
    expect(canSubmitCreateAgent({ ...ready, uncertain: true, templateSelection: BLANK_AGENT_TEMPLATE })).toBe(false);
  });
});
