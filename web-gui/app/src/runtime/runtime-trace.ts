export type RuntimeTraceOutcome = "ok" | "error" | "cancelled" | "deduped" | "skipped";

export interface RuntimeTraceRecord {
  traceId: string;
  spanId: string;
  parentSpanId?: string;
  name: string;
  agentId?: string;
  trigger?: string;
  startedAt: string;
  durationMs: number;
  outcome: RuntimeTraceOutcome;
  attributes?: Record<string, string | number | boolean | undefined>;
}

export interface RuntimeTraceContext {
  traceId: string;
  rootName?: string;
  parentSpanId?: string;
  agentId?: string;
  trigger?: string;
}

const TRACE_BUFFER_LIMIT = 500;
const traceBuffer: RuntimeTraceRecord[] = [];
let fallbackId = 0;

function traceId(): string {
  if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
  fallbackId += 1;
  return `trace-${Date.now()}-${fallbackId}`;
}

export function createRuntimeTrace(
  name: string,
  fields: Pick<RuntimeTraceContext, "agentId" | "trigger"> = {},
): RuntimeTraceContext {
  return { traceId: traceId(), rootName: name, ...fields };
}

export function startRuntimeSpan(
  context: RuntimeTraceContext,
  name: string,
  attributes?: RuntimeTraceRecord["attributes"],
): {
  context: RuntimeTraceContext;
  end: (outcome?: RuntimeTraceOutcome, finalAttributes?: RuntimeTraceRecord["attributes"]) => void;
} {
  const spanId = traceId();
  const startedAt = new Date();
  const started = performance.now();
  let ended = false;
  return {
    context: {
      ...context,
      parentSpanId: spanId,
    },
    end: (outcome = "ok", finalAttributes) => {
      if (ended) return;
      ended = true;
      traceBuffer.push({
        traceId: context.traceId,
        spanId,
        parentSpanId: context.parentSpanId,
        name,
        agentId: context.agentId,
        trigger: context.trigger,
        startedAt: startedAt.toISOString(),
        durationMs: Math.max(0, performance.now() - started),
        outcome,
        attributes: { ...attributes, ...finalAttributes },
      });
      if (traceBuffer.length > TRACE_BUFFER_LIMIT) {
        traceBuffer.splice(0, traceBuffer.length - TRACE_BUFFER_LIMIT);
      }
    },
  };
}

export function getRuntimeTraceRecords(filter: {
  traceId?: string;
  agentId?: string;
} = {}): readonly RuntimeTraceRecord[] {
  return traceBuffer.filter(
    (record) =>
      (!filter.traceId || record.traceId === filter.traceId) &&
      (!filter.agentId || record.agentId === filter.agentId),
  );
}

export function exportRuntimeTraceRecords(): string {
  return JSON.stringify(traceBuffer, null, 2);
}

export function clearRuntimeTraceRecords(): void {
  traceBuffer.length = 0;
}

export function installRuntimeTraceDebugApi(): void {
  if (typeof window === "undefined" || !import.meta.env.DEV) return;
  Object.defineProperty(window, "__HOLON_RUNTIME_TRACE__", {
    configurable: true,
    value: {
      records: getRuntimeTraceRecords,
      exportJson: exportRuntimeTraceRecords,
      clear: clearRuntimeTraceRecords,
    },
  });
}

declare global {
  interface Window {
    __HOLON_RUNTIME_TRACE__?: {
      records: typeof getRuntimeTraceRecords;
      exportJson: typeof exportRuntimeTraceRecords;
      clear: typeof clearRuntimeTraceRecords;
    };
  }
}
