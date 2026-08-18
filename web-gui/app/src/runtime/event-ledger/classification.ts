/**
 * Envelope classification for durable raw ingestion (W2).
 *
 * Classification is the bridge between the server event contract (S2
 * `projection_effect`, additive) and the ledger's durable demand model:
 * - `none` events never block projection readiness;
 * - `display_invalidation` events block readiness until satisfied —
 *   reference events create durable hydration jobs, self-contained events
 *   are satisfied by ingestion itself (projection replays deterministically
 *   from the raw cache), and deletes complete via canonical tombstones;
 * - envelopes with a contract version above the highest supported version
 *   block readiness outright: their semantics are unknowable.
 *
 * Display level never changes classification. Info/verbose/debug timelines
 * consume the same raw cache; display level only changes projection and
 * hydration demand at the render layer.
 */

import type { LedgerRecordKind } from "./keys";
import type { RawEventClassification } from "./ledger";

/**
 * Highest StreamEventEnvelope.contract_version this client understands.
 * Mirrors RUNTIME_EVENT_CONTRACT_VERSION in `runtime/session-events.ts`;
 * duplicated here so the ledger layer stays app-layer-independent.
 */
export const SUPPORTED_ENVELOPE_CONTRACT_VERSION = 2;

export type EnvelopeProjectionEffect = "none" | "display_invalidation";

/** A canonical record referenced by an event that needs durable hydration. */
export interface EnvelopeRecordReference {
  recordKind: LedgerRecordKind;
  recordId: string;
  /**
   * Minimum canonical revision that satisfies the reference, when the event
   * names one (e.g. `work_item_written` carries `revision`). A newer
   * canonical revision satisfies an older invalidation.
   */
  expectedRevision?: string | number;
}

/** A canonical record deletion carried by an event. */
export interface EnvelopeRecordTombstone {
  recordKind: LedgerRecordKind;
  recordId: string;
}

export interface ClassifiedEnvelope {
  eventSeq: number;
  classification: RawEventClassification;
  /**
   * True when the envelope's contract version is above the highest supported
   * version. Such events are still stored, but readiness can never pass them
   * until a client that understands the version upgrades the boundary.
   */
  blocksReadiness: boolean;
  /** Reference demand: create/refresh a durable hydration job. */
  reference: EnvelopeRecordReference | null;
  /** Delete demand: complete via canonical tombstone. */
  tombstone: EnvelopeRecordTombstone | null;
  /**
   * True when the event affects display but needs no canonical hydration:
   * its payload carries the display-relevant state and projection replays
 * from the raw cache.
   */
  selfContained: boolean;
}

interface EnvelopeLike {
  event_seq?: number | null;
  contract_version?: number | null;
  projection_effect?: EnvelopeProjectionEffect | null;
  type?: string | null;
  payload?: unknown;
}

/**
 * Classify one raw event envelope for ingestion.
 *
 * When the server does not advertise `projection_effect` yet (pre-S2-cutover
 * remotes), classification falls back to the conservative local registry:
 * every known event type invalidates display except the explicitly
 * effect-less diagnostic family. An unrecognized event type with no
 * `projection_effect` is conservatively treated as display-affecting.
 */
export function classifyEnvelope(envelope: EnvelopeLike): ClassifiedEnvelope {
  const eventSeq = envelope.event_seq;
  if (typeof eventSeq !== "number" || !Number.isFinite(eventSeq)) {
    throw new Error("cannot classify envelope without a finite event_seq");
  }
  const contractVersion = envelope.contract_version ?? 1;
  const blocksReadiness = contractVersion > SUPPORTED_ENVELOPE_CONTRACT_VERSION;
  const advertised = envelope.projection_effect ?? null;
  const effect: EnvelopeProjectionEffect =
    advertised ?? localFallbackEffect(envelope.type ?? "");

  const reference = effect === "display_invalidation" ? referenceForEnvelope(envelope) : null;
  const tombstone =
    effect === "display_invalidation" ? tombstoneForEnvelope(envelope) : null;
  return {
    eventSeq,
    classification: {
      projectionEffect: effect,
      envelopeContractVersion: contractVersion,
    },
    blocksReadiness,
    reference,
    tombstone,
    selfContained: effect === "display_invalidation" && reference === null && tombstone === null,
  };
}

/**
 * Reference mapping for known event families. Mirrors the hydration demand
 * of the render layer: messages, briefs, and transcript entries are fetched
 * through canonical batch APIs, everything else is self-contained.
 */
function referenceForEnvelope(
  envelope: EnvelopeLike,
): EnvelopeRecordReference | null {
  const payload = asRecord(envelope.payload);
  const eventType = envelope.type ?? "";
  const expectedRevision = revisionField(payload);
  if (eventType === "message_enqueued" || eventType === "message_processing_started") {
    const messageId = stringField(payload, "message_id");
    return messageId ? { recordKind: "message", recordId: messageId, expectedRevision } : null;
  }
  if (eventType === "brief_created") {
    const briefId = stringField(payload, "brief_id") ?? stringField(payload, "id");
    return briefId ? { recordKind: "brief", recordId: briefId, expectedRevision } : null;
  }
  if (eventType === "assistant_round_recorded") {
    const entryId = stringField(payload, "assistant_round_id");
    return entryId ? { recordKind: "transcript_entry", recordId: entryId, expectedRevision } : null;
  }
  // Unknown reference shapes stay self-contained: the raw envelope is still
  // stored and replayable, so no durable demand is lost.
  return null;
}

/**
 * Revision anchor when the event names one (forward-compatible: current
 * wire payloads do not carry revisions; snapshot and canonical APIs do).
 */
function revisionField(
  payload: Record<string, unknown> | undefined,
): string | number | undefined {
  const value = payload?.revision ?? payload?.brief_revision;
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.length > 0) return value;
  return undefined;
}

/**
 * Delete mapping. No wire delete event exists yet; the tombstone completion
 * path is exercised through pipeline APIs and becomes live when the server
 * contract adds deletion events.
 */
function tombstoneForEnvelope(
  envelope: EnvelopeLike,
): EnvelopeRecordTombstone | null {
  const payload = asRecord(envelope.payload);
  const eventType = envelope.type ?? "";
  if (eventType !== "canonical_record_deleted") return null;
  const recordKind = stringField(payload, "record_kind") as LedgerRecordKind | undefined;
  const recordId = stringField(payload, "record_id");
  if (!recordKind || !recordId) return null;
  if (recordKind !== "message" && recordKind !== "brief" && recordKind !== "transcript_entry") {
    return null;
  }
  return { recordKind, recordId };
}

/** Conservative pre-S2 fallback: diagnostics are inert, everything else displays. */
function localFallbackEffect(eventType: string): EnvelopeProjectionEffect {
  if (eventType === "scheduler_diagnostic") return "none";
  return "display_invalidation";
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function stringField(
  payload: Record<string, unknown> | undefined,
  field: string,
): string | undefined {
  const value = payload?.[field];
  return typeof value === "string" && value.length > 0 ? value : undefined;
}
