export interface SessionEventEnvelope {
  id?: string;
  event_seq?: number;
  ts?: string;
  type?: string;
  payload?: unknown;
}
