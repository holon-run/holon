import { useEffect, useState } from "react";
import QRCode from "qrcode";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { useCopyText } from "../../components/ClipboardProvider";
import { resolveRuntimeApiBase } from "../../runtime/client";
import { getRuntimeConnectionConfig } from "../../runtime/runtime-store";
import type { RuntimeConnection } from "../../runtime/types";

export function pairingLink(origin: string, ticket: string): string {
  const url = new URL("/login", origin);
  url.hash = new URLSearchParams({ pair: ticket }).toString();
  return url.toString();
}

export function pairingIssueUrl(connection: RuntimeConnection): string {
  const base = resolveRuntimeApiBase(connection);
  if (!base) throw new Error("Runtime API base is unavailable");
  return `${base}/auth/pairing/issue`;
}

export function PairingCard({ connection }: { connection: RuntimeConnection }) {
  const { t } = useTranslation();
  const copyText = useCopyText();
  const [pairing, setPairing] = useState<{ link: string; expiresAt: number; qr: string }>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const target = connection.mode === "remote" && connection.baseUrl
    ? new URL(connection.baseUrl, window.location.origin).origin : window.location.origin;

  useEffect(() => {
    if (!pairing) return;
    const timer = window.setTimeout(() => setPairing(undefined), Math.max(0, pairing.expiresAt - Date.now()));
    return () => window.clearTimeout(timer);
  }, [pairing]);

  async function issue() {
    setPairing(undefined);
    setCopied(false);
    setError("");
    setBusy(true);
    try {
      const token = getRuntimeConnectionConfig().token;
      const response = await fetch(pairingIssueUrl(connection), {
        method: "POST",
        credentials: "include",
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      if (!response.ok) throw new Error(t("settings.pairing.issueError"));
      const result: { ticket: string; expires_at: string } = await response.json();
      const expiresAt = Date.parse(result.expires_at);
      if (!result.ticket || !Number.isFinite(expiresAt) || expiresAt <= Date.now()) {
        throw new Error(t("settings.pairing.issueError"));
      }
      const link = pairingLink(target, result.ticket);
      const qr = await QRCode.toDataURL(link, { margin: 2, width: 220 });
      setPairing({ link, expiresAt, qr });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("settings.pairing.issueError"));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="settings-card settings-primary-card">
      <div className="settings-card-head">
        <div>
          <span className="eyebrow">{t("settings.pairing.title")}</span>
          <h2>{t("settings.pairing.title")}</h2>
        </div>
        <Button type="button" disabled={busy || connection.source !== "http"} onClick={() => void issue()}>
          {busy ? t("settings.pairing.issuing") : t("settings.pairing.issue")}
        </Button>
      </div>
      <p className="settings-muted">{t("settings.pairing.warning")}</p>
      <p className="settings-muted">{t("settings.pairing.target", { target })}</p>
      {error ? <div className="settings-error-banner" role="alert">{error}</div> : null}
      {pairing ? (
        <div>
          <img src={pairing.qr} alt={t("settings.pairing.qrAlt")} width={220} height={220} />
          <p>{t("settings.pairing.expires", { time: new Date(pairing.expiresAt).toLocaleTimeString() })}</p>
          <input aria-label={t("settings.pairing.link")} readOnly value={pairing.link} onFocus={(event) => event.currentTarget.select()} />
          <Button type="button" variant="secondary" onClick={() => {
            void copyText(pairing.link).then(setCopied);
          }}>{copied ? t("clipboard.copied") : t("settings.pairing.copy")}</Button>
        </div>
      ) : null}
    </Card>
  );
}
