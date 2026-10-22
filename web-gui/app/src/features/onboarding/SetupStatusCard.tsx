import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import type { RuntimeConfigState } from "../../runtime/types";

export function setupIsReady(config: RuntimeConfigState): boolean {
  const reload = config.reload;
  return !config.error && config.setup?.status === "ready"
    && (!reload || (reload.state !== "failed" && reload.state !== "applying"
      && reload.completedGeneration >= reload.requestedGeneration));
}

export function SetupStatusCard({
  config, loading, saving, onRepair, onRetry, onStart,
}: {
  config: RuntimeConfigState;
  loading?: boolean;
  saving?: boolean;
  onRepair: (action: string, provider: string | null) => void;
  onRetry: () => void;
  onStart?: () => void;
}) {
  const { t } = useTranslation();
  const reload = config.reload;
  const failed = reload?.state === "failed";
  const applying = saving || reload?.state === "applying"
    || (reload && reload.state !== "failed" && reload.completedGeneration < reload.requestedGeneration);
  const unknown = Boolean(config.error) || !config.setup;
  const ready = !loading && !applying && setupIsReady(config);
  const action = config.setup?.actions[0] ?? "configure_model_provider";
  const reason = action === "configure_model_credential" ? "credentials" : "model";
  const state = failed ? "failed" : applying ? "applying" : loading ? "loading"
    : unknown ? "unknown" : ready ? "ready" : "blocked";
  return (
    <section className="settings-callout setup-status-card" aria-label={t("setup.title")}>
      <div role="status" aria-live="polite" aria-atomic="true">
        <strong>{t(`setup.${state}`)}</strong>
        {config.setup?.defaultModel ? <p>{t("setup.defaultModel", { model: config.setup.defaultModel })}</p> : null}
        {state === "blocked" ? <p>{t(`setup.${reason}Reason`)}</p> : null}
        {ready ? <p>{t("setup.notTested")}</p> : null}
      </div>
      {applying && !saving ? <Button type="button" variant="outline" onClick={onRetry}>{t("setup.retry")}</Button> : null}
      {!loading && !applying ? (
        <div>
          {failed || unknown ? (
            <Button type="button" variant="outline" onClick={onRetry}>{t("setup.retry")}</Button>
          ) : ready ? (
            onStart ? <Button type="button" variant="accent" onClick={onStart}>{t("onboarding.title")}</Button> : null
          ) : (
            <Button type="button" variant="accent" onClick={() => onRepair(action, config.setup?.provider ?? null)}>
              {t(`setup.${reason}Action`)}
            </Button>
          )}
        </div>
      ) : null}
    </section>
  );
}
