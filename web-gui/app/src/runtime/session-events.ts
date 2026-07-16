import type { components } from "./generated/openapi";

export type SessionEventEnvelope = Partial<components["schemas"]["StreamEventEnvelope"]>;

const RUNTIME_EVENT_CONTRACT_VERSION = 2;
const runtimeEventSchemas: Record<string, string> = {
  message_enqueued: "holon.runtime_event.message_lifecycle",
  message_processing_started: "holon.runtime_event.message_lifecycle",
  brief_created: "holon.runtime_event.brief_created",
  task_created: "holon.runtime_event.task_lifecycle",
  task_status_updated: "holon.runtime_event.task_lifecycle",
  task_result_received: "holon.runtime_event.task_lifecycle",
  work_item_written: "holon.runtime_event.work_item_lifecycle",
  agent_state_changed: "holon.runtime_event.agent_state_changed",
};

export function canApplySessionEvent(event: SessionEventEnvelope): boolean {
  const contractVersion = event.contract_version ?? 1;
  if (contractVersion < RUNTIME_EVENT_CONTRACT_VERSION) return true;
  if (contractVersion !== RUNTIME_EVENT_CONTRACT_VERSION || !event.type) return false;
  return (
    runtimeEventSchemas[event.type] === event.payload_schema &&
    event.payload_schema_version === 1
  );
}
