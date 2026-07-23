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
const TRACE_ENABLED_STORAGE_KEY = "holon:runtimeTraceEnabled";
const traceBuffer: RuntimeTraceRecord[] = [];
const traceListeners = new Set<() => void>();
let fallbackId = 0;
let traceRevision = 0;
let traceEnabled = readStoredTraceEnabled();

function readStoredTraceEnabled(): boolean {
  if (typeof localStorage === "undefined") return false;
  try {
    return localStorage.getItem(TRACE_ENABLED_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

function notifyTraceListeners(): void {
  traceRevision += 1;
  traceListeners.forEach((listener) => listener());
}

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
  if (!traceEnabled) {
    return {
      context,
      end: () => undefined,
    };
  }
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
      notifyTraceListeners();
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
  if (traceBuffer.length === 0) return;
  traceBuffer.length = 0;
  notifyTraceListeners();
}

export function isRuntimeTraceEnabled(): boolean {
  return traceEnabled;
}

export function setRuntimeTraceEnabled(enabled: boolean, options: { clear?: boolean } = {}): void {
  const changed = traceEnabled !== enabled;
  traceEnabled = enabled;
  if (typeof localStorage !== "undefined") {
    try {
      localStorage.setItem(TRACE_ENABLED_STORAGE_KEY, String(enabled));
    } catch {
      // Keep the in-memory setting when browser storage is unavailable.
    }
  }
  if (options.clear) {
    traceBuffer.length = 0;
  }
  updateRuntimeTraceDebugApi();
  if (changed || options.clear) notifyTraceListeners();
}

export function subscribeRuntimeTrace(listener: () => void): () => void {
  traceListeners.add(listener);
  return () => traceListeners.delete(listener);
}

export function getRuntimeTraceRevision(): number {
  return traceRevision;
}

export interface RuntimeTraceDiagnosticBundle {
  schemaVersion: 1;
  exportedAt: string;
  gui: {
    version: string;
    mode: string;
  };
  agentId?: string;
  connection: {
    mode: "local" | "remote";
    source: "http" | "fixture";
    connected: boolean;
  };
  trace: {
    enabled: boolean;
    recordCount: number;
    records: readonly RuntimeTraceRecord[];
  };
}

export function buildRuntimeTraceDiagnosticBundle(options: {
  agentId?: string;
  guiVersion: string;
  mode: string;
  connection: RuntimeTraceDiagnosticBundle["connection"];
  exportedAt?: string;
}): RuntimeTraceDiagnosticBundle {
  const records = getRuntimeTraceRecords({ agentId: options.agentId });
  return {
    schemaVersion: 1,
    exportedAt: options.exportedAt ?? new Date().toISOString(),
    gui: {
      version: options.guiVersion,
      mode: options.mode,
    },
    agentId: options.agentId,
    connection: options.connection,
    trace: {
      enabled: traceEnabled,
      recordCount: records.length,
      records,
    },
  };
}

export function installRuntimeTraceDebugApi(): void {
  updateRuntimeTraceDebugApi();
}

function updateRuntimeTraceDebugApi(): void {
  if (typeof window === "undefined") return;
  if (!traceEnabled) {
    delete window.__HOLON_RUNTIME_TRACE__;
    return;
  }
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
