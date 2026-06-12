import type { RuntimeConnection, RuntimeModelCatalog, RuntimeModelOption } from "../../runtime/types";

interface SettingsPageProps {
  connection: RuntimeConnection;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  modelCatalogError?: string;
  onRefreshModels: () => Promise<void>;
}

export function SettingsPage({
  connection,
  modelCatalog,
  modelCatalogLoading,
  modelCatalogError,
  onRefreshModels,
}: SettingsPageProps) {
  const groupedModels = groupModelsByProvider(modelCatalog.options);
  const availableCount = modelCatalog.options.filter((model) => model.available).length;
  const unavailableCount = modelCatalog.options.length - availableCount;

  return (
    <section className="page settings-page" aria-label="Settings">
      <div className="page-inner settings-inner">
        <section className="summary-panel settings-hero">
          <span className="eyebrow">Runtime configuration</span>
          <h1>Settings</h1>
          <p>
            Read-only runtime settings and diagnostics for the current Web GUI session. Agent-specific model
            changes still happen from each agent page and apply to the next run when needed.
          </p>
        </section>

        <div className="settings-grid">
          <section className="settings-card">
            <div className="settings-card-head">
              <div>
                <span className="eyebrow">Connection</span>
                <h2>Runtime API</h2>
              </div>
              <span className={`source-chip ${connection.source === "http" ? "live" : "preview"}`}>
                {connection.source === "http" ? "live" : "preview"}
              </span>
            </div>
            <dl className="settings-list">
              <div>
                <dt>Mode</dt>
                <dd>{connection.mode}</dd>
              </div>
              <div>
                <dt>API base</dt>
                <dd>{connection.baseUrl ?? "not configured"}</dd>
              </div>
              <div>
                <dt>Status</dt>
                <dd>{connection.summary}</dd>
              </div>
              {connection.error ? (
                <div>
                  <dt>Error</dt>
                  <dd className="settings-error">{connection.error}</dd>
                </div>
              ) : null}
            </dl>
          </section>

          <section className="settings-card">
            <div className="settings-card-head">
              <div>
                <span className="eyebrow">Runtime defaults</span>
                <h2>Model posture</h2>
              </div>
            </div>
            <div className="settings-callout">
              <strong>Read-only in this page</strong>
              <span>
                Provider availability and runtime defaults are reported by the backend. Use the model picker on an
                agent page to set or clear that agent's override.
              </span>
            </div>
            <dl className="settings-list compact">
              <div>
                <dt>Catalog source</dt>
                <dd>{modelCatalog.source}</dd>
              </div>
              <div>
                <dt>Available models</dt>
                <dd>{availableCount}</dd>
              </div>
              <div>
                <dt>Unavailable models</dt>
                <dd>{unavailableCount}</dd>
              </div>
            </dl>
          </section>
        </div>

        <section className="settings-card settings-models">
          <div className="settings-card-head">
            <div>
              <span className="eyebrow">Models / Providers</span>
              <h2>Model catalog</h2>
            </div>
            <button type="button" disabled={modelCatalogLoading} onClick={() => void onRefreshModels()}>
              {modelCatalogLoading ? "Refreshing…" : "Refresh"}
            </button>
          </div>

          {modelCatalogError ? <div className="settings-error-banner">{modelCatalogError}</div> : null}
          {!modelCatalogLoading && modelCatalog.options.length === 0 ? (
            <div className="settings-empty">No models returned by the runtime yet.</div>
          ) : null}

          <div className="provider-list">
            {groupedModels.map(([provider, models]) => (
              <article className="provider-card" key={provider}>
                <header>
                  <div>
                    <h3>{provider}</h3>
                    <span>
                      {models.filter((model) => model.available).length}/{models.length} available
                    </span>
                  </div>
                </header>
                <div className="model-table" role="table" aria-label={`${provider} models`}>
                  {models.map((model) => (
                    <div className="model-table-row" role="row" key={model.model}>
                      <div role="cell">
                        <strong>{model.displayName}</strong>
                        <span>{model.model}</span>
                      </div>
                      <div role="cell">
                        <span className={`settings-status ${model.available ? "available" : "unavailable"}`}>
                          {model.available ? "available" : "unavailable"}
                        </span>
                      </div>
                      <div role="cell">
                        {model.supportsReasoningEffort ? <span className="settings-pill">reasoning</span> : null}
                        {model.unavailableReason ? <small>{model.unavailableReason}</small> : null}
                      </div>
                    </div>
                  ))}
                </div>
              </article>
            ))}
          </div>
        </section>
      </div>
    </section>
  );
}

function groupModelsByProvider(options: RuntimeModelOption[]): Array<[string, RuntimeModelOption[]]> {
  const grouped = new Map<string, RuntimeModelOption[]>();
  for (const option of options) {
    const models = grouped.get(option.provider) ?? [];
    models.push(option);
    grouped.set(option.provider, models);
  }
  return Array.from(grouped.entries()).sort(([left], [right]) => left.localeCompare(right));
}
