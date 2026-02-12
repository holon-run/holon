import type { SDKSession } from "@anthropic-ai/claude-agent-sdk";

export function tryGetSessionId(
  session: SDKSession,
  onUnavailable?: (message: string) => void
): string | undefined {
  try {
    const value = session.sessionId;
    if (typeof value === "string" && value.trim() !== "") {
      return value.trim();
    }
  } catch (error) {
    const details = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
    onUnavailable?.(`sessionId is not available yet (${details})`);
  }
  return undefined;
}
