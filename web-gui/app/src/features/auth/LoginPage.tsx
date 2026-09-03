import { useEffect, useMemo, useState, type FormEvent } from "react";

export function LoginPage() {
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
        if (!response.ok) throw new Error("Unable to discover authentication method.");
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
      if (!response.ok) throw new Error("The token could not be exchanged.");
      window.location.replace(returnTo || "/");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Login failed.");
    } finally {
      setBusy(false);
    }
  }

  const oidcStart = `/api/auth/oidc/start?return_to=${encodeURIComponent(returnTo || "/")}`;
  return (
    <main className="login-page">
      <section className="login-card" aria-labelledby="login-title">
        <h1 id="login-title">Sign in to Holon</h1>
        <p>
          {oidc === false
            ? "Enter a static token to create a session."
            : "Continue with organization login."}
        </p>
        {oidc === true ? (
          <a className="button button-primary" href={oidcStart}>
            Continue with organization login
          </a>
        ) : null}
        {oidc === false ? (
          <form onSubmit={submit}>
            <label htmlFor="login-token">Static token</label>
            <input
              id="login-token"
              type="password"
              value={token}
              onChange={(event) => setToken(event.target.value)}
              autoComplete="current-password"
            />
            <button className="button" type="submit" disabled={busy || !token.trim()}>
              {busy ? "Signing in…" : "Sign in with token"}
            </button>
            {error ? <p role="alert">{error}</p> : null}
          </form>
        ) : null}
      </section>
    </main>
  );
}
