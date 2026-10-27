import { useTranslation } from "react-i18next";
import type { ProviderCooldownWait } from "../../runtime/types";
import { Button } from "../../components/ui/Button";

export function ProviderCooldownNotice({
  wait,
  onStop,
  onInterject,
  stopping = false,
}: {
  wait?: ProviderCooldownWait | null;
  onStop: () => void;
  onInterject: () => void;
  stopping?: boolean;
}) {
  const { t, i18n } = useTranslation();
  if (!wait) return null;
  const cause = wait.cause ?? {};
  const reason = (() => {
    switch (cause.failure_kind) {
      case "rate_limited": return t("providerCooldown.rateLimited");
      case "server_error": return t("providerCooldown.serverError");
      case "timeout": return t("providerCooldown.timeout");
      case "connection":
      case "transport": return t("providerCooldown.transport");
      default: return t("providerCooldown.unavailable");
    }
  })();
  const date = new Date(wait.not_before);
  const deadline = Number.isNaN(date.getTime()) ? wait.not_before : date.toLocaleString(i18n.language);
  return (
    <section className="composer-status provider-cooldown-notice" role="status" aria-label={t("providerCooldown.title")}>
      <strong>{t("providerCooldown.title")}</strong>
      <p>{reason}{cause.http_status != null ? ` (HTTP ${cause.http_status})` : ""}</p>
      <p>{t("providerCooldown.earliestRetry")} <time dateTime={wait.not_before}>{deadline}</time></p>
      <p>{t("providerCooldown.notGuaranteed")}</p>
      {cause.retry_after ? <p>{t(cause.retry_after.source === "server_header" ? "providerCooldown.serverHeader" : "providerCooldown.localBackoff")}</p> : null}
      {cause.detail_unavailable_reason ? <p>{t("providerCooldown.unavailable")}</p> : null}
      {cause.incident_id ? <p>{t("providerCooldown.incident", { id: cause.incident_id })}</p> : null}
      <div>
        <Button variant="secondary" size="sm" onClick={onStop} disabled={stopping}>{t("agent.stopCurrentTurn")}</Button>
        <Button variant="secondary" size="sm" onClick={onInterject}>{t("providerCooldown.interject")}</Button>
      </div>
    </section>
  );
}
