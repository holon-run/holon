import { useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "../../../components/ui/Button";
import { Card } from "../../../components/ui/Card";
import { EmptyState } from "../../../components/ui/EmptyState";
import { StatusChip } from "../../../components/ui/StatusChip";
import { observerSyncDiagnostics, useRuntimeStore } from "../../../runtime/runtime-store";
import {
  clearRuntimeTraceRecords,
  getRuntimeTraceRecords,
  getRuntimeTraceRevision,
  isRuntimeTraceEnabled,
  setRuntimeTraceEnabled,
  subscribeRuntimeTrace,
} from "../../../runtime/runtime-trace";
import type { RuntimeConfigState, RuntimeModelCatalog, RuntimeModelOption } from "../../../runtime/types";

type Surface = NonNullable<RuntimeConfigState["surface"]>;

interface AdvancedSettingsSectionProps {
  surface?: Surface;
  runtimeConfig: RuntimeConfigState;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  modelCatalogError?: string;
  groupedModels: Array<[string, RuntimeModelOption[]]>;
  defaultToolOutputTokens: string;
  setDefaultToolOutputTokens: (value: string) => void;
  maxToolOutputTokens: string;
  setMaxToolOutputTokens: (value: string) => void;
  disableProviderFallback: boolean;
  setDisableProviderFallback: (value: boolean) => void;
  limitsSaveMessage?: string;
  onSaveLimits: () => Promise<void>;
  onRefreshRuntimeConfig: () => Promise<void>;
  onRefreshModels: () => Promise<void>;
}

export function AdvancedSettingsSection({
  surface,
  runtimeConfig,
  runtimeConfigLoading,
  runtimeConfigSaving,
  modelCatalog,
  modelCatalogLoading,
  modelCatalogError,
  groupedModels,
  defaultToolOutputTokens,
  setDefaultToolOutputTokens,
  maxToolOutputTokens,
  setMaxToolOutputTokens,
  disableProviderFallback,
  setDisableProviderFallback,
  limitsSaveMessage,
  onSaveLimits,
  onRefreshRuntimeConfig,
  onRefreshModels,
}: AdvancedSettingsSectionProps) {
  const { t } = useTranslation();
  useSyncExternalStore(subscribeRuntimeTrace, getRuntimeTraceRevision, getRuntimeTraceRevision);
  const runtimeTraceEnabled = isRuntimeTraceEnabled();
  const runtimeTraceRecordCount = getRuntimeTraceRecords().length;

  const limitsDirty = Boolean(
    surface && (
      defaultToolOutputTokens.trim() !== String(surface.defaultToolOutputTokens)
      || maxToolOutputTokens.trim() !== String(surface.maxToolOutputTokens)
      || disableProviderFallback !== surface.disableProviderFallback
    ),
  );

  return (
    <div className="settings-section-stack">
      <Card className="settings-card settings-primary-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.runtimeDefaults")}</span>
            <h2>{t("settings.executionLimitsHeading")}</h2>
          </div>
          {limitsDirty ? <span className="settings-dirty-pill">{t("settings.unsavedChanges")}</span> : null}
        </div>
        {!surface ? (
          <div className="settings-callout">
            <strong>{t("settings.runtimeConfigUnavailable")}</strong>
            <span>{t("settings.refreshRuntimeHint")}</span>
          </div>
        ) : (
          <form
            className="settings-form"
            onSubmit={(event) => {
              event.preventDefault();
              void onSaveLimits();
            }}
          >
            <div className="settings-form-row">
              <label>
                <span>{t("settings.defaultToolOutputTokens")}</span>
                <input
                  inputMode="numeric"
                  value={defaultToolOutputTokens}
                  onChange={(event) => setDefaultToolOutputTokens(event.target.value)}
                />
              </label>
              <label>
                <span>{t("settings.maxToolOutputTokens")}</span>
                <input
                  inputMode="numeric"
                  value={maxToolOutputTokens}
                  onChange={(event) => setMaxToolOutputTokens(event.target.value)}
                />
              </label>
            </div>
            <label className="settings-checkbox">
              <input
                type="checkbox"
                checked={disableProviderFallback}
                onChange={(event) => setDisableProviderFallback(event.target.checked)}
              />
              <span>{t("settings.disableProviderFallback")}</span>
            </label>
            <div className="settings-actions">
              <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                {runtimeConfigSaving ? t("settings.saving") : t("settings.save")}
              </Button>
              {limitsSaveMessage ? <span className="settings-save-message">{limitsSaveMessage}</span> : null}
            </div>
          </form>
        )}
      </Card>

      <Card className="settings-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("developerDiagnostics.eyebrow")}</span>
            <h2>{t("developerDiagnostics.title")}</h2>
          </div>
          <StatusChip
            className={`settings-status ${runtimeTraceEnabled ? "available" : "unavailable"}`}
            tone={runtimeTraceEnabled ? "success" : "error"}
            title={t(runtimeTraceEnabled ? "developerDiagnostics.enabled" : "developerDiagnostics.disabled")}
          />
        </div>
        <p className="settings-muted">{t("developerDiagnostics.description")}</p>
        <label className="settings-checkbox runtime-trace-setting">
          <input
            checked={runtimeTraceEnabled}
            type="checkbox"
            onChange={(event) => setRuntimeTraceEnabled(event.target.checked)}
          />
          <span>{t("developerDiagnostics.enable")}</span>
        </label>
        <div className="settings-callout">
          <strong>{t("runtimeTrace.privacyTitle")}</strong>
          <span>{t("runtimeTrace.privacyDescription")}</span>
        </div>
        <div className="settings-actions">
          <span className="settings-muted">
            {t("runtimeTrace.totalRecordCount", { count: runtimeTraceRecordCount })}
          </span>
          <Button
            type="button"
            variant="secondary"
            disabled={runtimeTraceRecordCount === 0}
            onClick={clearRuntimeTraceRecords}
          >
            {t("runtimeTrace.clear")}
          </Button>
        </div>
      </Card>

      <ObserverSyncDiagnosticsCard />

      <Card className="settings-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.tabAdvanced")}</span>
            <h2>{t("settings.rawConfig")}</h2>
          </div>
          <Button type="button" variant="secondary" disabled={runtimeConfigLoading} onClick={() => void onRefreshRuntimeConfig()}>
            {runtimeConfigLoading ? t("settings.refreshing") : t("common.refresh")}
          </Button>
        </div>
        <div className="settings-callout">
          <strong>{t("settings.rawConfigNotExposed")}</strong>
          <span>{t("settings.rawConfigHint")}</span>
        </div>
        <dl className="settings-list compact">
          <div>
            <dt>{t("settings.configFile")}</dt>
            <dd><code>{runtimeConfig.configFilePath ?? t("settings.notReported")}</code></dd>
          </div>
          <div>
            <dt>{t("settings.modelCatalogLabel")}</dt>
            <dd>{t("settings.modelCatalogSummary", { models: modelCatalog.options.length, providers: groupedModels.length })}</dd>
          </div>
          <div>
            <dt>{t("settings.searchProvidersLabel")}</dt>
            <dd>{surface?.webSearchProviders.map((provider) => provider.id).join(", ") || t("settings.builtinDefaultsValue")}</dd>
          </div>
        </dl>
      </Card>

      <Card className="settings-card settings-models">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.modelsProvidersLabel")}</span>
            <h2>{t("settings.modelCatalogDiagnostics")}</h2>
          </div>
          <Button type="button" variant="secondary" disabled={modelCatalogLoading} onClick={() => void onRefreshModels()}>
            {modelCatalogLoading ? t("settings.refreshing") : t("common.refresh")}
          </Button>
        </div>
        {modelCatalogError ? <div className="settings-error-banner">{modelCatalogError}</div> : null}
        {!modelCatalogLoading && modelCatalog.options.length === 0 ? (
          <EmptyState className="settings-empty" title={t("settings.noModelsReturned")} description={t("settings.noModelsReturnedDesc")} />
        ) : null}
        <p className="settings-muted">{t("settings.modelCatalogDesc")}</p>
        <details className="settings-diagnostics">
          <summary>
            {t("settings.showModelCatalog", {
              summary: t("settings.modelCatalogSummary", { models: modelCatalog.options.length, providers: groupedModels.length }),
            })}
          </summary>
          <div className="provider-list">
            {groupedModels.map(([provider, models]) => (
              <Card className="provider-card" key={provider}>
                <header>
                  <div>
                    <h3>{provider}</h3>
                    <span>
                      {models.filter((model) => model.available).length}/{models.length} {t("settings.availableLabel").toLowerCase()}
                    </span>
                  </div>
                </header>
                <div className="model-table" role="table" aria-label={`${provider} models`}>
                  {models.map((model) => (
                    <div className="model-table-row" role="row" key={model.routeRef}>
                      <div role="cell">
                        <strong>{model.displayName}</strong>
                        <span>{model.model}</span>
                      </div>
                      <div role="cell">
                        <StatusChip
                          className={`settings-status ${model.available ? "available" : "unavailable"}`}
                          tone={model.available ? "success" : "error"}
                          iconOnly
                          title={model.available ? t("settings.availableLabel") : t("settings.unavailableLabel")}
                        />
                      </div>
                      <div role="cell">
                        {model.endpoint !== "default" ? <span className="settings-pill">{model.endpoint}</span> : null}
                        {model.supportsImageInput ? <span className="settings-pill">image input</span> : null}
                        {model.supportsImageGeneration ? <span className="settings-pill">image generation</span> : null}
                        {model.supportsReasoningEffort ? <span className="settings-pill">reasoning</span> : null}
                        {model.unavailableReason ? <small>{model.unavailableReason}</small> : null}
                      </div>
                    </div>
                  ))}
                </div>
              </Card>
            ))}
          </div>
        </details>
      </Card>
    </div>
  );
}

function ObserverSyncDiagnosticsCard() {
  useRuntimeStore((state) => state.briefReadStateByAgentId);
  const diagnostics = observerSyncDiagnostics();
  const { t } = useTranslation();

  return (
    <Card className="settings-card">
      <div className="settings-card-head">
        <div>
          <span className="eyebrow">{t("settings.observerSync")}</span>
          <h2>{t("settings.observerSyncDiagnostics")}</h2>
        </div>
        <StatusChip
          tone={diagnostics.discovery.freshness === "fresh" ? "success" : "warning"}
          title={diagnostics.discovery.freshness}
        />
      </div>
      <p className="settings-muted">{t("settings.observerSyncDiagnosticsDesc")}</p>
      <dl className="settings-list compact">
        <div>
          <dt>{t("settings.discovery")}</dt>
          <dd>
            {diagnostics.discovery.mode} · {diagnostics.discovery.freshness}
          </dd>
        </div>
        <div>
          <dt>{t("settings.runtimeScopeEpoch")}</dt>
          <dd>
            {[
              diagnostics.discovery.runtimeId,
              diagnostics.discovery.visibilityScopeId,
              diagnostics.discovery.eventLogEpoch,
            ]
              .filter(Boolean)
              .join(" · ") || t("settings.notReported")}
          </dd>
        </div>
      </dl>
      {diagnostics.agents.length > 0 ? (
        <details className="settings-diagnostics" open={diagnostics.agents.length <= 3}>
          <summary>{t("settings.showAgentSyncDiagnostics", { count: diagnostics.agents.length })}</summary>
          <div className="provider-list">
            {diagnostics.agents.map((agent) => (
              <Card className="provider-card" key={agent.agentId}>
                <header>
                  <div>
                    <h3>{agent.agentId}</h3>
                    <span>{agent.durability} · {agent.state} · {agent.readCertainty}</span>
                  </div>
                </header>
                <dl className="settings-list compact">
                  <div>
                    <dt>{t("settings.syncCursors")}</dt>
                    <dd>
                      {agent.ingestedThroughSeq ?? "—"} / {agent.projectionReadyThroughSeq ?? "—"} / {agent.observedEventHeadSeq ?? "—"}
                    </dd>
                  </div>
                  <div>
                    <dt>{t("settings.hydrationJobs")}</dt>
                    <dd>{agent.pendingHydrationJobs} pending · {agent.failedHydrationJobs} failed</dd>
                  </div>
                  <div>
                    <dt>{t("settings.resetReason")}</dt>
                    <dd>{agent.resetReason ?? "—"}</dd>
                  </div>
                </dl>
              </Card>
            ))}
          </div>
        </details>
      ) : null}
    </Card>
  );
}
