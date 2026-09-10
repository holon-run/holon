import { describe, expect, it } from "vitest";

import { agentRenameErrorKey, isAgentRenameable, validateAgentDisplayName } from "./agent-rename";

describe("validateAgentDisplayName", () => {
  it("accepts normal names after trimming", () => {
    expect(validateAgentDisplayName("  Alpha One  ")).toBeUndefined();
    expect(validateAgentDisplayName("A")).toBeUndefined();
  });

  it("rejects empty and whitespace-only names", () => {
    expect(validateAgentDisplayName("")).toBe("required");
    expect(validateAgentDisplayName("   ")).toBe("required");
  });

  it("rejects names longer than 64 characters", () => {
    expect(validateAgentDisplayName("a".repeat(64))).toBeUndefined();
    expect(validateAgentDisplayName("a".repeat(65))).toBe("tooLong");
  });

  it("rejects path separators and control characters", () => {
    for (const value of ["a/b", "a\\b", "a:b", "a\nb", "a\u0000b", "a\u007fb", "a\u0085b"]) {
      expect(validateAgentDisplayName(value)).toBe("invalidChars");
    }
  });
});

describe("isAgentRenameable", () => {
  it("allows public self-owned named agents", () => {
    expect(isAgentRenameable({ visibility: "public", ownership: "self_owned", isDefaultAgent: false })).toBe(true);
  });

  it("blocks default agents and private children", () => {
    expect(isAgentRenameable({ visibility: "public", ownership: "self_owned", isDefaultAgent: true })).toBe(false);
    expect(isAgentRenameable({ visibility: "private", ownership: "parent_supervised", isDefaultAgent: false })).toBe(false);
  });

  it("blocks agents whose identity details have not loaded", () => {
    expect(isAgentRenameable({})).toBe(false);
  });
});

describe("agentRenameErrorKey", () => {
  it("maps conflict envelopes and ignores everything else", () => {
    expect(agentRenameErrorKey({ code: "agent_name_conflict" })).toBe("conflict");
    expect(agentRenameErrorKey({ code: "agent_name_invalid" })).toBeUndefined();
    expect(agentRenameErrorKey(new Error("boom"))).toBeUndefined();
    expect(agentRenameErrorKey(undefined)).toBeUndefined();
  });
});
