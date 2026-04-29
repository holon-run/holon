export function parseEnvelope(raw) {
  const parsed = typeof raw === "string" ? JSON.parse(raw) : raw;

  return {
    kind: parsed.kind ?? "webhook_event",
    priority: parsed.priority ?? "normal",
    metadata: parsed.metadata ?? null
  };
}
