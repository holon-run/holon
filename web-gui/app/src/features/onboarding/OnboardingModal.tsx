import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { ModelSelect } from "../../components/models/ModelSelect";
import { Button } from "../../components/ui/Button";
import type {
  AgentTemplateCatalogState,
  RuntimeConfigState,
  RuntimeModelCatalog,
} from "../../runtime/types";

const DEFAULT_TEMPLATE_ID = "holon-default";

interface OnboardingModalProps {
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  runtimeConfig: RuntimeConfigState;
  runtimeConfigLoading: boolean;
  templateCatalog: AgentTemplateCatalogState;
  templateCatalogLoading: boolean;
  onSaveModel: (model: string) => Promise<boolean>;
  onCreateAgent: (agentId: string, template: string) => Promise<boolean>;
  onClose: () => void;
}

export function OnboardingModal({
  modelCatalog,
  modelCatalogLoading,
  runtimeConfig,
  runtimeConfigLoading,
  templateCatalog,
  templateCatalogLoading,
  onSaveModel,
  onCreateAgent,
  onClose,
}: OnboardingModalProps) {
  const { t } = useTranslation();
  const [step, setStep] = useState<1 | 2>(1);
  const [model, setModel] = useState("");
  const [agentId, setAgentId] = useState("");
  const [template, setTemplate] = useState(DEFAULT_TEMPLATE_ID);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | undefined>();

  const templates = templateCatalog.catalog;
  const availableModels = modelCatalog.options.filter((option) => option.available);

  useEffect(() => {
    if (runtimeConfig.surface?.modelDefault && !model) {
      setModel(runtimeConfig.surface.modelDefault);
    } else if (!model && availableModels.length > 0) {
      setModel(availableModels[0].routeRef);
    }
  }, [availableModels, model, runtimeConfig.surface?.modelDefault]);

  function close(): void {
    if (!busy) onClose();
  }

  async function finish(): Promise<void> {
    const trimmedAgentId = agentId.trim();
    if (!model || !trimmedAgentId) return;
    setBusy(true);
    setError(undefined);
    try {
      if (!(await onSaveModel(model))) {
        setError(t("onboarding.saveModelFailed"));
        return;
      }
      if (!(await onCreateAgent(trimmedAgentId, template))) {
        setError(t("onboarding.createAgentFailed"));
        return;
      }
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="modal-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={t("onboarding.title")}
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
                disabled={busy || !model || availableModels.length === 0}
                onClick={() => {
                  setError(undefined);
                  setStep(2);
                }}
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
              <span>{t("app.agentId")}</span>
              <input
                value={agentId}
                onChange={(event) => setAgentId(event.target.value)}
                placeholder="my-agent"
                autoFocus
                disabled={busy}
              />
            </label>
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
            {error ? <span className="connection-error" role="alert">{error}</span> : null}
            <div className="modal-actions">
              <Button type="button" variant="outline" disabled={busy} onClick={() => setStep(1)}>{t("onboarding.back")}</Button>
              <Button type="submit" variant="accent" disabled={busy || !agentId.trim()}>
                {busy ? t("common.creating") : t("onboarding.finish")}
              </Button>
            </div>
          </form>
        )}
      </div>
    </div>
  );
}
