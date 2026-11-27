import { useRef, useState } from "react";
import { ArrowDown, ArrowRight, ArrowUp, GripVertical, Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import { ModelList } from "../../../components/models/ModelList";
import { ModelSelect } from "../../../components/models/ModelSelect";
import { Button } from "../../../components/ui/Button";
import { Card } from "../../../components/ui/Card";
import { modelSourceLabel } from "../../../lib/model-presentation";
import type { RuntimeConfigSurface, RuntimeConfigUpdateResult, RuntimeModelCatalog } from "../../../runtime/types";
import { reorderModelFallbacks } from "../settings-helpers";

interface LanguageModelSettingsSectionProps {
  surface?: RuntimeConfigSurface;
  modelCatalog: RuntimeModelCatalog;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  runtimeConfigError?: string;
  modelDefault: string;
  setModelDefault: (value: string) => void;
  modelFallbacks: string[];
  setModelFallbacks: (updater: string[] | ((current: string[]) => string[])) => void;
  saveMessage?: string;
  rejectedResults: RuntimeConfigUpdateResult[];
  onRefreshRuntimeConfig: () => Promise<void>;
  onSave: () => Promise<void>;
  onReset: () => void;
  onOpenProviders: () => void;
}

export function LanguageModelSettingsSection({
  surface,
  modelCatalog,
  runtimeConfigLoading,
  runtimeConfigSaving,
  runtimeConfigError,
  modelDefault,
  setModelDefault,
  modelFallbacks,
  setModelFallbacks,
  saveMessage,
  rejectedResults,
  onRefreshRuntimeConfig,
  onSave,
  onReset,
  onOpenProviders,
}: LanguageModelSettingsSectionProps) {
  const { t } = useTranslation();
  const [manualFallbackRoute, setManualFallbackRoute] = useState("");
  const [addingFallback, setAddingFallback] = useState(false);
  const [draggedIndex, setDraggedIndex] = useState<number | null>(null);
  const draggedIndexRef = useRef<number | null>(null);

  const addFallbackRoute = (route: string) => {
    const trimmed = route.trim();
    if (!trimmed || modelFallbacks.includes(trimmed)) return;
    setModelFallbacks((current) => [...current, trimmed]);
  };

  const persistedFallbacks = surface?.modelFallbacks ?? [];
  const isDirty = Boolean(
    surface && (
      modelDefault.trim() !== (surface.modelDefault ?? "").trim()
      || modelFallbacks.length !== persistedFallbacks.length
      || modelFallbacks.some((entry, idx) => entry !== persistedFallbacks[idx])
    ),
  );

  return (
    <div className="settings-section-stack settings-model-defaults">
      <Card className="settings-card settings-primary-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.runtimeDefaults")}</span>
            <h2>{t("settings.languageModelHeading")}</h2>
          </div>
          <div className="settings-card-actions">
            {isDirty ? <span className="settings-dirty-pill">{t("settings.unsavedChanges")}</span> : null}
            <Button
              type="button"
              variant="secondary"
              disabled={runtimeConfigLoading}
              onClick={() => void onRefreshRuntimeConfig()}
            >
              {runtimeConfigLoading ? t("settings.refreshing") : t("common.refresh")}
            </Button>
          </div>
        </div>
        {runtimeConfigError ? <div className="settings-error-banner">{runtimeConfigError}</div> : null}
        {!surface ? (
          <div className="settings-callout">
            <strong>{t("settings.runtimeConfigUnavailable")}</strong>
            <span>{t("settings.connectLiveRuntime")}</span>
          </div>
        ) : (
          <form
            className="settings-form"
            onSubmit={(event) => {
              event.preventDefault();
              void onSave();
            }}
          >
            <div id="setup-default-model">
              <ModelSelect
                label={t("settings.defaultModel")}
                options={modelCatalog.options}
                value={modelDefault}
                onChange={setModelDefault}
              />
            </div>
            <p className="settings-hint">{t("modelUi.defaultScope")}</p>

            <div className="settings-form-field settings-fallback-section">
              <div className="settings-field-head">
                <span className="model-select-label">{t("settings.fallbackModels")}</span>
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  aria-expanded={addingFallback}
                  onClick={() => setAddingFallback((open) => !open)}
                >
                  <Plus size={14} />
                  {t("modelUi.addFallback")}
                </Button>
              </div>
              <p className="settings-hint">{t("settings.fallbackOrderHint")}</p>

              {modelFallbacks.length > 0 ? (
                <div className="settings-fallback-list">
                  {modelFallbacks.map((model, index) => {
                    const catalogEntry = modelCatalog.options.find((option) => option.routeRef === model);
                    return (
                      <div
                        key={model}
                        className={`settings-fallback-row${draggedIndex === index ? " dragging" : ""}`}
                        draggable
                        onDragStart={() => {
                          draggedIndexRef.current = index;
                          setDraggedIndex(index);
                        }}
                        onDragOver={(e) => {
                          e.preventDefault();
                          const fromIndex = draggedIndexRef.current;
                          if (fromIndex === null || fromIndex === index) return;
                          draggedIndexRef.current = index;
                          setDraggedIndex(index);
                          setModelFallbacks((current) => reorderModelFallbacks(current, fromIndex, index));
                        }}
                        onDragEnd={() => {
                          draggedIndexRef.current = null;
                          setDraggedIndex(null);
                        }}
                      >
                        <span className="settings-fallback-grip" aria-hidden="true">
                          <GripVertical size={14} />
                        </span>
                        <span className="settings-fallback-index">{index + 1}</span>
                        <div className="settings-fallback-info">
                          <strong>{catalogEntry?.displayName ?? model}</strong>
                          <small>{catalogEntry ? `${modelSourceLabel(catalogEntry)} · ${model}` : model}</small>
                        </div>
                        <div className="settings-fallback-actions">
                          <button
                            type="button"
                            className="model-browser-icon"
                            disabled={index === 0}
                            title={t("settings.moveUp")}
                            aria-label={t("settings.moveUp")}
                            onClick={() => setModelFallbacks((current) => reorderModelFallbacks(current, index, index - 1))}
                          >
                            <ArrowUp size={14} />
                          </button>
                          <button
                            type="button"
                            className="model-browser-icon"
                            disabled={index === modelFallbacks.length - 1}
                            title={t("settings.moveDown")}
                            aria-label={t("settings.moveDown")}
                            onClick={() => setModelFallbacks((current) => reorderModelFallbacks(current, index, index + 1))}
                          >
                            <ArrowDown size={14} />
                          </button>
                          <button
                            type="button"
                            className="model-browser-icon settings-fallback-remove"
                            title={t("common.remove")}
                            aria-label={t("common.remove")}
                            onClick={() => setModelFallbacks(modelFallbacks.filter((m) => m !== model))}
                          >
                            <X size={14} />
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              ) : (
                <div className="settings-fallback-empty">{t("settings.noFallbacksConfigured")}</div>
              )}

              {addingFallback ? (
                <div className="model-select-panel settings-fallback-picker">
                  <ModelList
                    autoFocus
                    options={modelCatalog.options.filter(
                      (model) => model.routeRef !== modelDefault && !modelFallbacks.includes(model.routeRef),
                    )}
                    onSelect={(model) => {
                      addFallbackRoute(model.routeRef);
                      setAddingFallback(false);
                    }}
                  />
                  <div className="model-select-footer">
                    <details className="model-manual">
                      <summary>{t("modelUi.manualRoute")}</summary>
                      <div className="settings-fallback-manual-row">
                        <input
                          value={manualFallbackRoute}
                          onChange={(e) => setManualFallbackRoute(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") {
                              e.preventDefault();
                              addFallbackRoute(manualFallbackRoute);
                              setManualFallbackRoute("");
                              setAddingFallback(false);
                            }
                          }}
                          placeholder="provider@endpoint/model"
                        />
                        <Button
                          type="button"
                          variant="secondary"
                          size="sm"
                          disabled={!manualFallbackRoute.trim()}
                          onClick={() => {
                            addFallbackRoute(manualFallbackRoute);
                            setManualFallbackRoute("");
                            setAddingFallback(false);
                          }}
                        >
                          {t("settings.add")}
                        </Button>
                      </div>
                    </details>
                    <button type="button" className="model-browser-more" onClick={() => setAddingFallback(false)}>
                      {t("common.close")}
                    </button>
                  </div>
                </div>
              ) : null}
            </div>

            <div className="settings-actions">
              <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                {runtimeConfigSaving ? t("settings.saving") : t("settings.save")}
              </Button>
              {isDirty ? (
                <Button type="button" variant="ghost" disabled={runtimeConfigSaving} onClick={onReset}>
                  {t("settings.resetDraft")}
                </Button>
              ) : null}
              {saveMessage ? <span className="settings-save-message">{saveMessage}</span> : null}
            </div>
            {rejectedResults.length ? (
              <div className="settings-error-banner">
                {rejectedResults.map((result) => (
                  <div key={result.key}>
                    <strong>{result.key}</strong>: {result.reason}
                  </div>
                ))}
              </div>
            ) : null}
          </form>
        )}
        <div className="settings-cross-link">
          <button type="button" className="model-browser-more" onClick={onOpenProviders}>
            {t("settings.manageProvidersLink")} <ArrowRight size={14} />
          </button>
        </div>
      </Card>
    </div>
  );
}
