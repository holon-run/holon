import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import type { RuntimeConnection } from "../../runtime/types";

import type { ServeState } from "./useServeStatus";
export { tailscaleServeUrl } from "./useServeStatus";

export function TailscaleServeCard({ connection, serve }: { connection: RuntimeConnection; serve: ServeState }) {
  const { t } = useTranslation();
  const { status, error, busy } = serve;
  async function change(action: "enable" | "disable") {
    if (action === "enable" && !window.confirm(t("settings.serve.confirm"))) return;
    await serve.request(action);
  }

  const state = !status?.available || (status.connected && !status.status_known) ? "unavailable"
    : !status.connected ? "stopped"
    : status.conflict ? "conflict"
    : status.serving ? "serving" : "connected";
  const drifted = status?.status_known && status.desired_enabled !== status.serving;
  const action = status?.serving ? "disable" : "enable";
  return (
    <Card className="settings-card settings-primary-card">
      <div className="settings-card-head">
        <div>
          <span className="eyebrow">{t("settings.serve.title")}</span>
          <h2>{t("settings.serve.title")}</h2>
        </div>
        {status ? (
          <div className="settings-card-actions">
            <Button disabled={busy || !status.status_known || status.conflict} onClick={() => void change(action)}>
              {busy ? t("settings.serve.working") : drifted && status.desired_enabled ? t("settings.serve.restore") : action === "disable" ? t("settings.serve.disable") : t("settings.serve.enable")}
            </Button>
            {status.desired_enabled && !status.serving && status.status_known ? (
              <Button variant="secondary" disabled={busy || status.conflict} onClick={() => void change("disable")}>
                {t("settings.serve.turnOffDesired")}
              </Button>
            ) : null}
          </div>
        ) : null}
      </div>
      <p className="settings-muted">{t("settings.serve.warning")}</p>
      {connection.source !== "http" ? <p>{t("settings.serve.httpOnly")}</p> : null}
      {status ? (
        <div>
          <p>{t("settings.serve.desired")}: {t(status.desired_enabled ? "settings.serve.on" : "settings.serve.off")}</p>
          <p>{t("settings.serve.state")}: {t(`settings.serve.states.${state}`)}</p>
          {status.hostname ? <p>{t("settings.serve.hostname")}: {status.hostname}</p> : null}
          {status.serve_url ? <p>{t("settings.serve.url")}: {status.serve_url}</p> : null}
          <p>{status.message}</p>
          {status.control_authentication_available === false ? <p>{t("settings.pairing.authenticationRequired")}</p> : null}
          {drifted ? <p role="alert">{t("settings.serve.drift")}</p> : null}
        </div>
      ) : null}
      {error ? <div className="settings-error-banner" role="alert">{t("settings.serve.loadError")}: {error}</div> : null}
      {connection.source === "http" ? (
        <div className="settings-card-actions">
          <Button variant="secondary" disabled={busy} onClick={() => void serve.request()}>{t("settings.serve.refresh")}</Button>
        </div>
      ) : null}
    </Card>
  );
}
