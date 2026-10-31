import { useTranslation } from "react-i18next";

import type { ObjectDetailRefreshState } from "../../runtime/types";

export function ObjectDetailRefreshFeedback({
  state,
  onRefresh,
}: {
  state?: ObjectDetailRefreshState;
  onRefresh?: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="detail-refresh-feedback">
      {onRefresh ? <button type="button" className="task-detail-refresh" onClick={onRefresh} disabled={state?.loading || state?.refreshing}>
        {t("rightPanel.refreshDetail")}
      </button> : null}
      {state?.refreshing ? <span role="status">{t("rightPanel.detailRefreshing")}</span> : null}
      {state?.stale ? <span role="status">{t("rightPanel.detailStale")}</span> : null}
      {state?.stale && state.lastSuccessAt !== undefined ? <small>
        {t("rightPanel.detailLastSuccess", { time: new Date(state.lastSuccessAt).toLocaleString() })}
      </small> : null}
      {state?.error ? <p className="inspector-error">{state.error}</p> : null}
    </div>
  );
}
