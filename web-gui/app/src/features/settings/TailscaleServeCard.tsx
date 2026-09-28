import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { getRuntimeConnectionConfig } from "../../runtime/runtime-store";
import type { RuntimeConnection } from "../../runtime/types";

const endpoint = "/api/control/network/tailscale/serve";

type ServeStatus = {
  desired_enabled: boolean;
  available: boolean;
  connected: boolean;
  status_known: boolean;
  serving: boolean;
  conflict: boolean;
  hostname?: string;
  serve_url?: string;
  message: string;
};

function request(path: string, method = "GET", signal?: AbortSignal) {
  const token = getRuntimeConnectionConfig().token;
  return fetch(`${endpoint}${path}`, {
    method,
    credentials: "include",
    headers: token ? { Authorization: `Bearer ${token}` } : {},
    signal,
  });
}

export function TailscaleServeCard({ connection }: { connection: RuntimeConnection }) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<ServeStatus>();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    setStatus(undefined);
    setError("");
    if (connection.source !== "http") return;
    const controller = new AbortController();
    void request("", "GET", controller.signal)
      .then(async (response) => {
        if (!response.ok) throw new Error(t("settings.serve.loadError"));
        const result: ServeStatus = await response.json();
        if (!controller.signal.aborted) setStatus(result);
      })
      .catch(() => {
        if (!controller.signal.aborted) setError(t("settings.serve.loadError"));
      });
    return () => controller.abort();
  }, [connection.source, t, reload]);

  async function change(action: "enable" | "disable") {
    if (action === "enable" && !window.confirm(t("settings.serve.confirm"))) return;
    setBusy(true);
    setError("");
    try {
      const response = await request(`/${action}`, "POST");
      if (!response.ok) {
        const body: { error?: string } = await response.json().catch(() => ({}));
        throw new Error(body.error || t("settings.serve.actionError"));
      }
      const result: ServeStatus = await response.json();
      setStatus(result);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("settings.serve.actionError"));
    } finally {
      setBusy(false);
    }
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
          {drifted ? <p role="alert">{t("settings.serve.drift")}</p> : null}
        </div>
      ) : null}
      {error ? <div className="settings-error-banner" role="alert">{error}</div> : null}
      {connection.source === "http" && !status && error ? (
        <Button variant="secondary" onClick={() => setReload((value) => value + 1)}>{t("settings.serve.retry")}</Button>
      ) : null}
    </Card>
  );
}
