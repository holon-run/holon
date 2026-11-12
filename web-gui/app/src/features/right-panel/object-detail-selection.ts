import type { WorkItemDetailState, WorkItemSummary } from "../../runtime/types";

/** Navigation carries a fallback label, never authority over newer HTTP data. */
export function resolveWorkItemDetail(
  fallback: WorkItemSummary,
  listed: WorkItemSummary | undefined,
  detail: WorkItemDetailState | undefined,
  listReadId: number | undefined,
): WorkItemSummary {
  const summary = listed ?? fallback;
  const full = detail?.workItem;
  const revisionOrder = full?.revision !== undefined && listed?.revision !== undefined
    ? full.revision - listed.revision : 0;
  const updatedOrder = full?.updatedAt && listed?.updatedAt
    ? Date.parse(full.updatedAt) - Date.parse(listed.updatedAt) : 0;
  const versionOrder = revisionOrder || (Number.isFinite(updatedOrder) ? updatedOrder : 0);
  const sameVersion = full?.revision !== undefined && listed?.revision !== undefined && versionOrder === 0;
  const detailIsNewer = !listed || (versionOrder !== 0
    ? versionOrder > 0 : (detail?.dataRequestId ?? -1) > (listReadId ?? -1));
  return {
    ...summary,
    ...(full && detailIsNewer ? full : {}),
    // State previews do not invalidate full text from the same record version.
    ...(full && sameVersion ? { objective: full.objective, blockedBy: full.blockedBy } : {}),
    // Focus/membership belongs to the active snapshot, including absence.
    current: listed ? listed.current : listReadId === undefined ? summary.current : false,
    ...(full ? {
      planArtifact: detailIsNewer ? full.planArtifact : listed?.planArtifact ?? full.planArtifact,
      todoList: full.todoList,
      workRefs: full.workRefs,
      resultSummary: full.resultSummary,
    } : {}),
  };
}
