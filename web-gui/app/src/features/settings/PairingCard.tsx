import { useEffect, useRef, useState } from "react";
import QRCode from "qrcode";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { useCopyText } from "../../components/ClipboardProvider";
import { resolveRuntimeApiBase } from "../../runtime/client";
import { getRuntimeConnectionConfig } from "../../runtime/runtime-store";
import type { RuntimeConnection } from "../../runtime/types";
import type { ServeState, ServeStatus } from "./useServeStatus";

export function pairingOrigin(value: string): string | undefined {
  try {
    if (!/^https?:\/\/[^/?#]+\/?$/i.test(value.trim())) return undefined;
    const url = new URL(value.trim());
    const host = url.hostname.toLowerCase().replace(/\.$/, "");
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password
      || host === "localhost" || host.endsWith(".localhost")
      || /^127\.\d+\.\d+\.\d+$/.test(host) || host === "0.0.0.0" || host === "[::1]" || host === "[::]"
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

export function validServeOrigin(status?: ServeStatus): string | undefined {
  if (!status?.available || !status.connected || !status.status_known || !status.serving || status.conflict
    || !status.serve_url || !status.hostname) return undefined;
  const origin = pairingOrigin(status.serve_url);
  if (!origin) return undefined;
  const url = new URL(origin);
  return url.protocol === "https:" && url.hostname.toLowerCase() === status.hostname.toLowerCase()
    ? origin : undefined;
}

export function PairingCard({ connection, serve }: { connection: RuntimeConnection; serve: ServeState }) {
  const { t } = useTranslation();
  const copyText = useCopyText();
  const [pairing, setPairing] = useState<{ link: string; expiresAt: number; qr: string; identity: string }>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  // Undefined follows the automatic destination; an empty edit stays empty.
  const [customTarget, setCustomTarget] = useState<string>();
  const [authMode, setAuthMode] = useState<{ key: string; mode: string }>();
  const [authRetry, setAuthRetry] = useState(0);
  const [authError, setAuthError] = useState(false);
  const [authBusy, setAuthBusy] = useState(false);
  const runtimeBase = resolveRuntimeApiBase(connection);
  const runtimeOrigin = runtimeBase ? new URL(runtimeBase, window.location.origin).origin : window.location.origin;
  // A subpath API base is not a supported login destination.
  const basePath = runtimeBase ? new URL(runtimeBase, window.location.origin).pathname : "/api";
  const directTarget = basePath === "/api" || basePath === "/api/" ? pairingOrigin(runtimeOrigin) : undefined;
  const serveTarget = validServeOrigin(serve.status);
  const automaticTarget = serveTarget ?? directTarget;
  const address = customTarget ?? automaticTarget ?? "";
  const target = customTarget !== undefined ? pairingOrigin(customTarget) : automaticTarget;
  const identity = JSON.stringify([serve.key, serve.revision, serve.status, customTarget, target]);
  const inputs = JSON.stringify([serve.key, customTarget, runtimeOrigin]);
  const currentInputs = useRef(inputs);
  currentInputs.current = inputs;
  const mode = authMode?.key === serve.key ? authMode.mode : undefined;

  useEffect(() => {
    setCustomTarget(undefined);
    setPairing(undefined);
  }, [serve.key, runtimeBase, connection.source]);

  useEffect(() => {
    const controller = new AbortController();
    setAuthMode(undefined);
    setAuthError(false);
    if (connection.source === "http" && runtimeBase) {
      setAuthBusy(true);
      void fetch(`${runtimeBase}/auth/method`, { credentials: "include", signal: controller.signal })
        .then(async (response) => {
          if (!response.ok) throw new Error("Authentication method unavailable");
          const result: { mode: string } = await response.json();
          if (!controller.signal.aborted) setAuthMode({ key: serve.key, mode: result.mode });
        }).catch(() => {
          if (!controller.signal.aborted) setAuthError(true);
        }).finally(() => {
          if (!controller.signal.aborted) setAuthBusy(false);
        });
    }
    return () => controller.abort();
  }, [serve.key, runtimeBase, connection.source, authRetry]);

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
      const issuedKey = serve.key;
      const issuedCustom = customTarget;
      const initialGeneration = serve.sequence.current;
      const methodResponse = await fetch(`${runtimeBase}/auth/method`, { credentials: "include" });
      if (!methodResponse.ok) throw new Error(t("auth.authMethodError"));
      const method: { mode: string } = await methodResponse.json();
      if (currentInputs.current !== inputs || serve.sequence.current !== initialGeneration) return;
      setAuthMode({ key: issuedKey, mode: method.mode });
      if (method.mode === "oidc") return;
      const generation = serve.sequence.current + 1;
      const status = await serve.request();
      if (currentInputs.current !== inputs || serve.sequence.current !== generation) return;
      const origin = issuedCustom !== undefined ? pairingOrigin(issuedCustom) : validServeOrigin(status) ?? directTarget;
      if (!origin) throw new Error(t("settings.pairing.addressRequired"));
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
      if (serve.sequence.current !== generation || currentInputs.current !== inputs) return;
      setPairing({ link, expiresAt, qr,
        identity: JSON.stringify([issuedKey, serve.revision + 1, status, issuedCustom, origin]) });
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
        <Button type="button" disabled={busy || serve.busy || connection.source !== "http" || (!mode || mode === "oidc")} onClick={() => void issue()}>
          {busy ? t("settings.pairing.issuing") : t("settings.pairing.issue")}
        </Button>
      </div>
      <p className="settings-muted">{t("settings.pairing.warning")}</p>
      {authError ? <div className="settings-error-banner" role="alert">
        <p>{t("auth.authMethodError")}</p>
        <Button type="button" disabled={authBusy} onClick={() => setAuthRetry((value) => value + 1)}>
          {t("settings.serve.retry")}
        </Button>
      </div> : null}
      {mode === "oidc" ? <p>{t("settings.pairing.oidc")}</p> : null}
      {!target ? <p>{t("settings.pairing.unavailable")}</p> : null}
      {serveTarget && target === serveTarget ? <p>{t("settings.pairing.tailnet")}</p> : null}
      <div className="settings-form">
        <label>
          {t("settings.pairing.address")}
          <input
            type="url"
            value={address}
            placeholder="http://192.168.1.10:7878"
            autoCapitalize="none"
            spellCheck={false}
            aria-describedby="pairing-address-hint"
            disabled={busy}
            onChange={(event) => {
              setCustomTarget(event.currentTarget.value);
              setPairing(undefined);
              setCopied(false);
              setError("");
            }}
          />
        </label>
      </div>
      <p id="pairing-address-hint" className="settings-muted">{t("settings.pairing.addressHint")}</p>
      {target ? <p className="settings-muted">{t("settings.pairing.target", { target })}</p> : null}
      {error ? <div className="settings-error-banner" role="alert">{error}</div> : null}
      {pairing && pairing.identity === identity && !serve.busy && mode && mode !== "oidc" ? (
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
