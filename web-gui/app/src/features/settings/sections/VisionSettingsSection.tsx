import { ArrowRight } from "lucide-react";
import { useTranslation } from "react-i18next";

import { ModelSelect } from "../../../components/models/ModelSelect";
import { Button } from "../../../components/ui/Button";
import { Card } from "../../../components/ui/Card";
import { StatusChip } from "../../../components/ui/StatusChip";
import type { RuntimeConfigSurface, RuntimeModelOption } from "../../../runtime/types";

interface VisionSettingsSectionProps {
  surface?: RuntimeConfigSurface;
  visionModels: RuntimeModelOption[];
  imageGenModels: RuntimeModelOption[];
  visionDefault: string;
  setVisionDefault: (value: string) => void;
  imageGenDefault: string;
  setImageGenDefault: (value: string) => void;
  visionProviderId?: string;
  visionProviderReady?: boolean;
  imageGenProviderId?: string;
  imageGenProviderReady?: boolean;
  visionSaveMessage?: string;
  imageGenSaveMessage?: string;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  onSaveVision: () => Promise<void>;
  onSaveImageGen: () => Promise<void>;
  onOpenProvider: (providerId?: string) => void;
}

export function VisionSettingsSection({
  surface,
  visionModels,
  imageGenModels,
  visionDefault,
  setVisionDefault,
  imageGenDefault,
  setImageGenDefault,
  visionProviderId,
  visionProviderReady,
  imageGenProviderId,
  imageGenProviderReady,
  visionSaveMessage,
  imageGenSaveMessage,
  runtimeConfigLoading,
  runtimeConfigSaving,
  onSaveVision,
  onSaveImageGen,
  onOpenProvider,
}: VisionSettingsSectionProps) {
  const { t } = useTranslation();

  const visionDirty = Boolean(surface && visionDefault.trim() !== (surface.visionDefault ?? "").trim());
  const imageGenDirty = Boolean(surface && imageGenDefault.trim() !== (surface.imageGenerationDefault ?? "").trim());

  return (
    <div className="settings-section-stack">
      <Card className="settings-card settings-primary-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.tabVision")}</span>
            <h2>{t("settings.imageObservation")}</h2>
          </div>
          {visionDirty ? <span className="settings-dirty-pill">{t("settings.unsavedChanges")}</span> : null}
        </div>
        {!surface ? (
          <div className="settings-callout">
            <strong>{t("settings.visionConfigUnavailable")}</strong>
            <span>{t("settings.connectLiveVision")}</span>
          </div>
        ) : (
          <form
            className="settings-form"
            onSubmit={(event) => {
              event.preventDefault();
              void onSaveVision();
            }}
          >
            <ModelSelect
              label={t("settings.visionDefaultModel")}
              options={visionModels}
              value={visionDefault}
              onChange={setVisionDefault}
              allowEmpty
            />
            <p className="settings-hint">{t("settings.visionAutoDiscoverHint")}</p>
            <div className="settings-actions">
              <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                {runtimeConfigSaving ? t("settings.saving") : t("settings.saveVision")}
              </Button>
              {visionDefault ? (
                <StatusChip
                  className={`settings-status ${visionProviderReady ? "available" : "unavailable"}`}
                  tone={visionProviderReady ? "success" : "error"}
                  iconOnly
                  title={visionProviderReady ? t("settings.providerReady") : t("settings.providerCredentialMissing")}
                />
              ) : (
                <StatusChip
                  className="settings-status available"
                  tone="success"
                  iconOnly
                  title={t("settings.autoDiscovery")}
                />
              )}
              {visionDefault && visionProviderReady === false ? (
                <button
                  type="button"
                  className="model-browser-more"
                  onClick={() => onOpenProvider(visionProviderId)}
                >
                  {t("settings.configureProviderCredential", { provider: visionProviderId })} <ArrowRight size={14} />
                </button>
              ) : null}
              {visionSaveMessage ? <span className="settings-save-message">{visionSaveMessage}</span> : null}
            </div>
          </form>
        )}
      </Card>

      <Card className="settings-card settings-primary-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.tabVision")}</span>
            <h2>{t("settings.imageGeneration")}</h2>
          </div>
          {imageGenDirty ? <span className="settings-dirty-pill">{t("settings.unsavedChanges")}</span> : null}
        </div>
        {!surface ? (
          <div className="settings-callout">
            <strong>{t("settings.imageGenConfigUnavailable")}</strong>
            <span>{t("settings.connectLiveVision")}</span>
          </div>
        ) : (
          <form
            className="settings-form"
            onSubmit={(event) => {
              event.preventDefault();
              void onSaveImageGen();
            }}
          >
            <ModelSelect
              label={t("settings.imageGenDefaultModel")}
              options={imageGenModels}
              value={imageGenDefault}
              onChange={setImageGenDefault}
              allowEmpty
            />
            <p className="settings-hint">{t("settings.imageGenAutoDiscoverHint")}</p>
            <div className="settings-actions">
              <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                {runtimeConfigSaving ? t("settings.saving") : t("settings.saveImageGen")}
              </Button>
              {imageGenDefault ? (
                <StatusChip
                  className={`settings-status ${imageGenProviderReady ? "available" : "unavailable"}`}
                  tone={imageGenProviderReady ? "success" : "error"}
                  iconOnly
                  title={imageGenProviderReady ? t("settings.providerReady") : t("settings.providerCredentialMissing")}
                />
              ) : (
                <StatusChip
                  className="settings-status available"
                  tone="success"
                  iconOnly
                  title={t("settings.autoDiscovery")}
                />
              )}
              {imageGenDefault && imageGenProviderReady === false ? (
                <button
                  type="button"
                  className="model-browser-more"
                  onClick={() => onOpenProvider(imageGenProviderId)}
                >
                  {t("settings.configureProviderCredential", { provider: imageGenProviderId })} <ArrowRight size={14} />
                </button>
              ) : null}
              {imageGenSaveMessage ? <span className="settings-save-message">{imageGenSaveMessage}</span> : null}
            </div>
          </form>
        )}
      </Card>
    </div>
  );
}
