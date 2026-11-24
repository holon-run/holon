import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  BOOTSTRAP_STEPS,
  type AgentCreationIdentity,
  type AgentCreationLookup,
  type AgentCreationProgress,
  type AgentCreationResult,
} from "../../runtime/agent-creation";

/** Never renders raw server errors, job summaries, or bootstrap last_error. */
export function AgentCreationError({ result, onCheck, onOpen }: {
  result: AgentCreationResult;
  onCheck: () => Promise<AgentCreationLookup>;
  onOpen: (identity: AgentCreationIdentity) => void;
}) {
  const { t } = useTranslation();
  const [checking, setChecking] = useState(false);
  const [lookup, setLookup] = useState<AgentCreationLookup>();
  if (result.kind === "accepted" || result.kind === "stale") return null;
  const uncertain = result.kind === "uncertain";
  return (
    <div className="agent-creation-feedback">
      <p className="connection-error" role="alert">
        {uncertain ? t("agentCreation.uncertain") : t(`agentCreation.problems.${result.problem}`)}
        {!uncertain ? ` (HTTP ${result.status})` : null}
      </p>
      <button type="button" disabled={checking} onClick={async () => {
        setChecking(true);
        setLookup(undefined);
        try {
          const value = await onCheck();
          setLookup(value.kind === "stale" ? undefined : value);
        } finally {
          setChecking(false);
        }
      }}>{t("agentCreation.checkExisting")}</button>
      {lookup?.kind === "found" ? (
        <button type="button" onClick={() => onOpen(lookup.identity)}>{t("agentCreation.openExisting")}</button>
      ) : lookup && lookup.kind !== "stale" ? (
        <p role="status">{t(`agentCreation.lookup.${lookup.kind}`)}</p>
      ) : null}
    </div>
  );
}

type BootstrapStepStatus = "succeeded" | "failed" | "pending";

function stepStatusOf(status: string | undefined): BootstrapStepStatus {
  return status === "succeeded" ? "succeeded" : status === "failed" ? "failed" : "pending";
}

export function AgentCreationStatus({ progress, onRefresh }: {
  progress: AgentCreationProgress;
  onRefresh: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const [checking, setChecking] = useState(false);
  // Only a fully initialized creation collapses its step detail; degraded or
  // still-pending steps stay expanded so the operator sees what needs repair.
  const collapsed = progress.status === "ready";
  return (
    <section
      className={`agent-creation-status agent-creation-status--${progress.status}`}
      aria-label={t("agentCreation.title")}
      role="status"
    >
      <div className="agent-creation-head">
        <span className="agent-creation-indicator" aria-hidden="true" />
        <span className="agent-creation-heading">{t("agentCreation.summary")}</span>
        <span className="agent-creation-badge">{t(`agentCreation.progress.${progress.status}`)}</span>
      </div>
      <p className="agent-creation-hint">
        {t(progress.status === "ready" ? "agentCreation.executionSeparate" : "agentCreation.doNotRecreate")}
      </p>
      {progress.bootstrap ? (
        <details className="agent-creation-details" open={!collapsed}>
          <summary>{t("agentCreation.details")}</summary>
          <ul className="agent-creation-steps">
            {BOOTSTRAP_STEPS.map((key) => {
              const status = stepStatusOf(progress.bootstrap![key]?.status);
              return (
                <li key={key} className={`agent-creation-step agent-creation-step--${status}`}>
                  <span className="agent-creation-step-name">{t(`agentCreation.steps.${key}`)}</span>
                  <span className="agent-creation-step-state">{t(`agentCreation.stepStatus.${status}`)}</span>
                </li>
              );
            })}
          </ul>
        </details>
      ) : null}
      {progress.status !== "ready" ? (
        <div className="agent-creation-actions">
          <button type="button" disabled={checking} onClick={async () => {
            setChecking(true);
            try { await onRefresh(); } finally { setChecking(false); }
          }}>{t("agentCreation.checkInitialization")}</button>
        </div>
      ) : null}
    </section>
  );
}
