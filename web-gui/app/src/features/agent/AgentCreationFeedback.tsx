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

export function AgentCreationStatus({ progress, onRefresh }: {
  progress: AgentCreationProgress;
  onRefresh: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const [checking, setChecking] = useState(false);
  return (
    <section className="agent-creation-status" aria-label={t("agentCreation.title")} role="status">
      <strong>{t(`agentCreation.progress.${progress.status}`)}</strong>
      <p>{t(progress.status === "ready" ? "agentCreation.executionSeparate" : "agentCreation.doNotRecreate")}</p>
      {progress.bootstrap ? (
        <ul>{BOOTSTRAP_STEPS.map((key) => (
          <li key={key}>{t(`agentCreation.steps.${key}`)}: {t(`agentCreation.stepStatus.${
            progress.bootstrap![key]?.status === "succeeded" ? "succeeded"
              : progress.bootstrap![key]?.status === "failed" ? "failed" : "pending"
          }`)}</li>
        ))}</ul>
      ) : null}
      {progress.status !== "ready" ? (
        <button type="button" disabled={checking} onClick={async () => {
          setChecking(true);
          try { await onRefresh(); } finally { setChecking(false); }
        }}>{t("agentCreation.checkInitialization")}</button>
      ) : null}
    </section>
  );
}
