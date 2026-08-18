/**
 * Legacy-baseline migration policy for the event ledger (W1).
 *
 * The legacy `holon-webgui-cache` database is NOT imported:
 * - its event/session cache, cursors, and read markers are not authoritative
 *   for the new correctness keys and must not seed the new database;
 * - the authoritative baseline comes only from server roster/projection
 *   snapshots, event catch-up, and read markers produced by this browser in
 *   the new database;
 * - the legacy database stays untouched until the W6 soak and Web rollback
 *   window complete; only then is it deleted (cleanup failures are
 *   diagnostics only and must never pollute the new database).
 */

import type { EventLedger } from "./ledger";

export const LEGACY_DB_NAME = "holon-webgui-cache";

export const LEGACY_BASELINE_META_KEY = "legacy_baseline_v1";
export const UNREAD_MIGRATION_NOTICE_META_KEY = "unread_state_migration_notice_v1";

export interface LegacyBaselineMeta {
  metaKey: typeof LEGACY_BASELINE_META_KEY;
  strategy: "fresh_server_authoritative";
  legacyDbName: typeof LEGACY_DB_NAME;
  legacyImported: false;
  decidedAt: number;
}

export interface UnreadMigrationNoticeMeta {
  metaKey: typeof UNREAD_MIGRATION_NOTICE_META_KEY;
  shownAt: number;
}

/**
 * Record the fresh-baseline decision exactly once. Never reads the legacy
 * database. Optional non-authoritative sort-hint copying from the legacy
 * database is deliberately not implemented: it is not a precondition for
 * exact mode, and skipping it keeps this boundary unambiguous.
 */
export async function initializeFreshBaseline(
  ledger: EventLedger,
): Promise<LegacyBaselineMeta> {
  const existing = await ledger.getMigrationMeta<LegacyBaselineMeta>(LEGACY_BASELINE_META_KEY);
  if (existing && existing.metaKey === LEGACY_BASELINE_META_KEY) {
    return existing;
  }
  const meta: LegacyBaselineMeta = {
    metaKey: LEGACY_BASELINE_META_KEY,
    strategy: "fresh_server_authoritative",
    legacyDbName: LEGACY_DB_NAME,
    legacyImported: false,
    decidedAt: Date.now(),
  };
  await ledger.putMigrationMeta(LEGACY_BASELINE_META_KEY, meta);
  return meta;
}

/** One-time local unread-state migration notice state (display-only). */
export async function hasUnreadMigrationNoticeBeenShown(
  ledger: EventLedger,
): Promise<boolean> {
  const meta = await ledger.getMigrationMeta<UnreadMigrationNoticeMeta>(
    UNREAD_MIGRATION_NOTICE_META_KEY,
  );
  return meta?.metaKey === UNREAD_MIGRATION_NOTICE_META_KEY;
}

export async function markUnreadMigrationNoticeShown(ledger: EventLedger): Promise<void> {
  const meta: UnreadMigrationNoticeMeta = {
    metaKey: UNREAD_MIGRATION_NOTICE_META_KEY,
    shownAt: Date.now(),
  };
  await ledger.putMigrationMeta(UNREAD_MIGRATION_NOTICE_META_KEY, meta);
}

/**
 * Delete the legacy database. Intended for the W6 post-soak cleanup step
 * only. Failures must be recorded as diagnostics and must never be allowed
 * to corrupt or block the new ledger; the resolve value reports the outcome
 * instead of throwing.
 */
export function deleteLegacyDatabase(): Promise<{ ok: boolean; error?: string }> {
  return new Promise((resolve) => {
    if (typeof indexedDB === "undefined") {
      resolve({ ok: false, error: "indexedDB unavailable" });
      return;
    }
    try {
      const request = indexedDB.deleteDatabase(LEGACY_DB_NAME);
      request.onsuccess = () => resolve({ ok: true });
      request.onerror = () =>
        resolve({ ok: false, error: request.error?.message ?? "delete failed" });
      request.onblocked = () => resolve({ ok: false, error: "blocked by open connections" });
    } catch (error) {
      resolve({ ok: false, error: error instanceof Error ? error.message : "delete threw" });
    }
  });
}
