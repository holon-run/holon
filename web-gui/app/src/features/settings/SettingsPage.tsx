import { useEffect, useMemo, useState } from "react";

import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { EmptyState } from "../../components/ui/EmptyState";
import { StatusChip } from "../../components/ui/StatusChip";
import type {
  CodexDeviceLoginState,
  CredentialStoreState,
  RuntimeConfigState,
  RuntimeConnection,
  RuntimeModelCatalog,
  RuntimeModelOption,
  RuntimeProviderSummary,
  RuntimeWebSearchProviderSummary,
} from "../../runtime/types";

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
  codexDeviceLogin: CodexDeviceLoginState;
  onStartCodexDeviceLogin: () => Promise<void>;
  onClearCodexDeviceLogin: () => void;
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

export function buildVisionConfigUpdates(visionDefault: string): Array<{ key: string; value?: unknown; unset?: boolean }> {
  const trimmed = visionDefault.trim();
  return [trimmed ? { key: "vision.default", value: trimmed } : { key: "vision.default", unset: true }];
}

type ProviderDraft = Pick<
  RuntimeProviderSummary,
  "transport" | "baseUrl" | "credentialSource" | "credentialKind" | "credentialEnv" | "credentialProfile" | "credentialExternal"
>;

type SearchProviderDraft = Pick<RuntimeWebSearchProviderSummary, "kind" | "baseUrl" | "credentialProfile">;

type StandardSearchProviderDefinition = {
  id: string;
  kind: string;
  label: string;
  description: string;
  requiresApiKey: boolean;
  defaultCredentialProfile?: string;
  baseUrlPlaceholder?: string;
};

const webSearchProviderKinds = [
  "duck_duck_go",
  "searxng",
  "brave",
  "tavily",
  "exa",
  "perplexity",
  "firecrawl",
  "open_ai_native",
  "anthropic_native",
  "gemini_native",
  "command",
];

const standardSearchProviders: StandardSearchProviderDefinition[] = [
  {
    id: "brave",
    kind: "brave",
    label: "Brave Search",
    description: "Good general-purpose web results. Requires a Brave Search API key.",
    requiresApiKey: true,
    defaultCredentialProfile: "brave:default",
  },
  {
    id: "tavily",
    kind: "tavily",
    label: "Tavily",
    description: "Search API optimized for agent and RAG workflows. Requires a Tavily API key.",
    requiresApiKey: true,
    defaultCredentialProfile: "tavily:default",
  },
  {
    id: "exa",
    kind: "exa",
    label: "Exa",
    description: "Neural search API for high-relevance web retrieval. Requires an Exa API key.",
    requiresApiKey: true,
    defaultCredentialProfile: "exa:default",
  },
  {
    id: "perplexity",
    kind: "perplexity",
    label: "Perplexity",
    description: "Perplexity search-backed answers. Requires a Perplexity API key.",
    requiresApiKey: true,
    defaultCredentialProfile: "perplexity:default",
  },
  {
    id: "firecrawl",
    kind: "firecrawl",
    label: "Firecrawl",
    description: "Search and crawl provider for page extraction workflows. Requires a Firecrawl API key.",
    requiresApiKey: true,
    defaultCredentialProfile: "firecrawl:default",
  },
  {
    id: "searxng",
    kind: "searxng",
    label: "SearXNG",
    description: "Use a self-hosted or trusted SearXNG instance. No API key is needed.",
    requiresApiKey: false,
    baseUrlPlaceholder: "https://search.example.com",
  },
];

const standardSearchProviderById = new Map(standardSearchProviders.map((provider) => [provider.id, provider]));
const standardSearchProviderIds = new Set(standardSearchProviders.map((provider) => provider.id));

type SettingsTabKey = "general" | "models" | "vision" | "search" | "advanced";

const settingsTabs: Array<{ key: SettingsTabKey; label: string; description: string }> = [
  { key: "general", label: "General", description: "connection and runtime basics" },
  { key: "models", label: "Models", description: "defaults and provider keys" },
  { key: "vision", label: "Vision", description: "image observation model" },
  { key: "search", label: "Search", description: "routing and search providers" },
  { key: "advanced", label: "Advanced", description: "diagnostics and raw config" },
];

function defaultSearchProviderDraft(providerId: string): SearchProviderDraft {
  const definition = standardSearchProviderById.get(providerId);
  return {
    kind: definition?.kind ?? "brave",
    baseUrl: "",
    credentialProfile: definition?.requiresApiKey ? definition.defaultCredentialProfile ?? `${providerId}:default` : "",
  };
}

export function buildSearchProviderConfigUpdates(providerId: string, draft: SearchProviderDraft): Array<{ key: string; value?: unknown; unset?: boolean }> {
  return [
    { key: `web.providers.${providerId}.kind`, value: draft.kind },
    { key: `web.providers.${providerId}.base_url`, value: draft.baseUrl?.trim() ?? "" },
    { key: `web.providers.${providerId}.credential_profile`, value: draft.credentialProfile?.trim() ?? "" },
  ];
}

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
  codexDeviceLogin,
  onStartCodexDeviceLogin,
  onClearCodexDeviceLogin,
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
  const [searchProviderDrafts, setSearchProviderDrafts] = useState<Record<string, SearchProviderDraft>>({});
  const [newSearchProviderId, setNewSearchProviderId] = useState("");
  const [newSearchProviderKind, setNewSearchProviderKind] = useState("brave");
  const [providerDrafts, setProviderDrafts] = useState<Record<string, ProviderDraft>>({});
  const [saveMessage, setSaveMessage] = useState<string | undefined>();
  const [searchSaveMessage, setSearchSaveMessage] = useState<string | undefined>();
  const [searchProviderSaveMessage, setSearchProviderSaveMessage] = useState<string | undefined>();
  const [visionSaveMessage, setVisionSaveMessage] = useState<string | undefined>();
  const [providerSaveMessage, setProviderSaveMessage] = useState<string | undefined>();
  const [activeTab, setActiveTab] = useState<SettingsTabKey>("models");
  const [apiKeyDrafts, setApiKeyDrafts] = useState<Record<string, string>>({});
  const [searchApiKeyDrafts, setSearchApiKeyDrafts] = useState<Record<string, string>>({});
  const [credentialMessages, setCredentialMessages] = useState<Record<string, string>>({});
  const [searchCredentialMessages, setSearchCredentialMessages] = useState<Record<string, string>>({});
  const availableModels = useMemo(() => modelCatalog.options.filter((model) => model.available), [modelCatalog.options]);
  const visionModels = useMemo(() => modelCatalog.options.filter((model) => model.available && model.supportsImageInput), [modelCatalog.options]);
  const providersWithModels = useMemo(
    () => new Set(modelCatalog.options.map((m) => m.provider)),
    [modelCatalog.options],
  );
  const sortedProviders = useMemo(
    () => sortProvidersForSettings(surface?.providers ?? []),
    [surface?.providers],
  );
  const sortedSearchProviders = useMemo(
    () => sortSearchProvidersForSettings(surface?.webSearchProviders ?? []),
    [surface?.webSearchProviders],
  );

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
    setSearchProviderDrafts(
      Object.fromEntries(
        surface.webSearchProviders.map((provider) => [
          provider.id,
          {
            kind: provider.kind,
            baseUrl: provider.baseUrl ?? "",
            credentialProfile: provider.credentialProfile ?? "",
          },
        ]),
      ),
    );
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
    setSearchProviderSaveMessage(undefined);
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
    // Switch provider to credential_profile so the stored key is used
    await onUpdateRuntimeConfig([
      { key: `providers.${providerId}.auth.source`, value: "credential_profile" },
      { key: `providers.${providerId}.auth.kind`, value: credentialKind },
      { key: `providers.${providerId}.auth.profile`, value: credentialProfile },
    ]);
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
  const configuredSearchProviderCount = surface?.webSearchProviders.filter((provider) => provider.credentialConfigured).length ?? 0;
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
          ? "Saved to config.json. Changes applied via hot-reload."
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
          ? "Saved search settings to config.json. Changes applied via hot-reload."
          : "No search config changes were persisted.",
    );
  }

  function updateSearchProviderDraft(providerId: string, patch: Partial<SearchProviderDraft>) {
    setSearchProviderDrafts((drafts) => ({
      ...drafts,
      [providerId]: {
        ...(drafts[providerId] ?? defaultSearchProviderDraft(providerId)),
        ...patch,
      },
    }));
  }

  function addSearchProviderDraft() {
    const providerId = newSearchProviderId.trim();
    if (!providerId) return;
    updateSearchProviderDraft(providerId, {
      kind: newSearchProviderKind,
      credentialProfile: `${providerId}:default`,
    });
    setNewSearchProviderId("");
    setSearchProviderSaveMessage(`Prepared ${providerId}. Review and save the provider config below.`);
  }

  async function saveSearchProviderConfig(providerId: string) {
    const draft = searchProviderDrafts[providerId] ?? defaultSearchProviderDraft(providerId);
    setSearchProviderSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig(buildSearchProviderConfigUpdates(providerId, draft));
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setSearchProviderSaveMessage(
      rejected.length
        ? `${rejected.length} search provider setting${rejected.length === 1 ? "" : "s"} rejected.`
        : result.changed
          ? `Saved ${providerId} search provider settings to config.json. Changes applied via hot-reload.`
          : "No search provider config changes were persisted.",
    );
  }

  async function removeSearchProviderConfig(providerId: string) {
    const confirmed = window.confirm(
      `Remove ${providerId} from web.providers in config.json? This does not delete credentials.`,
    );
    if (!confirmed) return;

    setSearchProviderSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([{ key: `web.providers.${providerId}`, unset: true }]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setSearchProviderSaveMessage(
      rejected.length
        ? `${rejected.length} search provider removal${rejected.length === 1 ? "" : "s"} rejected.`
        : result.changed
          ? `Removed ${providerId} search provider config from config.json. Credentials were not deleted.`
          : `No persisted ${providerId} search provider config was removed.`,
    );
  }

  async function saveSearchApiKey(providerId: string, credentialProfile: string) {
    const key = searchApiKeyDrafts[providerId]?.trim();
    if (!key || !credentialProfile) return;
    setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: "Saving…" }));
    const result = await onSetCredential(credentialProfile, "api_key", key);
    if (result) {
      setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: "API key saved to credential store." }));
      setSearchApiKeyDrafts((prev) => ({ ...prev, [providerId]: "" }));
    } else {
      setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: "Failed to save API key." }));
    }
  }

  async function removeSearchApiKey(providerId: string, credentialProfile: string) {
    if (!credentialProfile) return;
    setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: "Removing…" }));
    try {
      await onDeleteCredential(credentialProfile);
      setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: "API key removed from credential store." }));
    } catch {
      setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: "Failed to remove API key." }));
    }
  }

  async function saveVisionConfig() {
    setVisionSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig(buildVisionConfigUpdates(visionDefault));
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setVisionSaveMessage(
      rejected.length
        ? `${rejected.length} vision setting${rejected.length === 1 ? "" : "s"} rejected.`
        : result.changed
          ? "Saved Vision default to config.json. Changes applied via hot-reload."
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
          ? `Saved ${providerId} provider settings to config.json. Changes applied via hot-reload.`
          : "No provider config changes were persisted.",
    );
  }

  async function removeProviderConfig(providerId: string) {
    const confirmed = window.confirm(
      `Remove ${providerId} from config.json? This only removes persisted provider config; it does not delete credentials or disable built-in provider defaults.`,
    );
    if (!confirmed) return;

    setProviderSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([{ key: `providers.${providerId}`, unset: true }]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setProviderSaveMessage(
      rejected.length
        ? `${rejected.length} provider config removal${rejected.length === 1 ? "" : "s"} rejected.`
        : result.changed
          ? `Removed ${providerId} provider config from config.json. Built-in providers may fall back to defaults; credentials were not deleted.`
          : `No persisted ${providerId} provider config was removed.`,
    );
  }

  return (
    <section className="page settings-page" aria-label="Settings">
      <div className="page-inner settings-inner">
        <Card className="summary-panel settings-hero">
          <span className="eyebrow">Runtime configuration</span>
          <h1>Settings</h1>
          <p>
            Configure common runtime defaults from the Web GUI. Saved model and vision defaults are persisted to config.json
            and take effect immediately via hot-reload.
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
              <small>Credential changes apply via hot-reload.</small>
            </div>
            <div>
              <span>Web search</span>
              <strong>{surface?.webSearch?.enabled ? "Enabled" : "Disabled"}</strong>
              <small>
                {searchProviderCount
                  ? `${configuredSearchProviderCount}/${searchProviderCount} search provider${searchProviderCount === 1 ? "" : "s"} ready`
                  : "Using builtin provider defaults"}
              </small>
            </div>
            <div>
              <span>Vision</span>
              <strong>{surface?.visionDefault ? "Pinned model" : "Auto-discovery"}</strong>
              <small>{surface?.visionDefault ?? `${visionModels.length} image-capable model${visionModels.length === 1 ? "" : "s"} ready`}</small>
            </div>
          </div>
        </Card>

        <div className="settings-tabs" role="tablist" aria-label="Settings sections">
          {settingsTabs.map((tab) => (
            <button
              aria-selected={activeTab === tab.key}
              className={`settings-tab ${activeTab === tab.key ? "active" : ""}`}
              key={tab.key}
              onClick={() => setActiveTab(tab.key)}
              role="tab"
              type="button"
            >
              <span>{tab.label}</span>
              <small>{tab.description}</small>
            </button>
          ))}
        </div>

        {activeTab === "general" ? (
          <div className="settings-grid">
            <Card className="settings-card settings-primary-card">
              <div className="settings-card-head">
                <div>
                  <span className="eyebrow">General</span>
                  <h2>Runtime overview</h2>
                </div>
                <Button type="button" variant="secondary" disabled={runtimeConfigLoading} onClick={() => void onRefreshRuntimeConfig()}>
                  {runtimeConfigLoading ? "Refreshing…" : "Refresh"}
                </Button>
              </div>
              {runtimeConfigError ? <div className="settings-error-banner">{runtimeConfigError}</div> : null}
              <dl className="settings-list compact">
                <div>
                  <dt>Connection</dt>
                  <dd>{connection.source === "http" ? "Live runtime" : "Preview data"}</dd>
                </div>
                <div>
                  <dt>API base</dt>
                  <dd>{connection.baseUrl ?? "not configured"}</dd>
                </div>
                <div>
                  <dt>Config file</dt>
                  <dd>{runtimeConfig.configFilePath ?? "not reported"}</dd>
                </div>
                <div>
                  <dt>Provider fallback</dt>
                  <dd>{surface?.disableProviderFallback ? "disabled" : "enabled"}</dd>
                </div>
                <div>
                  <dt>Model providers</dt>
                  <dd>
                    {configuredProviderCount}/{surface?.providers.length ?? 0} credential ready
                  </dd>
                </div>
                <div>
                  <dt>Search</dt>
                  <dd>{surface?.webSearch?.enabled ? "enabled" : "disabled"}</dd>
                </div>
                <div>
                  <dt>Vision</dt>
                  <dd>{surface?.visionDefault ? surface.visionDefault : "auto-discovery"}</dd>
                </div>
              </dl>
            </Card>
          </div>
        ) : null}

        <div className="settings-grid">
          {/* ── Model defaults ── */}
          <Card className="settings-card settings-primary-card" hidden={activeTab !== "models"}>
            <div className="settings-card-head">
              <div>
                <span className="eyebrow">Runtime defaults</span>
                <h2>Model</h2>
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

          {/* ── Vision defaults ── */}
          <Card className="settings-card settings-primary-card" hidden={activeTab !== "vision"}>
            <div className="settings-card-head">
              <div>
                <span className="eyebrow">Vision</span>
                <h2>Image observation</h2>
              </div>
            </div>
            {!surface ? (
              <div className="settings-callout">
                <strong>Vision config unavailable</strong>
                <span>Connect to a live runtime and refresh this page to edit the Vision default model.</span>
              </div>
            ) : (
              <form
                className="settings-form"
                onSubmit={(event) => {
                  event.preventDefault();
                  void saveVisionConfig();
                }}
              >
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
                <p className="settings-hint">
                  Leave empty to let ViewImage auto-discover an authenticated image-capable model.
                </p>
                <div className="settings-actions">
                  <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                    {runtimeConfigSaving ? "Saving…" : "Save Vision"}
                  </Button>
                  {visionDefault ? (
                    <StatusChip className={`settings-status ${visionProviderReady ? "available" : "unavailable"}`} tone={visionProviderReady ? "success" : "error"}>
                      {visionProviderReady ? "provider ready" : "provider credential missing"}
                    </StatusChip>
                  ) : (
                    <StatusChip className="settings-status available" tone="success">
                      auto-discovery
                    </StatusChip>
                  )}
                  {visionSaveMessage ? <span>{visionSaveMessage}</span> : null}
                </div>
              </form>
            )}
          </Card>

          {/* ── Web search ── */}
          <Card className="settings-card settings-primary-card" hidden={activeTab !== "search"}>
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
                  <span>Routing</span>
                  <select value={searchProvider || "auto"} onChange={(event) => setSearchProvider(event.target.value)}>
                    <option value="auto">Auto — use configured providers, then DuckDuckGo</option>
                    <option value="duckduckgo">DuckDuckGo — builtin, no API key</option>
                    {standardSearchProviders.map((provider) => {
                      const configured = surface.webSearchProviders.find((entry) => entry.id === provider.id);
                      const ready = provider.requiresApiKey ? configured?.credentialConfigured : Boolean(configured);
                      return (
                        <option key={provider.id} value={provider.id}>
                          {provider.label}{ready ? " — ready" : provider.requiresApiKey ? " — API key needed" : ""}
                        </option>
                      );
                    })}
                  </select>
                </label>
                <label className="settings-checkbox">
                  <input
                    type="checkbox"
                    checked={searchBuiltinProviderEnabled}
                    onChange={(event) => setSearchBuiltinProviderEnabled(event.target.checked)}
                  />
                  <span>Allow model-native search when available</span>
                </label>
                <p className="settings-hint">
                  DuckDuckGo and native search do not need API keys. Add keys only for API-backed providers below.
                </p>
                <details className="settings-advanced">
                  <summary>Advanced</summary>
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

        {/* ── Web search providers ── */}
        <Card className="settings-card" hidden={activeTab !== "search"}>
          <div className="settings-card-head">
            <div>
              <span className="eyebrow">Provider accounts</span>
              <h2>Web search providers</h2>
            </div>
          </div>
          {!surface ? (
            <div className="settings-callout">
              <strong>Search provider config unavailable</strong>
              <span>Connect to a live runtime and refresh this page to edit web search provider credentials.</span>
            </div>
          ) : (
            <div className="settings-provider-list">
              <p className="settings-muted">
                Standard providers are shown as product choices. The UI creates the matching <code>web.providers.&lt;id&gt;</code> entry and stores API keys in the existing credential store.
              </p>
              <div className="settings-builtins">
                <div>
                  <strong>Native search</strong>
                  <span>Uses model-provider native search when the runtime can route to it. No API key is configured here.</span>
                </div>
                <StatusChip className={`settings-status ${searchBuiltinProviderEnabled ? "available" : "unavailable"}`} tone={searchBuiltinProviderEnabled ? "success" : "error"}>
                  {searchBuiltinProviderEnabled ? "allowed" : "disabled"}
                </StatusChip>
                <div>
                  <strong>DuckDuckGo</strong>
                  <span>Built in and ready by default. No provider id, kind, or API key is required.</span>
                </div>
                <StatusChip className="settings-status available" tone="success">
                  ready
                </StatusChip>
              </div>
              {standardSearchProviders.map((definition) => {
                const provider = surface.webSearchProviders.find((entry) => entry.id === definition.id);
                const draft = searchProviderDrafts[definition.id] ?? defaultSearchProviderDraft(definition.id);
                const credentialProfile = draft.credentialProfile?.trim() ?? definition.defaultCredentialProfile ?? "";
                const credentialReady = credentialProfile ? isCredentialProfileConfigured(credentialProfile) : false;
                const providerReady = definition.requiresApiKey ? credentialReady : Boolean(provider);
                return (
                  <form
                    className="settings-provider-editor"
                    key={definition.id}
                    onSubmit={(event) => {
                      event.preventDefault();
                      void saveSearchProviderConfig(definition.id);
                    }}
                  >
                    <header>
                      <div>
                        <strong>{definition.label}</strong>
                        <small>
                          {definition.description}
                        </small>
                      </div>
                      <StatusChip className={`settings-status ${providerReady ? "available" : "unavailable"}`} tone={providerReady ? "success" : "error"}>
                        {providerReady ? "ready" : definition.requiresApiKey ? "key needed" : "not configured"}
                      </StatusChip>
                    </header>
                    {!definition.requiresApiKey ? (
                      <div className="settings-form-row">
                        <label>
                          <span>Base URL</span>
                          <input
                            value={draft.baseUrl ?? ""}
                            onChange={(event) => updateSearchProviderDraft(definition.id, { baseUrl: event.target.value })}
                            placeholder={definition.baseUrlPlaceholder ?? "Optional provider base URL"}
                          />
                        </label>
                      </div>
                    ) : null}
                    {definition.requiresApiKey ? (
                      <label>
                        <span>Credential profile</span>
                        <input
                          value={credentialProfile}
                          onChange={(event) => updateSearchProviderDraft(definition.id, { credentialProfile: event.target.value })}
                          placeholder={definition.defaultCredentialProfile}
                        />
                      </label>
                    ) : null}
                    {definition.requiresApiKey && credentialProfile ? (
                      <div className="settings-api-key-section">
                        <div className="settings-api-key-header">
                          <span>API Key for &quot;{credentialProfile}&quot;</span>
                          <StatusChip
                            className={`settings-status ${credentialReady ? "available" : "unavailable"}`}
                            tone={credentialReady ? "success" : "error"}
                          >
                            {credentialReady ? "key set" : "no key"}
                          </StatusChip>
                        </div>
                        <div className="settings-form-row">
                          <label>
                            <span>API Key{credentialStoreLoading ? " (loading…)" : ""}</span>
                            <input
                              type="password"
                              placeholder="Paste API key for this search provider"
                              value={searchApiKeyDrafts[definition.id] ?? ""}
                              onChange={(event) => setSearchApiKeyDrafts((prev) => ({ ...prev, [definition.id]: event.target.value }))}
                            />
                          </label>
                        </div>
                        <div className="settings-actions">
                          <Button
                            type="button"
                            variant="secondary"
                            disabled={!searchApiKeyDrafts[definition.id]?.trim()}
                            onClick={() => void saveSearchApiKey(definition.id, credentialProfile)}
                          >
                            Save API Key
                          </Button>
                          {credentialReady ? (
                            <Button type="button" variant="secondary" onClick={() => void removeSearchApiKey(definition.id, credentialProfile)}>
                              Remove Key
                            </Button>
                          ) : null}
                          {searchCredentialMessages[definition.id] ? (
                            <span className="settings-save-message">{searchCredentialMessages[definition.id]}</span>
                          ) : null}
                        </div>
                      </div>
                    ) : null}
                    <details className="settings-advanced">
                      <summary>Advanced</summary>
                      <div className="settings-form-row">
                        <label>
                          <span>Provider id</span>
                          <input value={definition.id} readOnly disabled />
                        </label>
                        <label>
                          <span>Kind</span>
                          <input value={definition.kind} readOnly disabled />
                        </label>
                        {definition.requiresApiKey ? (
                          <label>
                            <span>Base URL</span>
                            <input value={draft.baseUrl ?? ""} onChange={(event) => updateSearchProviderDraft(definition.id, { baseUrl: event.target.value })} placeholder="Optional provider default" />
                          </label>
                        ) : null}
                      </div>
                    </details>
                    <div className="settings-actions">
                      <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                        {runtimeConfigSaving ? "Saving…" : provider ? `Save ${definition.label}` : `Enable ${definition.label}`}
                      </Button>
                      {provider ? (
                        <Button
                          type="button"
                          variant="outline"
                          disabled={runtimeConfigSaving || runtimeConfigLoading}
                          onClick={() => void removeSearchProviderConfig(definition.id)}
                        >
                          Remove Config
                        </Button>
                      ) : null}
                    </div>
                  </form>
                );
              })}
              {sortedSearchProviders
                .filter((provider) => !standardSearchProviderIds.has(provider.id))
                .map((provider) => provider.id)
                .concat(Object.keys(searchProviderDrafts).filter((providerId) => !surface.webSearchProviders.some((provider) => provider.id === providerId) && !standardSearchProviderIds.has(providerId)))
                .map((providerId) => {
                  const draft = searchProviderDrafts[providerId];
                  if (!draft) return null;
                  return (
                    <form
                      className="settings-provider-editor"
                      key={providerId}
                      onSubmit={(event) => {
                        event.preventDefault();
                        void saveSearchProviderConfig(providerId);
                      }}
                    >
                      <header>
                        <div>
                          <strong>{providerId}</strong>
                          <small>Unsaved search provider</small>
                        </div>
                        <StatusChip className="settings-status unavailable" tone="error">
                          not saved
                        </StatusChip>
                      </header>
                      <div className="settings-form-row">
                        <label>
                          <span>Kind</span>
                          <select value={draft.kind} onChange={(event) => updateSearchProviderDraft(providerId, { kind: event.target.value })}>
                            {webSearchProviderKinds.map((kind) => (
                              <option key={kind} value={kind}>
                                {kind}
                              </option>
                            ))}
                          </select>
                        </label>
                        <label>
                          <span>Base URL</span>
                          <input value={draft.baseUrl ?? ""} onChange={(event) => updateSearchProviderDraft(providerId, { baseUrl: event.target.value })} />
                        </label>
                        <label>
                          <span>Credential profile</span>
                          <input value={draft.credentialProfile ?? ""} onChange={(event) => updateSearchProviderDraft(providerId, { credentialProfile: event.target.value })} />
                        </label>
                      </div>
                      <div className="settings-actions">
                        <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                          {runtimeConfigSaving ? "Saving…" : `Save ${providerId}`}
                        </Button>
                      </div>
                    </form>
                  );
                })}
              <details className="settings-advanced">
                <summary>Advanced custom provider</summary>
                <div className="settings-provider-editor">
                  <header>
                    <div>
                      <strong>Add custom search provider</strong>
                      <small>Only use this for custom ids, command providers, or experimental provider kinds.</small>
                    </div>
                  </header>
                  <div className="settings-form-row">
                    <label>
                      <span>Provider id</span>
                      <input value={newSearchProviderId} onChange={(event) => setNewSearchProviderId(event.target.value)} placeholder="custom_search" />
                    </label>
                    <label>
                      <span>Kind</span>
                      <select value={newSearchProviderKind} onChange={(event) => setNewSearchProviderKind(event.target.value)}>
                        {webSearchProviderKinds.map((kind) => (
                          <option key={kind} value={kind}>
                            {kind}
                          </option>
                        ))}
                      </select>
                    </label>
                  </div>
                  <div className="settings-actions">
                    <Button type="button" variant="secondary" disabled={!newSearchProviderId.trim()} onClick={addSearchProviderDraft}>
                      Add custom draft
                    </Button>
                  </div>
                </div>
              </details>
              {searchProviderSaveMessage ? <span className="settings-save-message">{searchProviderSaveMessage}</span> : null}
            </div>
          )}
        </Card>

        {/* ── Model providers ── */}
        <Card className="settings-card" hidden={activeTab !== "models"}>
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
              {sortedProviders.map((provider) => {
                const draft = providerDrafts[provider.id];
                if (!draft) return null;
                const effectiveProfile = draft.credentialProfile?.trim() || `${provider.id}:default`;
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
                          {provider.transport}
                        </small>
                      </div>
                      <StatusChip className={`settings-status ${provider.credentialConfigured ? "available" : "unavailable"}`} tone={provider.credentialConfigured ? "success" : "error"}>
                        {provider.credentialConfigured ? "credential ready" : "credential missing"}
                      </StatusChip>
                    </header>
                    {provider.credentialConfigured && !providersWithModels.has(provider.id) ? (
                      <p className="settings-provider-hint">
                        No models in catalog for this provider — it will not appear in the model selector. Add model entries under <strong>Model overrides</strong> or configure model discovery to make its models available.
                      </p>
                    ) : null}
                    {/* Primary: API Key management */}
                    {draft.credentialKind === "api_key" ? (
                      <div className="settings-api-key-section">
                        <div className="settings-api-key-header">
                          <span>API Key for &quot;{effectiveProfile}&quot;</span>
                          <StatusChip
                            className={`settings-status ${isCredentialProfileConfigured(effectiveProfile) ? "available" : "unavailable"}`}
                            tone={isCredentialProfileConfigured(effectiveProfile) ? "success" : "error"}
                          >
                            {isCredentialProfileConfigured(effectiveProfile) ? "key set" : "no key"}
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
                            onClick={() => void saveApiKey(provider.id, effectiveProfile, draft.credentialKind)}
                          >
                            Save API Key
                          </Button>
                          {isCredentialProfileConfigured(effectiveProfile) ? (
                            <Button
                              type="button"
                              variant="secondary"
                              onClick={() => void removeApiKey(provider.id, effectiveProfile)}
                            >
                              Remove Key
                            </Button>
                          ) : null}
                          {credentialMessages[provider.id] ? (
                            <span className="settings-save-message">{credentialMessages[provider.id]}</span>
                          ) : null}
                        </div>
                      </div>
                    ) : null}
                    {/* OAuth device login */}
                    {draft.credentialKind === "oauth" ? (
                      <div className="settings-device-login-section">
                        {provider.credentialConfigured ? (
                          <div className="settings-device-login-header">
                            <span>Connected via OAuth</span>
                            <StatusChip className="settings-status available" tone="success">
                              credential ready
                            </StatusChip>
                          </div>
                        ) : null}
                        {codexDeviceLogin.status === "idle" || codexDeviceLogin.status === "failed" ? (
                          <>
                          <div className="settings-actions">
                            <Button type="button" variant="secondary" disabled={credentialStoreLoading}
                              onClick={() => void onStartCodexDeviceLogin()}>
                              {provider.credentialConfigured ? "Re-login" : "Login with Device Flow"}
                            </Button>
                          </div>
                          {codexDeviceLogin.status === "failed" ? (
                            <div className="settings-device-login-error">{codexDeviceLogin.error}</div>
                          ) : null}
                          </>
                        ) : null}
                        {codexDeviceLogin.status === "starting" ? (
                          <p className="settings-hint">Starting device login…</p>
                        ) : null}
                        {codexDeviceLogin.status === "waiting" ? (
                          <div className="settings-device-login-panel">
                            <Button type="button" variant="secondary"
                              onClick={() => window.open(codexDeviceLogin.verificationUrl, "_blank", "noopener,noreferrer")}>
                              Open Device Login Page →
                            </Button>
                            <p className="settings-hint">Enter this code on the page:</p>
                            <div className="settings-device-login-code">{codexDeviceLogin.userCode}</div>
                            <p className="settings-muted">Waiting for authorization…</p>
                            <Button type="button" variant="outline" onClick={onClearCodexDeviceLogin}>Cancel</Button>
                          </div>
                        ) : null}
                        {codexDeviceLogin.status === "completed" ? (
                          <div className="settings-device-login-panel">
                            <StatusChip className="settings-status available" tone="success">Login successful</StatusChip>
                            <Button type="button" variant="outline" onClick={onClearCodexDeviceLogin}>Dismiss</Button>
                          </div>
                        ) : null}
                      </div>
                    ) : null}
                    {draft.credentialKind !== "api_key" && draft.credentialKind !== "oauth" ? (
                      <p className="settings-hint">
                        This provider uses <code>{draft.credentialKind}</code> authentication via <code>{draft.credentialSource}</code>. Configure it in <code>{runtimeConfig.configFilePath ?? "config.json"}</code>.
                      </p>
                    ) : null}
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
                      </div>
                    </details>
                    <div className="settings-actions">
                      <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                        {runtimeConfigSaving ? "Saving…" : `Save ${provider.id}`}
                      </Button>
                      <Button
                        type="button"
                        variant="outline"
                        disabled={runtimeConfigSaving || runtimeConfigLoading || !provider.configuredInConfig}
                        title={
                          provider.configuredInConfig
                            ? "Remove this provider from config.json"
                            : "This provider is currently using built-in defaults; no persisted config exists to remove."
                        }
                        onClick={() => void removeProviderConfig(provider.id)}
                      >
                        Remove Config
                      </Button>
                    </div>
                  </form>
                );
              })}
              {providerSaveMessage ? <span className="settings-save-message">{providerSaveMessage}</span> : null}
            </div>
          )}
        </Card>

        <Card className="settings-card" hidden={activeTab !== "advanced"}>
          <div className="settings-card-head">
            <div>
              <span className="eyebrow">Advanced</span>
              <h2>Raw config</h2>
            </div>
            <Button type="button" variant="secondary" disabled={runtimeConfigLoading} onClick={() => void onRefreshRuntimeConfig()}>
              {runtimeConfigLoading ? "Refreshing…" : "Refresh"}
            </Button>
          </div>
          <div className="settings-callout">
            <strong>Raw config editing is not exposed in the Web GUI yet.</strong>
            <span>Use the focused tabs for common changes, or edit the config file directly for uncommon runtime fields.</span>
          </div>
          <dl className="settings-list compact">
            <div>
              <dt>Config file</dt>
              <dd>{runtimeConfig.configFilePath ?? "not reported"}</dd>
            </div>
            <div>
              <dt>Model catalog</dt>
              <dd>
                {modelCatalog.options.length} model{modelCatalog.options.length === 1 ? "" : "s"} across {groupedModels.length} provider{groupedModels.length === 1 ? "" : "s"}
              </dd>
            </div>
            <div>
              <dt>Search providers</dt>
              <dd>{surface?.webSearchProviders.map((provider) => provider.id).join(", ") || "builtin defaults"}</dd>
            </div>
          </dl>
        </Card>

        {/* ── Model catalog diagnostics ── */}
        <Card className="settings-card settings-models" hidden={activeTab !== "advanced"}>
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

export function sortProvidersForSettings(providers: RuntimeProviderSummary[]): RuntimeProviderSummary[] {
  return providers
    .map((provider, index) => ({ provider, index }))
    .sort((a, b) => {
      const credentialRank = Number(b.provider.credentialConfigured) - Number(a.provider.credentialConfigured);
      return credentialRank || a.index - b.index;
    })
    .map(({ provider }) => provider);
}

export function sortSearchProvidersForSettings(providers: RuntimeWebSearchProviderSummary[]): RuntimeWebSearchProviderSummary[] {
  return providers
    .map((provider, index) => ({ provider, index }))
    .sort((a, b) => {
      const credentialRank = Number(b.provider.credentialConfigured) - Number(a.provider.credentialConfigured);
      return credentialRank || a.index - b.index;
    })
    .map(({ provider }) => provider);
}
