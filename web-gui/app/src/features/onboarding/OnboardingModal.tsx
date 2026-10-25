import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { ModelSelect } from "../../components/models/ModelSelect";
import { Button } from "../../components/ui/Button";
import { SetupStatusCard, setupIsReady } from "./SetupStatusCard";
import { AgentCreationError } from "../agent/AgentCreationFeedback";
import type { AgentCreationLookup, AgentCreationResult } from "../../runtime/agent-creation";
import type {
  AgentTemplateCatalogState,
  RuntimeConfigState,
  RuntimeModelCatalog,
} from "../../runtime/types";

const DEFAULT_TEMPLATE_ID = "holon-default";

interface OnboardingModalProps {
  active?: boolean;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  runtimeConfig: RuntimeConfigState;
  runtimeConfigLoading: boolean;
  templateCatalog: AgentTemplateCatalogState;
  templateCatalogLoading: boolean;
  onSaveModel: (model: string) => Promise<boolean>;
  onCreateAgent: (agentId: string, template: string, name?: string) => Promise<AgentCreationResult>;
  onLookupAgent: (agentId: string) => Promise<AgentCreationLookup>;
  onConfigure?: (action: string, provider: string | null) => void;
  onRefresh?: () => void;
  existingAgents?: Array<{ id: string; name?: string }>;
  onOpenAgent?: (agentId: string, incarnation?: number) => void;
  onClose: () => void;
}

export function OnboardingModal({
  active = true,
  modelCatalog,
  modelCatalogLoading,
  runtimeConfig,
  runtimeConfigLoading,
  templateCatalog,
  templateCatalogLoading,
  onSaveModel,
  onCreateAgent,
  onLookupAgent,
  onConfigure,
  onRefresh,
  existingAgents = [],
  onOpenAgent,
  onClose,
}: OnboardingModalProps) {
  const { t } = useTranslation();
  const [step, setStep] = useState<1 | 2>(1);
  const [model, setModel] = useState("");
  const [agentId, setAgentId] = useState("holon-bot");
  const [name, setName] = useState("Holon Bot");
  const [template, setTemplate] = useState(DEFAULT_TEMPLATE_ID);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | undefined>();
  const submitting = useRef(false);
  const dialog = useRef<HTMLDivElement>(null);
  const [creationResult, setCreationResult] = useState<AgentCreationResult>();
  const creationUncertain = creationResult?.kind === "uncertain";

  const templates = templateCatalog.catalog;
  const availableModels = modelCatalog.options.filter((option) => option.available);
  useEffect(() => {
    if (!active) return;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const target = step === 2 ? dialog.current?.querySelector<HTMLElement>("form input")
      : dialog.current?.querySelector<HTMLElement>(".model-select-trigger, .modal-close");
    target?.focus();
    return () => { previous?.focus(); };
  }, [active, step]);

  useEffect(() => {
    if (runtimeConfig.surface?.modelDefault && !model) {
      setModel(runtimeConfig.surface.modelDefault);
    }
  }, [model, runtimeConfig.surface?.modelDefault]);

  function close(): void {
    if (!busy) onClose();
  }

  async function next(): Promise<void> {
    if (submitting.current || !model) return;
    submitting.current = true;
    setBusy(true);
    setError(undefined);
    try {
      if (await onSaveModel(model)) setStep(2);
      else setError(t("onboarding.saveModelFailed"));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  }

  async function finish(): Promise<void> {
    const trimmedAgentId = agentId.trim();
    if (submitting.current || creationUncertain || !setupIsReady(runtimeConfig)
      || !trimmedAgentId || !name.trim()) return;
    if (existingAgents.some((agent) => agent.id === trimmedAgentId)) {
      setCreationResult({ kind: "rejected", problem: "conflict", status: 409 });
      return;
    }
    submitting.current = true;
    setBusy(true);
    setError(undefined);
    setCreationResult(undefined);
    try {
      const result = await onCreateAgent(trimmedAgentId, template, name.trim());
      if (result.kind === "accepted") onClose();
      else if (result.kind !== "stale") setCreationResult(result);
    } catch {
      setCreationResult({ kind: "uncertain" });
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  }

  return (
    <div
      className="modal-overlay"
      ref={dialog}
      role="dialog"
      aria-modal="true"
      aria-label={t("onboarding.title")}
      onKeyDown={(event) => {
        if (event.key === "Escape") close();
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
      <div className="modal-card onboarding-card">
        <div className="modal-head">
          <div>
            <strong>{t("onboarding.title")}</strong>
            <p className="onboarding-step">{t("onboarding.step", { current: step, total: 2 })}</p>
          </div>
          <button type="button" className="modal-close" aria-label={t("common.close")} onClick={close}>×</button>
        </div>
        <SetupStatusCard config={runtimeConfig} loading={runtimeConfigLoading}
          saving={busy && step === 1}
          onRepair={(action, provider) => { if (!busy) onConfigure?.(action, provider); }}
          onRetry={() => onRefresh?.()} />
        {existingAgents.length > 0 && onOpenAgent ? (
          <details className="settings-advanced">
            <summary>{t("setup.existing")}</summary>
            {existingAgents.map((agent) => (
              <Button key={agent.id} type="button" variant="outline" disabled={busy}
                onClick={() => onOpenAgent(agent.id)}>{agent.name ?? agent.id} ({agent.id})</Button>
            ))}
          </details>
        ) : null}
        {step === 1 ? (
          <div className="modal-body">
            <p>{t("onboarding.modelIntro")}</p>
            {runtimeConfigLoading || modelCatalogLoading ? (
              <div role="status">{t("onboarding.loadingModel")}</div>
            ) : modelCatalog.options.length === 0 ? (
              <div className="connection-error" role="alert">{t("onboarding.noModels")}</div>
            ) : (
              <ModelSelect
                label={t("onboarding.modelLabel")}
                options={modelCatalog.options}
                value={model}
                onChange={setModel}
              />
            )}
            {error ? <span className="connection-error" role="alert">{error}</span> : null}
            <div className="modal-actions">
              <Button type="button" variant="outline" disabled={busy} onClick={close}>{t("common.cancel")}</Button>
              <Button
                type="button"
                variant="accent"
                disabled={busy || runtimeConfigLoading || !availableModels.some((option) => option.routeRef === model)}
                onClick={() => void next()}
              >
                {t("onboarding.next")}
              </Button>
            </div>
          </div>
        ) : (
          <form
            className="modal-body"
            onSubmit={(event) => {
              event.preventDefault();
              void finish();
            }}
          >
            <p>{t("onboarding.agentIntro")}</p>
            <label>
              <span>{t("setup.name")}</span>
              <input value={name} onChange={(event) => setName(event.target.value)}
                autoFocus disabled={busy || creationUncertain} />
            </label>
            <p>{t("setup.nameHint")}</p>
            <details className="settings-advanced">
              <summary>{t("setup.advanced")}</summary>
            <label>
              <span>{t("app.agentId")}</span>
              <input
                value={agentId}
                onChange={(event) => { setAgentId(event.target.value); setCreationResult(undefined); }}
                disabled={busy || creationUncertain}
              />
            </label>
            <p>{t("setup.idHint")}</p>
            <label>
              <span>{t("app.template")}</span>
              {templateCatalogLoading ? (
                <span role="status">{t("onboarding.loadingTemplate")}</span>
              ) : (
                <select value={template} onChange={(event) => setTemplate(event.target.value)} disabled={busy}>
                  <option value={DEFAULT_TEMPLATE_ID}>{t("onboarding.defaultTemplateOption")}</option>
                  {templates.map((entry) => (
                    <option key={entry.catalogId} value={entry.template}>
                      {entry.name} ({entry.source})
                    </option>
                  ))}
                </select>
              )}
            </label>
            {templates.length === 0 && !templateCatalogLoading ? (
              <span role="note">{t("onboarding.noTemplatesNote")}</span>
            ) : null}
            </details>
            {error ? <span className="connection-error" role="alert">{error}</span> : null}
            {creationResult ? <AgentCreationError key={`${agentId}:${creationResult.kind}`} result={creationResult}
              onCheck={() => onLookupAgent(agentId.trim())}
              onOpen={(identity) => { onClose(); onOpenAgent?.(identity.agent_id, identity.incarnation); }} /> : null}
            <div className="modal-actions">
              <Button type="button" variant="outline" disabled={busy} onClick={() => setStep(1)}>{t("onboarding.back")}</Button>
              <Button type="submit" variant="accent" disabled={busy || creationUncertain || !setupIsReady(runtimeConfig) || !agentId.trim() || !name.trim()}>
                {busy ? t("common.creating") : t("onboarding.finish")}
              </Button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}
