import { useTranslation } from "react-i18next";

import { Button } from "../../../components/ui/Button";
import { Card } from "../../../components/ui/Card";
import { useI18nSettings } from "../../../i18n";
import { LANGUAGE_MODE_OPTIONS } from "../../../i18n/types";
import type { RuntimeConfigState, RuntimeConnection } from "../../../runtime/types";
import { SystemNotificationSettingsCard } from "../../notifications/NotificationCenter";
import { runtimeReloadMessage } from "../settings-helpers";

interface GeneralSettingsSectionProps {
  connection: RuntimeConnection;
  runtimeConfig: RuntimeConfigState;
  runtimeConfigLoading: boolean;
  runtimeConfigError?: string;
  onRefreshRuntimeConfig: () => Promise<void>;
}

export function GeneralSettingsSection({
  connection,
  runtimeConfig,
  runtimeConfigLoading,
  runtimeConfigError,
  onRefreshRuntimeConfig,
}: GeneralSettingsSectionProps) {
  const { t } = useTranslation();
  const { languageMode, resolvedLanguageLabel, setLanguageMode } = useI18nSettings();
  const reloadMessage = runtimeReloadMessage(runtimeConfig);

  return (
    <div className="settings-section-stack">
      <Card className="settings-card settings-primary-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.tabGeneral")}</span>
            <h2>{t("settings.language.label")}</h2>
          </div>
        </div>
        <p className="settings-muted">{t("settings.language.description")}</p>
        <div className="settings-form-row">
          <label>
            <span>{t("settings.language.label")}</span>
            <select
              value={languageMode}
              onChange={(event) => setLanguageMode(event.target.value as typeof languageMode)}
            >
              {LANGUAGE_MODE_OPTIONS.map((mode) => (
                <option key={mode} value={mode}>
                  {mode === "system"
                    ? t("settings.language.system")
                    : mode === "en"
                      ? t("settings.language.english")
                      : t("settings.language.chineseSimplified")}
                </option>
              ))}
            </select>
          </label>
        </div>
        {languageMode === "system" ? (
          <p className="settings-muted">
            {t("settings.language.systemResolved", { language: resolvedLanguageLabel })}
          </p>
        ) : null}
      </Card>

      <SystemNotificationSettingsCard />

      <Card className="settings-card settings-primary-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.connection")}</span>
            <h2>{t("settings.runtimeNodeHeading")}</h2>
          </div>
          <Button
            type="button"
            variant="secondary"
            disabled={runtimeConfigLoading}
            onClick={() => void onRefreshRuntimeConfig()}
          >
            {runtimeConfigLoading ? t("settings.refreshing") : t("common.refresh")}
          </Button>
        </div>
        {runtimeConfigError ? <div className="settings-error-banner">{runtimeConfigError}</div> : null}
        {reloadMessage ? (
          <div className={runtimeConfig.reload?.state === "failed" ? "settings-error-banner" : "settings-callout"}>
            {reloadMessage}
          </div>
        ) : null}
        <dl className="settings-list compact">
          <div>
            <dt>{t("settings.connection")}</dt>
            <dd>{connection.source === "http" ? t("settings.liveRuntime") : t("settings.previewData")}</dd>
          </div>
          <div>
            <dt>{t("settings.apiBase")}</dt>
            <dd><code>{connection.baseUrl ?? window.location.origin}</code></dd>
          </div>
          <div>
            <dt>{t("settings.configFile")}</dt>
            <dd><code>{runtimeConfig.configFilePath ?? t("settings.notReported")}</code></dd>
          </div>
        </dl>
      </Card>
    </div>
  );
}
