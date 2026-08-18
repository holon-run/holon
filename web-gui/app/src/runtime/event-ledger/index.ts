/**
 * Observer-sync event ledger (W1): durable, scope-partitioned IndexedDB
 * storage with atomic composable transactions and explicit failure states.
 */

export {
  LEDGER_DB_NAME,
  LEDGER_DB_VERSION,
  LEDGER_STORES,
  applyLedgerUpgrade,
  deleteLedgerDatabase,
} from "./db";
export {
  LedgerCursorRegressionError,
  LedgerIdentityConflictError,
  LedgerQuotaError,
  LedgerTransactionAbortedError,
  LedgerUnavailableError,
  isQuotaExceeded,
  type LedgerDurability,
  type LedgerUnavailableReason,
} from "./errors";
export {
  agentRecordKey,
  agentScopeRange,
  canonicalRecordKey,
  computeEnvelopeFingerprint,
  hydrationJobKey,
  rawEventKey,
  rawEventRange,
  rawEventRangeBetween,
  stableStringify,
  type LedgerRecordKind,
  type LedgerRemoteScopeKey,
  type LedgerScopeKey,
} from "./keys";
export {
  EventLedger,
  EventLedgerWriteBatch,
  type EventLedgerOpenResult,
  type LedgerAgentSessionRecord,
  type LedgerCanonicalRecord,
  type LedgerHydrationJobRecord,
  type LedgerRawEventRecord,
  type LedgerReadStateRecord,
  type LedgerRuntimeScopeRecord,
  type RawEventClassification,
} from "./ledger";
export {
  LEGACY_DB_NAME,
  deleteLegacyDatabase,
  hasUnreadMigrationNoticeBeenShown,
  initializeFreshBaseline,
  markUnreadMigrationNoticeShown,
  type LegacyBaselineMeta,
  type UnreadMigrationNoticeMeta,
} from "./migration";
