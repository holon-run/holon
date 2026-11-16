import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import { FileBrowserPanel, type FileBrowserSnapshot, type FileRevision } from "./FileBrowserPanel";

interface FloatingFileBrowserProps {
  agentId?: string;
  workspaceId: string;
  executionRootId?: string;
  initialPath?: string;
  initialFilePath?: string;
  initialFragment?: string;
  workspaceLabel?: string;
  snapshot?: FileBrowserSnapshot;
  onSnapshot?: (snapshot: FileBrowserSnapshot) => void;
  fileRevisions?: readonly FileRevision[];
  onClose: () => void;
}

/**
 * Large floating Explorer dialog. Browsing keeps the conversation visible and
 * reachable behind the dimmed overlay; Esc, the backdrop, or the header close
 * returns focus to where the window was opened.
 */
export function FloatingFileBrowser({
  agentId,
  workspaceId,
  executionRootId,
  initialPath,
  initialFilePath,
  initialFragment,
  workspaceLabel,
  snapshot,
  onSnapshot,
  fileRevisions,
  onClose,
}: FloatingFileBrowserProps) {
  const { t } = useTranslation();
  const dialog = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.current?.querySelector<HTMLElement>(".modal-close")?.focus();
    return () => { previous?.focus(); };
  }, []);
  const close = () => onClose();
  const label = workspaceLabel ?? workspaceId;
  return createPortal(
    <div
      className="modal-overlay floating-file-browser-overlay"
      ref={dialog}
      role="dialog"
      aria-modal="true"
      aria-label={t("rightPanel.fileBrowser")}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          // Keep Escape for this dialog only; the side panel must stay open.
          event.preventDefault();
          event.stopPropagation();
          close();
        }
        if (event.key !== "Tab") return;
        const elements = [...event.currentTarget.querySelectorAll<HTMLElement>(
          'button:not(:disabled), input:not(:disabled), select:not(:disabled), summary, [tabindex="0"]',
        )].filter((element) => !element.closest("details:not([open])")
          || element.tagName === "SUMMARY");
        const first = elements[0];
        const last = elements[elements.length - 1];
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <div className="floating-file-browser-card">
        <div className="floating-file-browser-head">
          <strong title={label}>{label}</strong>
          <button type="button" className="modal-close" aria-label={t("common.close")} onClick={close}>×</button>
        </div>
        <div className="floating-file-browser-body">
          <FileBrowserPanel
            agentId={agentId}
            workspaceId={workspaceId}
            executionRootId={executionRootId}
            initialPath={initialPath}
            initialFilePath={initialFilePath}
            initialFragment={initialFragment}
            workspaceLabel={workspaceLabel}
            snapshot={snapshot}
            onSnapshot={onSnapshot}
            fileRevisions={fileRevisions}
            onClose={close}
          />
        </div>
      </div>
    </div>,
    document.body,
  );
}
