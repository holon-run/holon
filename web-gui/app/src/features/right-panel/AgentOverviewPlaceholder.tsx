import { useEffect } from "react";
import { Maximize2, Minimize2, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import type { AgentAccessStatus } from "../../runtime/useAgentAccess";

interface AgentOverviewPlaceholderProps {
  status: AgentAccessStatus;
  error?: string;
  open: boolean;
  mode: "normal" | "expanded";
  full: boolean;
  onClose: () => void;
  onToggleMode: () => void;
  onRetry: () => void;
}

/** Keep the panel's layout without inventing an AgentSummary before access resolves. */
export function AgentOverviewPlaceholder({
  status, error, open, mode, full, onClose, onToggleMode, onRetry,
}: AgentOverviewPlaceholderProps) {
  const { t } = useTranslation();
  const loading = status === "loading";
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.isComposing || document.querySelector('[role="dialog"]')) return;
      if ((event.metaKey || event.ctrlKey) && event.key === ".") {
        event.preventDefault();
        onToggleMode();
      } else if (event.key === "Escape") {
        event.preventDefault();
        if (mode === "expanded") onToggleMode();
        else onClose();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, mode, onClose, onToggleMode]);

  return (
    <aside className="side-panel" data-mode={mode} data-full={full} aria-label={t("rightPanel.contextPanel")} hidden={!open}>
      <div className="panel-header">
        <div><strong>{t("rightPanel.agentOverview")}</strong></div>
        <div className="panel-actions">
          {full ? <button type="button" onClick={onClose}>{t("rightPanel.backToConversation")}</button> : null}
          <button type="button" aria-label={t(mode === "expanded" ? "rightPanel.restorePanel" : "rightPanel.expandPanel")} onClick={onToggleMode}>
            {mode === "expanded" ? <Minimize2 size={14} aria-hidden="true" /> : <Maximize2 size={14} aria-hidden="true" />}
          </button>
          <button type="button" aria-label={t("panel.closePanel")} onClick={onClose}>
            <X size={16} aria-hidden="true" />
          </button>
        </div>
      </div>
      <div className="panel-body agent-overview-placeholder" aria-busy={loading}>
        {loading ? (
          <>
            <p className="agent-overview-loading-label" role="status">{t("rightPanel.loadingAgentInformation")}</p>
            <div className="inspector-stack" aria-hidden="true">
              {[0, 1, 2, 3].map((section) => (
                <div key={section} className="context-card agent-overview-skeleton">
                  <span className="skeleton-line short" />
                  <span className="skeleton-line" />
                  {section === 0 ? <div className="skeleton-grid"><span /><span /></div> : <span className="skeleton-line" />}
                </div>
              ))}
            </div>
          </>
        ) : (
          <div role="status">
            <EmptyState title={t(`agentAccess.${status}`)} description={error} />
            <Button onClick={onRetry}>{t("common.refresh")}</Button>
          </div>
        )}
      </div>
    </aside>
  );
}
