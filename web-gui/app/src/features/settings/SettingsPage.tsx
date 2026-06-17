import { useEffect, useMemo, useState } from "react";

import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { StatusChip } from "../../components/ui/StatusChip";
import type { RuntimeConfigState, RuntimeConnection, RuntimeModelCatalog, RuntimeModelOption, RuntimeProviderSummary, CredentialStoreState } from "../../runtime/types";

interface SettingsPageProps {
  connection: RuntimeConnection;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  modelCatalogError?: string;
  runtimeConfig: RuntimeConfigState;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  runtimeConfigError?: string;
  onRefreshModels: () => Promise<void>;
  onRefreshRuntimeConfig: () => Promise<void>;
  onUpdateRuntimeConfig: (updates: Array<{ key: string; value?: unknown; unset?: boolean }>) => Promise<RuntimeConfigState | undefined>;
  credentialStore: CredentialStoreState;
  credentialStoreLoading: boolean;
  onRefreshCredentialStore: () => Promise<void>;
  onSetCredential: (profile: string, kind: string, material: string) => Promise<unknown>;
  onDeleteCredential: (profile: string) => Promise<void>;
}

function splitCsv(value: string): string[] {
  return value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

function numberFromInput(value: string): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
}

type ProviderDraft = Pick<
  RuntimeProviderSummary,
  "transport" | "baseUrl" | "credentialSource" | "credentialKind" | "credentialEnv" | "credentialProfile" | "credentialExternal"
>;

const credentialSources = ["env", "credential_profile", "external_cli", "credential_process", "none"];
const credentialKinds = ["api_key", "bearer_token", "oauth", "session_token", "aws_sdk", "none"];

export function SettingsPage({
  connection,
  modelCatalog,
  modelCatalogLoading,
  modelCatalogError,
  runtimeConfig,
  runtimeConfigLoading,
  runtimeConfigSaving,
  runtimeConfigError,
  onRefreshModels,
  onRefreshRuntimeConfig,
  onUpdateRuntimeConfig,
  credentialStore,
  credentialStoreLoading,
  onRefreshCredentialStore,
  onSetCredential,
  onDeleteCredential,
}: SettingsPageProps) {
  const groupedModels = groupModelsByProvider(modelCatalog.options);
  const availableCount = modelCatalog.options.filter((model) => model.available).length;
  const unavailableCount = modelCatalog.options.length - availableCount;
  const surface = runtimeConfig.surface;
  const [modelDefault, setModelDefault] = useState("");
  const [modelFallbacks, setModelFallbacks] = useState("");
  const [visionDefault, setVisionDefault] = useState("");
  const [runtimeMaxOutputTokens, setRuntimeMaxOutputTokens] = useState("");
  const [defaultToolOutputTokens, setDefaultToolOutputTokens] = useState("");
  const [maxToolOutputTokens, setMaxToolOutputTokens] = useState("");
  const [disableProviderFallback, setDisableProviderFallback] = useState(false);
  const [searchEnabled, setSearchEnabled] = useState(true);
  const [searchBuiltinProviderEnabled, setSearchBuiltinProviderEnabled] = useState(true);
  const [searchProvider, setSearchProvider] = useState("auto");
  const [searchMode, setSearchMode] = useState<"single" | "fallback" | "aggregate">("fallback");
  const [searchProviders, setSearchProviders] = useState("");
  const [searchMaxResults, setSearchMaxResults] = useState("");
  const [searchMaxProviderAttempts, setSearchMaxProviderAttempts] = useState("");
  const [providerDrafts, setProviderDrafts] = useState<Record<string, ProviderDraft>>({});
  const [saveMessage, setSaveMessage] = useState<string | undefined>();
  const [searchSaveMessage, setSearchSaveMessage] = useState<string | undefined>();
  const [visionSaveMessage, setVisionSaveMessage] = useState<string | undefined>();
  const [providerSaveMessage, setProviderSaveMessage] = useState<string | undefined>();
  const [apiKeyDrafts, setApiKeyDrafts] = useState<Record<string, string>>({});
  const [credentialMessages, setCredentialMessages] = useState<Record<string, string>>({});
  const availableModels = useMemo(() => modelCatalog.options.filter((model) => model.available), [modelCatalog.options]);
  const visionModels = useMemo(() => modelCatalog.options.filter((model) => model.available && model.supportsImageInput), [modelCatalog.options]);

  useEffect(() => {
    if (!surface) return;
    setModelDefault(surface.modelDefault);
    setModelFallbacks(surface.modelFallbacks.join(", "));
    setVisionDefault(surface.visionDefault ?? "");
    setRuntimeMaxOutputTokens(String(surface.runtimeMaxOutputTokens));
    setDefaultToolOutputTokens(String(surface.defaultToolOutputTokens));
    setMaxToolOutputTokens(String(surface.maxToolOutputTokens));
    setDisableProviderFallback(surface.disableProviderFallback);
    if (surface.webSearch) {
      setSearchEnabled(surface.webSearch.enabled);
      setSearchBuiltinProviderEnabled(surface.webSearch.builtinProviderEnabled);
      setSearchProvider(surface.webSearch.provider);
      setSearchMode(surface.webSearch.mode);
      setSearchProviders(surface.webSearch.providers.join(", "));
      setSearchMaxResults(String(surface.webSearch.maxResults));
      setSearchMaxProviderAttempts(String(surface.webSearch.maxProviderAttempts));
    }
    setProviderDrafts(
      Object.fromEntries(
        surface.providers.map((provider) => [
          provider.id,
          {
            transport: provider.transport,
            baseUrl: provider.baseUrl,
            credentialSource: provider.credentialSource,
            credentialKind: provider.credentialKind,
            credentialEnv: provider.credentialEnv ?? "",
            credentialProfile: provider.credentialProfile ?? "",
            credentialExternal: provider.credentialExternal ?? "",
          },
        ]),
      ),
    );
    setSaveMessage(undefined);
    setSearchSaveMessage(undefined);
    setVisionSaveMessage(undefined);
    setProviderSaveMessage(undefined);
  }, [surface]);

  useEffect(() => {
    void onRefreshCredentialStore();
  }, [onRefreshCredentialStore]);

  function isCredentialProfileConfigured(profile: string): boolean {
    return credentialStore.profiles.some((p) => p.profile === profile && p.configured);
  }

  async function saveApiKey(providerId: string, credentialProfile: string, credentialKind: string) {
    const key = apiKeyDrafts[providerId]?.trim();
    if (!key || !credentialProfile) return;
    setCredentialMessages((prev) => ({ ...prev, [providerId]: "Saving…" }));
    const result = await onSetCredential(credentialProfile, credentialKind, key);
    if (result) {
      setCredentialMessages((prev) => ({ ...prev, [providerId]: "API key saved to credential store." }));
      setApiKeyDrafts((prev) => ({ ...prev, [providerId]: "" }));
    } else {
      setCredentialMessages((prev) => ({ ...prev, [providerId]: "Failed to save API key." }));
    }
  }

  async function removeApiKey(providerId: string, credentialProfile: string) {
    if (!credentialProfile) return;
    setCredentialMessages((prev) => ({ ...prev, [providerId]: "Removing…" }));
    try {
      await onDeleteCredential(credentialProfile);
      setCredentialMessages((prev) => ({ ...prev, [providerId]: "API key removed from credential store." }));
    } catch {
      setCredentialMessages((prev) => ({ ...prev, [providerId]: "Failed to remove API key." }));
    }
  }

  const rejectedResults = runtimeConfig.results?.filter((result) => result.effect === "rejected") ?? [];
  const configuredProviderCount = surface?.providers.filter((provider) => provider.credentialConfigured).length ?? 0;
  const searchProviderCount = surface?.webSearchProviders.length ?? 0;
  const visionProviderReady = visionDefault ? surface?.providers.find((provider) => provider.id === visionDefault.split("/")[0])?.credentialConfigured : undefined;

  async function saveRuntimeConfig() {
    setSaveMessage(undefined);
    const updates = [
      { key: "model.default", value: modelDefault.trim() },
      { key: "model.fallbacks", value: splitCsv(modelFallbacks) },
      { key: "runtime.max_output_tokens", value: numberFromInput(runtimeMaxOutputTokens) },
      { key: "runtime.default_tool_output_tokens", value: numberFromInput(defaultToolOutputTokens) },
      { key: "runtime.max_tool_output_tokens", value: numberFromInput(maxToolOutputTokens) },
      { key: "runtime.disable_provider_fallback", value: disableProviderFallback },
    ];
    const result = await onUpdateRuntimeConfig(updates);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setSaveMessage(
      rejected.length
        ? `${rejected.length} setting${rejected.length === 1 ? "" : "s"} rejected.`
        : result.changed
          ? "Saved to config.json. Restart the daemon for these runtime defaults to take effect."
          : "No runtime config changes were persisted.",
    );
  }

  async function saveSearchConfig() {
    setSearchSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([
      { key: "web.search.enabled", value: searchEnabled },
      { key: "web.search.builtin_provider.enabled", value: searchBuiltinProviderEnabled },
      { key: "web.search.provider", value: searchProvider.trim() || "auto" },
      { key: "web.search.mode", value: searchMode },
      { key: "web.search.providers", value: splitCsv(searchProviders) },
      { key: "web.search.max_results", value: numberFromInput(searchMaxResults) },
      { key: "web.search.max_provider_attempts", value: numberFromInput(searchMaxProviderAttempts) },
    ]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setSearchSaveMessage(
      rejected.length
        ? `${rejected.length} search setting${rejected.length === 1 ? "" : "s"} rejected.`
        : result.changed
          ? "Saved search settings to config.json. Restart the daemon for routing changes to take effect."
          : "No search config changes were persisted.",
    );
  }

  async function saveVisionConfig() {
    setVisionSaveMessage(undefined);
    const trimmed = visionDefault.trim();
    const result = await onUpdateRuntimeConfig([trimmed ? { key: "vision.default", value: trimmed } : { key: "vision.default", unset: true }]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setVisionSaveMessage(
      rejected.length
        ? `${rejected.length} vision setting${rejected.length === 1 ? "" : "s"} rejected.`
        : result.changed
          ? "Saved Vision default to config.json. Restart the daemon for ViewImage selection to take effect."
          : "No Vision config changes were persisted.",
    );
  }

  function updateProviderDraft(providerId: string, patch: Partial<ProviderDraft>) {
    setProviderDrafts((drafts) => ({
      ...drafts,
      [providerId]: {
        ...(drafts[providerId] ?? {
          transport: "openai_responses",
          baseUrl: "",
          credentialSource: "env",
          credentialKind: "api_key",
          credentialEnv: "",
          credentialProfile: "",
          credentialExternal: "",
        }),
        ...patch,
      },
    }));
  }

  async function saveProviderConfig(providerId: string) {
    const draft = providerDrafts[providerId];
    if (!draft) return;
    setProviderSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([
      { key: `providers.${providerId}.base_url`, value: draft.baseUrl.trim() },
      { key: `providers.${providerId}.auth.source`, value: draft.credentialSource },
      { key: `providers.${providerId}.auth.kind`, value: draft.credentialKind },
      { key: `providers.${providerId}.auth.env`, value: draft.credentialEnv?.trim() ?? "" },
      { key: `providers.${providerId}.auth.profile`, value: draft.credentialProfile?.trim() ?? "" },
      { key: `providers.${providerId}.auth.external`, value: draft.credentialExternal?.trim() ?? "" },
    ]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setProviderSaveMessage(
      rejected.length
        ? `${rejected.length} provider setting${rejected.length === 1 ? "" : "s"} rejected.`
        : result.changed
          ? `Saved ${providerId} provider settings to config.json. Restart the daemon for credential changes to take effect.`
          : "No provider config changes were persisted.",
    );
  }

  return (
    <section className="page settings-page" aria-label="Settings">
      <div className="page-inner settings-inner">
        <Card className="summary-panel settings-hero">
          <span className="eyebrow">Runtime configuration</span>
          <h1>Settings</h1>
          <p>
            Configure common runtime defaults from the Web GUI. Saved model defaults are persisted to config.json
            and take effect after the daemon is restarted.
          </p>
          <div className="settings-quickstart" aria-label="Settings overview">
            <div>
              <span>Connection</span>
              <strong>{connection.source === "http" ? "Live runtime" : "Preview data"}</strong>
              <small>{connection.baseUrl ?? "No API base configured"}</small>
            </div>
            <div>
              <span>Model providers</span>
              <strong>
                {configuredProviderCount}/{surface?.providers.length ?? 0} ready
              </strong>
              <small>Credential changes may require daemon restart.</small>
            </div>
            <div>
              <span>Web search</span>
              <strong>{surface?.webSearch?.enabled ? "Enabled" : "Disabled"}</strong>
              <small>
                {searchProviderCount ? `${searchProviderCount} configured provider${searchProviderCount === 1 ? "" : "s"}` : "Using builtin provider defaults"}
              </small>
            </div>
            <div>
              <span>Vision</span>
              <strong>{surface?.visionDefault ? "Pinned model" : "Auto-discovery"}</strong>
              <small>{surface?.visionDefault ?? `${visionModels.length} image-capable model${visionModels.length === 1 ? "" : "s"} ready`}</small>
            </div>
          </div>
        </Card>

        <div className="settings-grid">
          {/* ── Model & Vision ── */}
          <Card className="settings-card">
            <div className="settings-card-head">
              <div>
                <span className="eyebrow">Runtime defaults</span>
                <h2>Model &amp; Vision</h2>
              </div>
              <Button type="button" variant="secondary" disabled={runtimeConfigLoading} onClick={() => void onRefreshRuntimeConfig()}>
                {runtimeConfigLoading ? "Refreshing…" : "Refresh"}
              </Button>
            </div>
            {runtimeConfigError ? <div className="settings-error-banner">{runtimeConfigError}</div> : null}
            {!surface ? (
              <div className="settings-callout">
                <strong>Runtime config unavailable</strong>
                <span>Connect to a live runtime and refresh this page to edit model defaults.</span>
              </div>
            ) : (
              <form
                className="settings-form"
                onSubmit={(event) => {
                  event.preventDefault();
                  void saveRuntimeConfig();
                }}
              >
                <label>
                  <span>Default model</span>
                  <input list="available-models" value={modelDefault} onChange={(event) => setModelDefault(event.target.value)} />
                  <datalist id="available-models">
                    {availableModels.map((model) => (
                      <option key={model.model} value={model.model}>
                        {model.displayName}
                      </option>
                    ))}
                  </datalist>
                </label>
                <details className="settings-advanced">
                  <summary>Advanced</summary>
                  <label>
                    <span>Vision default model</span>
                    <input list="vision-models" value={visionDefault} onChange={(event) => setVisionDefault(event.target.value)} placeholder="provider/model or empty for auto" />
                    <datalist id="vision-models">
                      {visionModels.map((model) => (
                        <option key={model.model} value={model.model}>
                          {model.displayName}
                        </option>
                      ))}
                    </datalist>
                  </label>
                  <label>
                    <span>Fallback models</span>
                    <input value={modelFallbacks} onChange={(event) => setModelFallbacks(event.target.value)} placeholder="provider/model, provider/model" />
                  </label>
                  <div className="settings-form-row">
                    <label>
                      <span>Max output tokens</span>
                      <input inputMode="numeric" value={runtimeMaxOutputTokens} onChange={(event) => setRuntimeMaxOutputTokens(event.target.value)} />
                    </label>
                    <label>
                      <span>Default tool output tokens</span>
                      <input inputMode="numeric" value={defaultToolOutputTokens} onChange={(event) => setDefaultToolOutputTokens(event.target.value)} />
                    </label>
                    <label>
                      <span>Max tool output tokens</span>
                      <input inputMode="numeric" value={maxToolOutputTokens} onChange={(event) => setMaxToolOutputTokens(event.target.value)} />
                    </label>
                  </div>
                  <label className="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={disableProviderFallback}
                      onChange={(event) => setDisableProviderFallback(event.target.checked)}
                    />
                    <span>Disable provider fallback</span>
                  </label>
                </details>
                <div className="settings-actions">
                  <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                    {runtimeConfigSaving ? "Saving…" : "Save"}
                  </Button>
                  {saveMessage ? <span>{saveMessage}</span> : null}
                  {visionSaveMessage ? <span>{visionSaveMessage}</span> : null}
                </div>
                {rejectedResults.length ? (
                  <div className="settings-error-banner">
                    {rejectedResults.map((result) => (
                      <div key={result.key}>
                        <strong>{result.key}</strong>: {result.reason}
                      </div>
                    ))}
                  </div>
                ) : null}
              </form>
            )}
            <dl className="settings-list compact">
              <div>
                <dt>Config file</dt>
                <dd>{runtimeConfig.configFilePath ?? "not reported"}</dd>
              </div>
              <div>
                <dt>Provider fallback</dt>
                <dd>{surface?.disableProviderFallback ? "disabled" : "enabled"}</dd>
              </div>
              <div>
                <dt>Providers configured</dt>
                <dd>{configuredProviderCount}</dd>
              </div>
            </dl>
          </Card>

          {/* ── Web search ── */}
          <Card className="settings-card">
            <div className="settings-card-head">
              <div>
                <span className="eyebrow">Runtime defaults</span>
                <h2>Web search</h2>
              </div>
            </div>
            {!surface?.webSearch ? (
              <div className="settings-callout">
                <strong>Search config unavailable</strong>
                <span>Refresh runtime config after connecting to a live daemon.</span>
              </div>
            ) : (
              <form
                className="settings-form"
                onSubmit={(event) => {
                  event.preventDefault();
                  void saveSearchConfig();
                }}
              >
                <label className="settings-checkbox">
                  <input type="checkbox" checked={searchEnabled} onChange={(event) => setSearchEnabled(event.target.checked)} />
                  <span>Enable WebSearch</span>
                </label>
                <label>
                  <span>Default provider</span>
                  <input list="web-search-providers" value={searchProvider} onChange={(event) => setSearchProvider(event.target.value)} />
                  <datalist id="web-search-providers">
                    <option value="auto">auto</option>
                    <option value="duckduckgo">duckduckgo</option>
                    {surface.webSearchProviders.map((provider) => (
                      <option key={provider.id} value={provider.id}>
                        {provider.kind}
                      </option>
                    ))}
                  </datalist>
                </label>
                <details className="settings-advanced">
                  <summary>Advanced</summary>
                  <label className="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={searchBuiltinProviderEnabled}
                      onChange={(event) => setSearchBuiltinProviderEnabled(event.target.checked)}
                    />
                    <span>Allow provider-native search when available</span>
                  </label>
                  <div className="settings-form-row">
                    <label>
                      <span>Mode</span>
                      <select value={searchMode} onChange={(event) => setSearchMode(event.target.value as "single" | "fallback" | "aggregate")}>
                        <option value="single">single</option>
                        <option value="fallback">fallback</option>
                        <option value="aggregate">aggregate</option>
                      </select>
                    </label>
                    <label>
                      <span>Provider order</span>
                      <input value={searchProviders} onChange={(event) => setSearchProviders(event.target.value)} placeholder="duckduckgo, brave" />
                    </label>
                    <label>
                      <span>Max results</span>
                      <input inputMode="numeric" value={searchMaxResults} onChange={(event) => setSearchMaxResults(event.target.value)} />
                    </label>
                  </div>
                  <div className="settings-form-row">
                    <label>
                      <span>Max provider attempts</span>
                      <input inputMode="numeric" value={searchMaxProviderAttempts} onChange={(event) => setSearchMaxProviderAttempts(event.target.value)} />
                    </label>
                    <label>
                      <span>Configured providers</span>
                      <input readOnly value={surface.webSearchProviders.map((provider) => provider.id).join(", ") || "duckduckgo builtin"} />
                    </label>
                  </div>
                </details>
                <div className="settings-actions">
                  <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                    {runtimeConfigSaving ? "Saving…" : "Save"}
                  </Button>
                  {searchSaveMessage ? <span>{searchSaveMessage}</span> : null}
                </div>
              </form>
            )}
          </Card>
        </div>

        {/* ── Model providers ── */}
        <Card className="settings-card">
          <div className="settings-card-head">
            <div>
              <span className="eyebrow">Provider accounts</span>
              <h2>Model providers</h2>
            </div>
          </div>
          {!surface ? (
            <div className="settings-callout">
              <strong>Provider config unavailable</strong>
              <span>Connect to a live runtime and refresh this page to edit model provider credentials.</span>
            </div>
          ) : (
            <div className="settings-provider-list">
              <p className="settings-muted">
                Configure each provider account. Enter the API key in the primary section; expand Advanced for transport and credential details.
              </p>
              {surface.providers.map((provider) => {
                const draft = providerDrafts[provider.id];
                if (!draft) return null;
                return (
                  <form
                    className="settings-provider-editor"
                    key={provider.id}
                    onSubmit={(event) => {
                      event.preventDefault();
                      void saveProviderConfig(provider.id);
                    }}
                  >
                    <header>
                      <div>
                        <strong>{provider.id}</strong>
                        <small>
                          {provider.transport} · {provider.credentialSource}/{provider.credentialKind}
                        </small>
                      </div>
                      <StatusChip className={`settings-status ${provider.credentialConfigured ? "available" : "unavailable"}`} tone={provider.credentialConfigured ? "success" : "error"}>
                        {provider.credentialConfigured ? "credential ready" : "credential missing"}
                      </StatusChip>
                    </header>
                    {/* Primary: API Key management */}
                    {draft.credentialSource === "credential_profile" && draft.credentialProfile ? (
                      <div className="settings-api-key-section">
                        <div className="settings-api-key-header">
                          <span>API Key for &quot;{draft.credentialProfile}&quot;</span>
                          <StatusChip
                            className={`settings-status ${isCredentialProfileConfigured(draft.credentialProfile) ? "available" : "unavailable"}`}
                            tone={isCredentialProfileConfigured(draft.credentialProfile) ? "success" : "error"}
                          >
                            {isCredentialProfileConfigured(draft.credentialProfile) ? "key set" : "no key"}
                          </StatusChip>
                        </div>
                        <div className="settings-form-row">
                          <label>
                            <span>API Key{credentialStoreLoading ? " (loading…)" : ""}</span>
                            <input
                              type="password"
                              placeholder="Paste API key for this credential profile"
                              value={apiKeyDrafts[provider.id] ?? ""}
                              onChange={(event) => setApiKeyDrafts((prev) => ({ ...prev, [provider.id]: event.target.value }))}
                            />
                          </label>
                        </div>
                        <div className="settings-actions">
                          <Button
                            type="button"
                            variant="secondary"
                            disabled={!apiKeyDrafts[provider.id]?.trim()}
                            onClick={() => void saveApiKey(provider.id, draft.credentialProfile!, draft.credentialKind)}
                          >
                            Save API Key
                          </Button>
                          {isCredentialProfileConfigured(draft.credentialProfile) ? (
                            <Button
                              type="button"
                              variant="secondary"
                              onClick={() => void removeApiKey(provider.id, draft.credentialProfile!)}
                            >
                              Remove Key
                            </Button>
                          ) : null}
                          {credentialMessages[provider.id] ? (
                            <span className="settings-save-message">{credentialMessages[provider.id]}</span>
                          ) : null}
                        </div>
                      </div>
                    ) : (
                      <p className="settings-hint">
                        Credential source is <code>{draft.credentialSource}</code>. Switch to <code>credential_profile</code> in Advanced to manage API keys from the web UI.
                      </p>
                    )}
                    {/* Advanced: full provider config */}
                    <details className="settings-advanced">
                      <summary>Advanced</summary>
                      <div className="settings-form-row">
                        <label>
                          <span>Transport <small className="settings-muted">(read-only)</small></span>
                          <input value={provider.transport} readOnly disabled />
                        </label>
                        <label>
                          <span>Base URL</span>
                          <input value={draft.baseUrl} onChange={(event) => updateProviderDraft(provider.id, { baseUrl: event.target.value })} />
                        </label>
                        <label>
                          <span>Credential source</span>
                          <select value={draft.credentialSource} onChange={(event) => {
                            const source = event.target.value;
                            if (source === "credential_profile" && !draft.credentialProfile?.trim()) {
                              updateProviderDraft(provider.id, { credentialSource: source, credentialProfile: `${provider.id}:default` });
                            } else {
                              updateProviderDraft(provider.id, { credentialSource: source });
                            }
                          }}>
                            {credentialSources.map((source) => (
                              <option key={source} value={source}>
                                {source}
                              </option>
                            ))}
                          </select>
                        </label>
                      </div>
                      <div className="settings-form-row">
                        <label>
                          <span>Credential kind</span>
                          <select value={draft.credentialKind} onChange={(event) => updateProviderDraft(provider.id, { credentialKind: event.target.value })}>
                            {credentialKinds.map((kind) => (
                              <option key={kind} value={kind}>
                                {kind}
                              </option>
                            ))}
                          </select>
                        </label>
                        <label>
                          <span>Env variable</span>
                          <input value={draft.credentialEnv ?? ""} onChange={(event) => updateProviderDraft(provider.id, { credentialEnv: event.target.value })} />
                        </label>
                        <label>
                          <span>Credential profile</span>
                          <input value={draft.credentialProfile ?? ""} onChange={(event) => updateProviderDraft(provider.id, { credentialProfile: event.target.value })} />
                        </label>
                      </div>
                      {draft.credentialSource === "credential_profile" ? (
                        <p className="settings-hint">Auto-named <code>{provider.id}:default</code> if left empty. Multiple providers can share one profile; use different names (e.g. <code>{provider.id}:work</code>) for separate keys.</p>
                      ) : null}
                      <label>
                        <span>External credential provider</span>
                        <input value={draft.credentialExternal ?? ""} onChange={(event) => updateProviderDraft(provider.id, { credentialExternal: event.target.value })} />
                      </label>
                    </details>
                    <div className="settings-actions">
                      <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                        {runtimeConfigSaving ? "Saving…" : `Save ${provider.id}`}
                      </Button>
                    </div>
                  </form>
                );
              })}
              {providerSaveMessage ? <span className="settings-save-message">{providerSaveMessage}</span> : null}
            </div>
          )}
        </Card>

        {/* ── Model catalog diagnostics ── */}
        <Card className="settings-card settings-models">
          <div className="settings-card-head">
            <div>
              <span className="eyebrow">Models / Providers</span>
              <h2>Model catalog diagnostics</h2>
            </div>
            <Button type="button" variant="secondary" disabled={modelCatalogLoading} onClick={() => void onRefreshModels()}>
              {modelCatalogLoading ? "Refreshing…" : "Refresh"}
            </Button>
          </div>

          {modelCatalogError ? <div className="settings-error-banner">{modelCatalogError}</div> : null}
          {!modelCatalogLoading && modelCatalog.options.length === 0 ? (
            <EmptyState className="settings-empty" title="No models returned" description="The runtime has not returned a model catalog yet." />
          ) : null}

          <p className="settings-muted">The editable provider accounts above are the primary configuration surface. Use this catalog only to inspect runtime model availability.</p>
          <details className="settings-diagnostics">
            <summary>
              Show {modelCatalog.options.length} model{modelCatalog.options.length === 1 ? "" : "s"} across {groupedModels.length} provider{groupedModels.length === 1 ? "" : "s"}
            </summary>
            <div className="provider-list">
              {groupedModels.map(([provider, models]) => (
                <Card className="provider-card" key={provider}>
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
                          <StatusChip className={`settings-status ${model.available ? "available" : "unavailable"}`} tone={model.available ? "success" : "error"}>
                            {model.available ? "available" : "unavailable"}
                          </StatusChip>
                        </div>
                        <div role="cell">
                          {model.supportsReasoningEffort ? <span className="settings-pill">reasoning</span> : null}
                          {model.unavailableReason ? <small>{model.unavailableReason}</small> : null}
                        </div>
                      </div>
                    ))}
                  </div>
                </Card>
              ))}
            </div>
          </details>
        </Card>
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
