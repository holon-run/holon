import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "../../../components/ui/Button";
import { Card } from "../../../components/ui/Card";
import { StatusChip } from "../../../components/ui/StatusChip";
import type {
  RuntimeConfigState,
  RuntimeLocalOnnxPresetStatus,
  RuntimeModelOption,
} from "../../../runtime/types";
import { isDecisionCatalogRoute } from "../settings-helpers";

type Surface = NonNullable<RuntimeConfigState["surface"]>;

interface DecisionSettingsSectionProps {
  surface?: Surface;
  decisionModels: RuntimeModelOption[];
  decisionEnabled: boolean;
  setDecisionEnabled: (value: boolean) => void;
  decisionModel: string;
  setDecisionModel: (value: string) => void;
  decisionPreset: string;
  setDecisionPreset: (value: string) => void;
  decisionModelDir: string;
  setDecisionModelDir: (value: string) => void;
  decisionVariant: string;
  setDecisionVariant: (value: string) => void;
  decisionNumThreads: string;
  setDecisionNumThreads: (value: string) => void;
  decisionChecksum: string;
  setDecisionChecksum: (value: string) => void;
  advisoryToolEnabled: boolean;
  setAdvisoryToolEnabled: (value: boolean) => void;
  advisoryToolMaxCalls: string;
  setAdvisoryToolMaxCalls: (value: string) => void;
  advisoryToolTimeout: string;
  setAdvisoryToolTimeout: (value: string) => void;
  advisoryToolMinConfidence: string;
  setAdvisoryToolMinConfidence: (value: string) => void;
  decisionSaveMessage?: string;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  onSaveDecision: () => Promise<void>;
  onUpdateRuntimeConfig: (updates: Array<{ key: string; value?: unknown; unset?: boolean }>) => Promise<RuntimeConfigState | undefined>;
  onDownloadLocalOnnxPreset: (preset: string) => Promise<RuntimeLocalOnnxPresetStatus>;
  onCancelLocalOnnxPreset: (preset: string) => Promise<RuntimeLocalOnnxPresetStatus>;
}

export function DecisionSettingsSection({
  surface,
  decisionModels,
  decisionEnabled,
  setDecisionEnabled,
  decisionModel,
  setDecisionModel,
  decisionPreset,
  setDecisionPreset,
  decisionModelDir,
  setDecisionModelDir,
  decisionVariant,
  setDecisionVariant,
  decisionNumThreads,
  setDecisionNumThreads,
  decisionChecksum,
  setDecisionChecksum,
  advisoryToolEnabled,
  setAdvisoryToolEnabled,
  advisoryToolMaxCalls,
  setAdvisoryToolMaxCalls,
  advisoryToolTimeout,
  setAdvisoryToolTimeout,
  advisoryToolMinConfidence,
  setAdvisoryToolMinConfidence,
  decisionSaveMessage,
  runtimeConfigLoading,
  runtimeConfigSaving,
  onSaveDecision,
  onUpdateRuntimeConfig,
  onDownloadLocalOnnxPreset,
  onCancelLocalOnnxPreset,
}: DecisionSettingsSectionProps) {
  const { t } = useTranslation();
  const [localOnnxDownload, setLocalOnnxDownload] = useState<RuntimeLocalOnnxPresetStatus | undefined>();
  const [localOnnxDownloading, setLocalOnnxDownloading] = useState(false);

  const decisionModelIsCatalogRoute = isDecisionCatalogRoute(decisionModel, decisionModels);

  return (
    <div className="settings-section-stack">
      <Card className="settings-card settings-primary-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.runtimeDefaults")}</span>
            <h2>{t("settings.decisionProvider")}</h2>
          </div>
          <StatusChip
            className={`settings-status ${decisionEnabled ? "available" : "unavailable"}`}
            tone={decisionEnabled ? "success" : "warning"}
            title={decisionEnabled ? t("settings.decisionEnabledChip") : t("settings.decisionDisabledChip")}
          />
        </div>
        {!surface ? (
          <div className="settings-callout">
            <strong>{t("settings.decisionConfigUnavailable")}</strong>
            <span>{t("settings.refreshRuntimeHint")}</span>
          </div>
        ) : (
          <form
            className="settings-form"
            onSubmit={(event) => {
              event.preventDefault();
              void onSaveDecision();
            }}
          >
            <label className="settings-checkbox">
              <input type="checkbox" checked={decisionEnabled} onChange={(event) => setDecisionEnabled(event.target.checked)} />
              <span>{t("settings.enableDecisionProvider")}</span>
            </label>
            <p className="settings-hint">{t("settings.decisionEnableHint")}</p>
            <label>
              <span>{t("settings.decisionModel")}</span>
              <select
                value={decisionModelIsCatalogRoute ? decisionModel : ""}
                onChange={(event) => setDecisionModel(event.target.value)}
                disabled={decisionModels.length === 0}
              >
                <option value="">{t("settings.decisionModelNone")}</option>
                {decisionModels.map((model) => (
                  <option key={model.routeRef} value={model.routeRef} disabled={!model.available}>
                    {model.displayName} · {model.provider} · {model.decisionProtocol ?? "unknown"}
                  </option>
                ))}
              </select>
            </label>
            <p className="settings-hint">
              {decisionModelIsCatalogRoute
                ? t("settings.decisionModelHint")
                : t("settings.decisionModelNotInCatalog", { model: decisionModel })}
            </p>
            <details className="settings-advanced">
              <summary>{t("settings.decisionLocalOnnx")}</summary>
              <label className="settings-checkbox">
                <input
                  type="checkbox"
                  checked={surface.decision?.localOnnx.enabled ?? false}
                  onChange={(event) => {
                    void onUpdateRuntimeConfig([{
                      key: "decision.local_onnx.enabled",
                      value: event.target.checked,
                    }]);
                  }}
                />
                <span>{t("settings.enableDecisionLocalOnnx")}</span>
              </label>
              <label>
                <span>{t("settings.decisionPreset")}</span>
                <input
                  value={decisionPreset}
                  onChange={(event) => setDecisionPreset(event.target.value)}
                  placeholder="jev-selector-q4f16"
                />
              </label>
              <div className="settings-inline-actions">
                <Button
                  type="button"
                  disabled={localOnnxDownloading || !decisionPreset.trim()}
                  onClick={async () => {
                    setLocalOnnxDownloading(true);
                    try {
                      setLocalOnnxDownload(await onDownloadLocalOnnxPreset(decisionPreset.trim()));
                    } finally {
                      setLocalOnnxDownloading(false);
                    }
                  }}
                >
                  {localOnnxDownloading ? t("settings.downloadingDecisionPreset") : t("settings.downloadDecisionPreset")}
                </Button>
                {localOnnxDownload?.cancellable ? (
                  <Button
                    type="button"
                    variant="secondary"
                    disabled={localOnnxDownloading}
                    onClick={async () => {
                      setLocalOnnxDownload(await onCancelLocalOnnxPreset(decisionPreset.trim()));
                    }}
                  >
                    {t("common.cancel")}
                  </Button>
                ) : null}
                {localOnnxDownload ? (
                  <span className="settings-hint">
                    {localOnnxDownload.complete
                      ? t("settings.decisionPresetReady")
                      : `${t("settings.decisionPresetIncomplete")} (${localOnnxDownload.phase}, ${localOnnxDownload.downloadedBytes} bytes)`}
                  </span>
                ) : null}
              </div>
              <label>
                <span>{t("settings.decisionModelDir")}</span>
                <input
                  value={decisionModelDir}
                  onChange={(event) => setDecisionModelDir(event.target.value)}
                  placeholder="/var/lib/holon/models/decision"
                />
              </label>
              <p className="settings-hint">{t("settings.decisionModelDirHint")}</p>
              <div className="settings-form-row">
                <label>
                  <span>{t("settings.decisionVariant")}</span>
                  <select value={decisionVariant} onChange={(event) => setDecisionVariant(event.target.value)}>
                    <option value="fp32">fp32</option>
                    <option value="fp16">fp16</option>
                    <option value="q4">q4</option>
                    <option value="q4f16">q4f16</option>
                  </select>
                </label>
                <label>
                  <span>{t("settings.decisionNumThreads")}</span>
                  <input
                    type="number"
                    min="1"
                    step="1"
                    value={decisionNumThreads}
                    onChange={(event) => setDecisionNumThreads(event.target.value)}
                  />
                </label>
                <label>
                  <span>{t("settings.decisionChecksum")}</span>
                  <input
                    value={decisionChecksum}
                    onChange={(event) => setDecisionChecksum(event.target.value)}
                    placeholder="sha256:..."
                  />
                </label>
              </div>
              <p className="settings-hint">{t("settings.decisionChecksumHint")}</p>
            </details>

            <label className="settings-checkbox">
              <input
                type="checkbox"
                checked={advisoryToolEnabled}
                onChange={(event) => setAdvisoryToolEnabled(event.target.checked)}
              />
              <span>{t("settings.enableAdvisoryDecisionTool")}</span>
            </label>
            <p className="settings-hint">{t("settings.advisoryDecisionToolHint")}</p>
            {advisoryToolEnabled ? (
              <div className="settings-form-row">
                <label>
                  <span>{t("settings.advisoryDecisionMaxCalls")}</span>
                  <input
                    inputMode="numeric"
                    value={advisoryToolMaxCalls}
                    onChange={(event) => setAdvisoryToolMaxCalls(event.target.value)}
                  />
                </label>
                <label>
                  <span>{t("settings.advisoryDecisionTimeout")}</span>
                  <input
                    inputMode="numeric"
                    value={advisoryToolTimeout}
                    onChange={(event) => setAdvisoryToolTimeout(event.target.value)}
                  />
                </label>
                <label>
                  <span>{t("settings.advisoryDecisionMinConfidence")}</span>
                  <input
                    inputMode="numeric"
                    value={advisoryToolMinConfidence}
                    onChange={(event) => setAdvisoryToolMinConfidence(event.target.value)}
                  />
                </label>
              </div>
            ) : null}
            <div className="settings-actions">
              <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                {runtimeConfigSaving ? t("settings.saving") : t("settings.saveDecision")}
              </Button>
              {decisionSaveMessage ? <span className="settings-save-message">{decisionSaveMessage}</span> : null}
            </div>
          </form>
        )}
      </Card>
    </div>
  );
}
