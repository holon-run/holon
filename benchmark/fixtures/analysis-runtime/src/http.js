export function normalizeRequest(body) {
  return {
    kind: body.kind ?? "webhook_event",
    priority: body.priority ?? "normal",
    metadata: body.metadata ?? null
  };
}

export function enqueueFromHttp(queue, body) {
  const normalized = normalizeRequest(body);
  queue.push(normalized);
  return normalized;
}
