import i18next from "i18next";
import type { AgentTimelineActivity, RuntimeMessageEnvelope } from "./types";

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
}
function decode(value: unknown): unknown {
  if (typeof value !== "string") return value;
  try { return JSON.parse(value); } catch { return value; }
}
function bodyValue(value: unknown): unknown {
  const body = record(decode(value));
  if (body?.type === "text" && typeof body.text === "string") return body.text;
  if (body?.type === "json") return body.value;
  return decode(value);
}
function displayText(value: unknown): string {
  return typeof value === "string" ? value : JSON.stringify(value, null, 2) ?? "";
}

/** MessageBody is standard; the producer's JSON payload is not one fixed schema. */
export function inputPresentation(body: unknown): { summary: string; text: string } {
  let value = bodyValue(body);
  const wake = typeof value === "string" && /^wake hint:\s*/i.test(value);
  if (wake && typeof value === "string") value = value.replace(/^wake hint:\s*/i, "");
  value = decode(value);
  const data = record(value);
  const text = displayText(value);
  if (data) {
    const task = record(data.task);
    const summary = [data.summary, data.summary_text, data.title, data.description, data.reason, data.message, task?.summary]
      .find((entry): entry is string => typeof entry === "string" && entry.trim().length > 0);
    const kind = [data.kind, data.type, task?.kind].find((entry): entry is string => typeof entry === "string");
    const status = [data.status, task?.status].find((entry): entry is string => typeof entry === "string");
    const fields = Object.entries(data).filter(([, value]) => value !== null && value !== undefined).map(([key, value]) => {
      const name = key.replace(/([a-z])([A-Z])/g, "$1 $2").replaceAll("_", " ");
      const label = name.charAt(0).toUpperCase() + name.slice(1);
      return `${label}: ${displayText(value)}`;
    }).join("\n\n");
    return { summary: summary ?? ([kind, status].filter(Boolean).join(" · ") || i18next.t("agentPage.structuredEvent")), text: fields };
  }
  // Truncated JSON cannot be decoded safely; preserve it only in the inspector.
  const structured = /^[\[{]/.test(text.trim());
  return { summary: structured ? i18next.t(wake ? "agentPage.wakeNotification" : "agentPage.structuredEvent")
    : text.split("\n")[0] || i18next.t("agentPage.structuredEvent"), text };
}

export function inputInspectorActivity(input: { message_id: string; preview: string; created_at?: string }, source?: string): AgentTimelineActivity {
  const presentation = inputPresentation(input.preview);
  return {
    id: `message:${input.message_id}`, messageId: input.message_id, kind: source === "operator" ? "operator" : "event",
    label: i18next.t(source === "operator" ? "agentPage.activityKind.operator" : source ? `agentPage.turnSource.${source}` : "agentPage.activityKind.input"), body: presentation.summary,
    timestamp: input.created_at ?? "", meta: input.message_id, minDisplayLevel: "info", sourceIds: [input.message_id],
    detail: { label: i18next.t("agentPage.eventPreview"), text: presentation.text, tone: "data" },
  };
}

export function hydrateInputActivity(activity: AgentTimelineActivity, message: RuntimeMessageEnvelope): AgentTimelineActivity {
  const wake = record(record(message.metadata)?.wake_hint);
  // Wake hint text is itself truncated by the runtime. Its metadata carries the original body.
  const value = wake?.body ?? message.body;
  const presentation = inputPresentation(value);
  const origin = record(message.origin);
  const provenance = [origin?.kind, origin?.subsystem].filter((value) => typeof value === "string").join(" · ");
  return { ...activity, body: presentation.summary, meta: provenance || activity.meta,
    timestamp: typeof message.created_at === "string" ? message.created_at : activity.timestamp,
    detail: { label: i18next.t("inspector.fullDetail"), text: presentation.text, tone: "data" }, rawEvent: message,
  };
}
