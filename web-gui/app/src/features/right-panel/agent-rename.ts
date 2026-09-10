import type { AgentSummary } from "../../runtime/types";

/** Mirrors the backend `normalize_agent_name` rules (src/types.rs). */
export type AgentNameValidationError = "required" | "tooLong" | "invalidChars";

export const MAX_AGENT_NAME_LENGTH = 64;

/**
 * Validate a display name against the backend rules before sending it:
 * trimmed non-empty, at most 64 chars, no control characters or `/ \ :`.
 */
export function validateAgentDisplayName(input: string): AgentNameValidationError | undefined {
  const name = input.trim();
  if (!name) return "required";
  if (Array.from(name).length > MAX_AGENT_NAME_LENGTH) return "tooLong";
  for (const character of name) {
    const code = character.codePointAt(0) ?? 0;
    // Unicode Cc (control) category, matching Rust `char::is_control`.
    const isControl = code <= 0x1f || code === 0x7f || (code >= 0x80 && code <= 0x9f);
    if (isControl || character === "/" || character === "\\" || character === ":") {
      return "invalidChars";
    }
  }
  return undefined;
}

/**
 * Renaming is an operator surface for public self-owned named agents; the
 * backend stays authoritative and rejects anything this gating misses.
 */
export function isAgentRenameable(agent: Pick<AgentSummary, "canRename">): boolean {
  return agent.canRename === true;
}

/** Map a rename failure's error-envelope code to a stable i18n suffix. */
export function agentRenameErrorKey(error: unknown): "conflict" | undefined {
  return (error as { code?: unknown } | null | undefined)?.code === "agent_name_conflict"
    ? "conflict"
    : undefined;
}
