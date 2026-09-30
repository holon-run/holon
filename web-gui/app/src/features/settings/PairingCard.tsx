import { useEffect, useState } from "react";
import QRCode from "qrcode";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { useCopyText } from "../../components/ClipboardProvider";
import { resolveRuntimeApiBase } from "../../runtime/client";
import { getRuntimeConnectionConfig } from "../../runtime/runtime-store";
import type { RuntimeConnection } from "../../runtime/types";
import { tailscaleServeUrl } from "./TailscaleServeCard";

export function pairingOrigin(value: string): string | undefined {
  try {
    const url = new URL(value.trim());
    const host = url.hostname.toLowerCase().replace(/\.$/, "");
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password
      || host === "localhost" || host.endsWith(".localhost")
      || host.startsWith("127.") || host === "0.0.0.0" || host === "[::1]" || host === "[::]"
      || host.startsWith("[::ffff:7f") || host === "[::ffff:0:0]") return undefined;
    if (url.pathname !== "/" || url.search || url.hash) return undefined;
    return url.origin;
  } catch {
    return undefined;
  }
}

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
  const [customTarget, setCustomTarget] = useState("");
  const [serveTarget, setServeTarget] = useState<string>();
  const runtimeOrigin = connection.mode === "remote" && connection.baseUrl
    ? new URL(connection.baseUrl, window.location.origin).origin : window.location.origin;
  const directTarget = pairingOrigin(runtimeOrigin);
  const target = customTarget ? pairingOrigin(customTarget) : directTarget ?? serveTarget;

  async function discoverTarget(signal?: AbortSignal): Promise<string | undefined> {
    const token = getRuntimeConnectionConfig().token;
    const response = await fetch(tailscaleServeUrl(connection), {
      credentials: "include",
      headers: token ? { Authorization: `Bearer ${token}` } : {},
      signal,
    });
    if (!response.ok) return undefined;
    const status: { serving: boolean; serve_url?: string } = await response.json();
    return status.serving && status.serve_url ? pairingOrigin(status.serve_url) : undefined;
  }

  useEffect(() => {
    setPairing(undefined);
    setCustomTarget("");
    setServeTarget(undefined);
    if (directTarget || connection.source !== "http") return;
    const controller = new AbortController();
    void discoverTarget(controller.signal).then((origin) => {
      if (!controller.signal.aborted) setServeTarget(origin);
    }).catch(() => {});
    return () => controller.abort();
  }, [runtimeOrigin, connection]);

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
      const origin = customTarget ? pairingOrigin(customTarget)
        : directTarget ?? await discoverTarget().catch(() => undefined);
      if (!origin) throw new Error(t("settings.pairing.addressRequired"));
      if (!customTarget && !directTarget) setServeTarget(origin);
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
      const link = pairingLink(origin, result.ticket);
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
      <label>
        {t("settings.pairing.address")}
        <input
          type="url"
          value={customTarget}
          placeholder={target ?? "http://192.168.1.10:7878"}
          disabled={busy}
          onChange={(event) => {
            setCustomTarget(event.currentTarget.value);
            setPairing(undefined);
            setCopied(false);
            setError("");
          }}
        />
      </label>
      <p className="settings-muted">{t("settings.pairing.addressHint")}</p>
      {target ? <p className="settings-muted">{t("settings.pairing.target", { target })}</p> : null}
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
