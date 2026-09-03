import { useEffect, useMemo, useState, type FormEvent } from "react";
import { KeyRound } from "lucide-react";
import { useTranslation } from "react-i18next";

import holonMarkUrl from "../../assets/holon-mark.png";
import { Button } from "../../components/ui/Button";

export function LoginPage() {
  const { t } = useTranslation();
  const [token, setToken] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [oidc, setOidc] = useState<boolean>();
  const returnTo = useMemo(() => {
    const requested = new URLSearchParams(window.location.search).get("return_to");
    return requested?.startsWith("/") && !requested.startsWith("//")
      ? requested
      : window.location.pathname === "/login"
        ? "/"
        : `${window.location.pathname}${window.location.search}${window.location.hash}`;
  }, []);

  useEffect(() => {
    void fetch("/api/auth/method", {
      credentials: "include",
      headers: { Accept: "application/json" },
    })
      .then(async (response) => {
        if (!response.ok) throw new Error(t("auth.authMethodError"));
        const body = (await response.json()) as { mode?: string };
        setOidc(body.mode === "oidc");
      })
      .catch(() => setOidc(true));
  }, []);

  useEffect(() => {
    if (oidc !== true) return;
    window.location.replace(oidcStart);
  }, [oidc]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      const response = await fetch("/api/auth/session/exchange", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ credential: token }),
      });
      if (!response.ok) throw new Error(t("auth.tokenExchangeError"));
      window.location.replace(returnTo || "/");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("auth.loginFailed"));
    } finally {
      setBusy(false);
    }
  }

  const oidcStart = `/api/auth/oidc/start?return_to=${encodeURIComponent(returnTo || "/")}`;
  const description =
    oidc === false
      ? t("auth.tokenDescription")
      : oidc === true
        ? t("auth.oidcDescription")
        : t("auth.checkingMethod");

  return (
    <main className="login-page">
      <div className="login-shell">
        <div className="login-brand" aria-label="Holon">
          <img src={holonMarkUrl} alt="" />
          <span>Holon</span>
        </div>
        <section className="login-card" aria-labelledby="login-title">
          <div className="login-card-header">
            <span className="login-card-icon" aria-hidden="true">
              <KeyRound size={20} strokeWidth={2} />
            </span>
            <div>
              <p className="login-eyebrow">{t("auth.runtimeAccess")}</p>
              <h1 id="login-title">{t("auth.signInTitle")}</h1>
              <p className="login-description">{description}</p>
            </div>
          </div>
          {oidc === true ? (
            <a className="login-action" href={oidcStart}>
              {t("auth.organizationLogin")}
            </a>
          ) : null}
          {oidc === false ? (
            <form className="login-form" onSubmit={submit}>
              <div className="login-field">
                <label htmlFor="login-token">{t("auth.staticToken")}</label>
                <div className="login-input">
                  <KeyRound size={17} aria-hidden="true" />
                  <input
                    id="login-token"
                    type="password"
                    value={token}
                    onChange={(event) => setToken(event.target.value)}
                    placeholder={t("auth.pasteToken")}
                    autoComplete="current-password"
                    aria-describedby="login-token-hint"
                    autoFocus
                  />
                </div>
                <p id="login-token-hint">{t("auth.tokenHint")}</p>
              </div>
              <Button className="login-submit" variant="accent" type="submit" disabled={busy || !token.trim()}>
                {busy ? t("auth.signingIn") : t("auth.signInWithToken")}
              </Button>
              {error ? (
                <p className="login-error" role="alert">
                  {error}
                </p>
              ) : null}
            </form>
          ) : null}
        </section>
      </div>
    </main>
  );
}
