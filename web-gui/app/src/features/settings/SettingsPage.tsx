import { useEffect, useMemo, useState } from "react";
import { ArrowRight } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "../../components/ui/Button";
import { useI18nSettings } from "../../i18n";
import { LANGUAGE_MODE_OPTIONS } from "../../i18n/types";
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

const settingsTabs: Array<{ key: SettingsTabKey; labelKey: string; descriptionKey: string }> = [
  { key: "general", labelKey: "settings.tabGeneral", descriptionKey: "settings.tabGeneralDesc" },
  { key: "models", labelKey: "settings.tabModels", descriptionKey: "settings.tabModelsDesc" },
  { key: "vision", labelKey: "settings.tabVision", descriptionKey: "settings.tabVisionDesc" },
  { key: "search", labelKey: "settings.tabSearch", descriptionKey: "settings.tabSearchDesc" },
  { key: "advanced", labelKey: "settings.tabAdvanced", descriptionKey: "settings.tabAdvancedDesc" },
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
  const { t } = useTranslation();
  const { languageMode, resolvedLanguageLabel, setLanguageMode } = useI18nSettings();
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
      setCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.apiKeySaved") }));
      setApiKeyDrafts((prev) => ({ ...prev, [providerId]: "" }));
    } else {
      setCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.failedSaveKey") }));
    }
  }

  async function removeApiKey(providerId: string, credentialProfile: string) {
    if (!credentialProfile) return;
    setCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.removingKey") }));
    try {
      await onDeleteCredential(credentialProfile);
      setCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.apiKeyRemoved") }));
    } catch {
      setCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.failedRemoveKey") }));
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
      t("settings.confirmRemoveSearchProvider", { provider: providerId }),
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
          ? t("settings.removedSearchProviderConfig", { provider: providerId })
          : t("settings.noSearchProviderConfigRemoved", { provider: providerId }),
    );
  }

  async function saveSearchApiKey(providerId: string, credentialProfile: string) {
    const key = searchApiKeyDrafts[providerId]?.trim();
    if (!key || !credentialProfile) return;
    setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.savingKey") }));
    const result = await onSetCredential(credentialProfile, "api_key", key);
    if (result) {
      setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.apiKeySaved") }));
      setSearchApiKeyDrafts((prev) => ({ ...prev, [providerId]: "" }));
    } else {
      setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.failedSaveKey") }));
    }
  }

  async function removeSearchApiKey(providerId: string, credentialProfile: string) {
    if (!credentialProfile) return;
    setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.removingKey") }));
    try {
      await onDeleteCredential(credentialProfile);
      setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.apiKeyRemoved") }));
    } catch {
      setSearchCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.failedRemoveKey") }));
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
      t("settings.confirmRemoveProvider", { provider: providerId }),
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
          ? t("settings.removedProviderConfig", { provider: providerId })
          : t("settings.noProviderConfigRemoved", { provider: providerId }),
    );
  }

  return (
    <section className="page settings-page" aria-label={t("settings.settingsAria")}>
      <div className="page-inner settings-inner">
        <Card className="summary-panel settings-hero">
          <span className="eyebrow">{t("settings.runtimeConfig")}</span>
          <h1>{t("settings.title")}</h1>
          <div className="settings-quickstart" aria-label={t("settings.settingsOverviewAria")}>
            <div>
              <span>{t("settings.connection")}</span>
              <strong>{connection.source === "http" ? t("settings.liveRuntime") : t("settings.previewData")}</strong>
              <small>{connection.baseUrl ?? t("settings.noApiBase")}</small>
            </div>
            <div>
              <span>{t("settings.modelProviders")}</span>
              <strong>
                {configuredProviderCount}/{surface?.providers.length ?? 0} ready
              </strong>
              <small>{t("settings.credentialHotReload")}</small>
            </div>
            <div>
              <span>{t("settings.webSearch")}</span>
              <strong>{surface?.webSearch?.enabled ? t("settings.enabled") : "Disabled"}</strong>
              <small>
                {searchProviderCount
                  ? `${configuredSearchProviderCount}/${searchProviderCount} search provider${searchProviderCount === 1 ? "" : "s"} ready`
                  : "Using builtin provider defaults"}
              </small>
            </div>
            <div>
              <span>{t("settings.tabVision")}</span>
              <strong>{surface?.visionDefault ? t("settings.pinnedModel") : "Auto-discovery"}</strong>
              <small>{surface?.visionDefault ?? `${visionModels.length} image-capable model${visionModels.length === 1 ? "" : "s"} ready`}</small>
            </div>
          </div>
        </Card>

        <div className="settings-tabs" role="tablist" aria-label={t("settings.settingsSectionsAria")}>
          {settingsTabs.map((tab) => (
            <button
              aria-selected={activeTab === tab.key}
              className={`settings-tab ${activeTab === tab.key ? "active" : ""}`}
              key={tab.key}
              onClick={() => setActiveTab(tab.key)}
              role="tab"
              type="button"
            >
              <span>{t(tab.labelKey)}</span>
              <small>{t(tab.descriptionKey)}</small>
            </button>
          ))}
        </div>

        {activeTab === "general" ? (
          <div className="settings-grid">
            <Card className="settings-card settings-primary-card">
              <div className="settings-card-head">
                <div>
                  <span className="eyebrow">{t("settings.tabGeneral")}</span>
                  <h2>{t("settings.runtimeOverview")}</h2>
                </div>
                <Button type="button" variant="secondary" disabled={runtimeConfigLoading} onClick={() => void onRefreshRuntimeConfig()}>
                  {runtimeConfigLoading ? t("settings.refreshing") : t("common.refresh")}
                </Button>
              </div>
              {runtimeConfigError ? <div className="settings-error-banner">{runtimeConfigError}</div> : null}
              <dl className="settings-list compact">
                <div>
                  <dt>{t("settings.connection")}</dt>
                  <dd>{connection.source === "http" ? t("settings.liveRuntime") : t("settings.previewData")}</dd>
                </div>
                <div>
                  <dt>{t("settings.apiBase")}</dt>
                  <dd>{connection.baseUrl ?? t("settings.notConfigured")}</dd>
                </div>
                <div>
                  <dt>{t("settings.configFile")}</dt>
                  <dd>{runtimeConfig.configFilePath ?? t("settings.notReported")}</dd>
                </div>
                <div>
                  <dt>{t("settings.providerFallback")}</dt>
                  <dd>{surface?.disableProviderFallback ? t("settings.disabled") : t("settings.enabled")}</dd>
                </div>
                <div>
                  <dt>{t("settings.modelProviders")}</dt>
                  <dd>
                    {configuredProviderCount}/{surface?.providers.length ?? 0}{t("settings.credentialReady")}
                  </dd>
                </div>
                <div>
                  <dt>{t("settings.tabSearch")}</dt>
                  <dd>{surface?.webSearch?.enabled ? t("settings.enabled") : t("settings.disabled")}</dd>
                </div>
                <div>
                  <dt>{t("settings.tabVision")}</dt>
                  <dd>{surface?.visionDefault ? surface.visionDefault : t("settings.autoDiscovery")}</dd>
                </div>
              </dl>
            </Card>

            {/* ── Language ── */}
            <Card className="settings-card settings-primary-card">
              <div className="settings-card-head">
                <div>
                  <span className="eyebrow">{t("settings.language.label")}</span>
                  <h2>{t("settings.language.label")}</h2>
                </div>
              </div>
              <p className="settings-muted">{t("settings.language.description")}</p>
              <div className="settings-form-row">
                <label>
                  <span>{t("settings.language.label")}</span>
                  <select
                    value={languageMode}
                    onChange={(event) => setLanguageMode(event.target.value as typeof languageMode)}
                  >
                    {LANGUAGE_MODE_OPTIONS.map((mode) => (
                      <option key={mode} value={mode}>
                        {mode === "system"
                          ? t("settings.language.system")
                          : mode === "en"
                            ? t("settings.language.english")
                            : t("settings.language.chineseSimplified")}
                      </option>
                    ))}
                  </select>
                </label>
              </div>
              {languageMode === "system" ? (
                <p className="settings-muted">
                  {t("settings.language.systemResolved", { language: resolvedLanguageLabel })}
                </p>
              ) : null}
            </Card>
          </div>
        ) : null}

        <div className="settings-grid">
          {/* ── Model defaults ── */}
          <Card className="settings-card settings-primary-card" hidden={activeTab !== "models"}>
            <div className="settings-card-head">
              <div>
                <span className="eyebrow">{t("settings.runtimeDefaults")}</span>
                <h2>{t("settings.modelHeading")}</h2>
              </div>
              <Button type="button" variant="secondary" disabled={runtimeConfigLoading} onClick={() => void onRefreshRuntimeConfig()}>
                {runtimeConfigLoading ? t("settings.refreshing") : t("common.refresh")}
              </Button>
            </div>
            {runtimeConfigError ? <div className="settings-error-banner">{runtimeConfigError}</div> : null}
            {!surface ? (
              <div className="settings-callout">
                <strong>{t("settings.runtimeConfigUnavailable")}</strong>
                <span>{t("settings.connectLiveRuntime")}</span>
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
                  <span>{t("settings.defaultModel")}</span>
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
                  <summary>{t("settings.tabAdvanced")}</summary>
                  <label>
                    <span>{t("settings.fallbackModels")}</span>
                    <input value={modelFallbacks} onChange={(event) => setModelFallbacks(event.target.value)} placeholder="provider/model, provider/model" />
                  </label>
                  <div className="settings-form-row">
                    <label>
                      <span>{t("settings.maxOutputTokens")}</span>
                      <input inputMode="numeric" value={runtimeMaxOutputTokens} onChange={(event) => setRuntimeMaxOutputTokens(event.target.value)} />
                    </label>
                    <label>
                      <span>{t("settings.defaultToolOutputTokens")}</span>
                      <input inputMode="numeric" value={defaultToolOutputTokens} onChange={(event) => setDefaultToolOutputTokens(event.target.value)} />
                    </label>
                    <label>
                      <span>{t("settings.maxToolOutputTokens")}</span>
                      <input inputMode="numeric" value={maxToolOutputTokens} onChange={(event) => setMaxToolOutputTokens(event.target.value)} />
                    </label>
                  </div>
                  <label className="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={disableProviderFallback}
                      onChange={(event) => setDisableProviderFallback(event.target.checked)}
                    />
                    <span>{t("settings.disableProviderFallback")}</span>
                  </label>
                </details>
                <div className="settings-actions">
                  <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                    {runtimeConfigSaving ? t("settings.saving") : "Save"}
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
                <dt>{t("settings.configFile")}</dt>
                <dd>{runtimeConfig.configFilePath ?? t("settings.notReported")}</dd>
              </div>
              <div>
                <dt>{t("settings.providerFallback")}</dt>
                <dd>{surface?.disableProviderFallback ? t("settings.disabled") : t("settings.enabled")}</dd>
              </div>
              <div>
                <dt>{t("settings.providersConfigured")}</dt>
                <dd>{configuredProviderCount}</dd>
              </div>
            </dl>
          </Card>

          {/* ── Vision defaults ── */}
          <Card className="settings-card settings-primary-card" hidden={activeTab !== "vision"}>
            <div className="settings-card-head">
              <div>
                <span className="eyebrow">{t("settings.tabVision")}</span>
                <h2>{t("settings.imageObservation")}</h2>
              </div>
            </div>
            {!surface ? (
              <div className="settings-callout">
                <strong>{t("settings.visionConfigUnavailable")}</strong>
                <span>{t("settings.connectLiveVision")}</span>
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
                  <span>{t("settings.visionDefaultModel")}</span>
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
                  {t("settings.visionAutoDiscoverHint")}
                </p>
                <div className="settings-actions">
                  <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                    {runtimeConfigSaving ? t("settings.saving") : t("settings.saveVision")}
                  </Button>
                  {visionDefault ? (
                    <StatusChip className={`settings-status ${visionProviderReady ? "available" : "unavailable"}`} tone={visionProviderReady ? "success" : "error"} iconOnly title={visionProviderReady ? t("settings.providerReady") : t("settings.providerCredentialMissing")} />
                  ) : (
                    <StatusChip className="settings-status available" tone="success" iconOnly title={t("settings.autoDiscovery")} />
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
                <span className="eyebrow">{t("settings.runtimeDefaults")}</span>
                <h2>{t("settings.webSearch")}</h2>
              </div>
            </div>
            {!surface?.webSearch ? (
              <div className="settings-callout">
                <strong>{t("settings.searchConfigUnavailable")}</strong>
                <span>{t("settings.refreshRuntimeHint")}</span>
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
                  <span>{t("settings.enableWebSearch")}</span>
                </label>
                <label>
                  <span>{t("settings.routing")}</span>
                  <select value={searchProvider || "auto"} onChange={(event) => setSearchProvider(event.target.value)}>
                    <option value="auto">{t("settings.autoRoutingOption")}</option>
                    <option value="duckduckgo">DuckDuckGo {t("settings.builtinNoKey")}</option>
                    {standardSearchProviders.map((provider) => {
                      const configured = surface.webSearchProviders.find((entry) => entry.id === provider.id);
                      const ready = provider.requiresApiKey ? configured?.credentialConfigured : Boolean(configured);
                      return (
                        <option key={provider.id} value={provider.id}>
                          {provider.label}{ready ? ` — ${t("settings.readyLabel")}` : provider.requiresApiKey ? ` — ${t("settings.apiKeyNeededLabel")}` : ""}
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
                  <span>{t("settings.allowModelNativeSearch")}</span>
                </label>
                <p className="settings-hint">
                  DuckDuckGo and native search do not need API keys. Add keys only for API-backed providers below.
                </p>
                <details className="settings-advanced">
                  <summary>{t("settings.tabAdvanced")}</summary>
                  <div className="settings-form-row">
                    <label>
                      <span>{t("settings.mode")}</span>
                      <select value={searchMode} onChange={(event) => setSearchMode(event.target.value as "single" | "fallback" | "aggregate")}>
                        <option value="single">single</option>
                        <option value="fallback">fallback</option>
                        <option value="aggregate">aggregate</option>
                      </select>
                    </label>
                    <label>
                      <span>{t("settings.providerOrder")}</span>
                      <input value={searchProviders} onChange={(event) => setSearchProviders(event.target.value)} placeholder="duckduckgo, brave" />
                    </label>
                    <label>
                      <span>{t("settings.maxResults")}</span>
                      <input inputMode="numeric" value={searchMaxResults} onChange={(event) => setSearchMaxResults(event.target.value)} />
                    </label>
                  </div>
                  <div className="settings-form-row">
                    <label>
                      <span>{t("settings.maxProviderAttempts")}</span>
                      <input inputMode="numeric" value={searchMaxProviderAttempts} onChange={(event) => setSearchMaxProviderAttempts(event.target.value)} />
                    </label>
                    <label>
                      <span>{t("settings.configuredProviders")}</span>
                      <input readOnly value={surface.webSearchProviders.map((provider) => provider.id).join(", ") || "duckduckgo builtin"} />
                    </label>
                  </div>
                </details>
                <div className="settings-actions">
                  <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                    {runtimeConfigSaving ? t("settings.saving") : "Save"}
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
              <h2>{t("settings.webSearchProviders")}</h2>
            </div>
          </div>
          {!surface ? (
            <div className="settings-callout">
              <strong>{t("settings.searchProviderConfigUnavailable")}</strong>
              <span>{t("settings.connectLiveSearchCreds")}</span>
            </div>
          ) : (
            <div className="settings-provider-list">
              <p className="settings-muted">
                Standard providers are shown as product choices. The UI creates the matching <code>web.providers.&lt;id&gt;</code> entry and stores API keys in the existing credential store.
              </p>
              <div className="settings-builtins">
                <div>
                  <strong>{t("settings.nativeSearch")}</strong>
                  <span>{t("settings.nativeSearchDesc")}</span>
                </div>
                <StatusChip className={`settings-status ${searchBuiltinProviderEnabled ? "available" : "unavailable"}`} tone={searchBuiltinProviderEnabled ? "success" : "error"} iconOnly title={searchBuiltinProviderEnabled ? t("settings.allowedLabel") : t("settings.disabled")} />
                <div>
                  <strong>DuckDuckGo</strong>
                  <span>{t("settings.duckDuckGoDesc")}</span>
                </div>
                <StatusChip className="settings-status available" tone="success" iconOnly title={t("settings.readyLabel")} />
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
                      <StatusChip className={`settings-status ${providerReady ? "available" : "unavailable"}`} tone={providerReady ? "success" : "error"} iconOnly title={providerReady ? t("settings.readyLabel") : definition.requiresApiKey ? t("settings.keyNeededLabel") : t("settings.notConfigured")} />
                    </header>
                    {!definition.requiresApiKey ? (
                      <div className="settings-form-row">
                        <label>
                          <span>{t("settings.baseUrl")}</span>
                          <input
                            value={draft.baseUrl ?? ""}
                            onChange={(event) => updateSearchProviderDraft(definition.id, { baseUrl: event.target.value })}
                            placeholder={definition.baseUrlPlaceholder ?? t("settings.optionalBaseUrl")}
                          />
                        </label>
                      </div>
                    ) : null}
                    {definition.requiresApiKey ? (
                      <label>
                        <span>{t("settings.credentialProfile")}</span>
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
                          <span>{t("settings.apiKeyFor", { profile: credentialProfile })}</span>
                          <StatusChip
                            className={`settings-status ${credentialReady ? "available" : "unavailable"}`}
                            tone={credentialReady ? "success" : "error"}
                            iconOnly
                            title={credentialReady ? t("settings.keySet") : t("settings.noKey")}
                          />
                        </div>
                        <div className="settings-form-row">
                          <label>
                            <span>API Key{credentialStoreLoading ? " (loading…)" : ""}</span>
                            <input
                              type="password"
                              placeholder={t("settings.pasteApiKeySearch")}
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
                            {t("settings.saveApiKey")}
                          </Button>
                          {credentialReady ? (
                            <Button type="button" variant="secondary" onClick={() => void removeSearchApiKey(definition.id, credentialProfile)}>
                              {t("settings.removeKey")}
                            </Button>
                          ) : null}
                          {searchCredentialMessages[definition.id] ? (
                            <span className="settings-save-message">{searchCredentialMessages[definition.id]}</span>
                          ) : null}
                        </div>
                      </div>
                    ) : null}
                    <details className="settings-advanced">
                      <summary>{t("settings.tabAdvanced")}</summary>
                      <div className="settings-form-row">
                        <label>
                          <span>{t("settings.providerId")}</span>
                          <input value={definition.id} readOnly disabled />
                        </label>
                        <label>
                          <span>{t("settings.kindLabel")}</span>
                          <input value={definition.kind} readOnly disabled />
                        </label>
                        {definition.requiresApiKey ? (
                          <label>
                            <span>{t("settings.baseUrl")}</span>
                            <input value={draft.baseUrl ?? ""} onChange={(event) => updateSearchProviderDraft(definition.id, { baseUrl: event.target.value })} placeholder={t("settings.optionalProviderDefault")} />
                          </label>
                        ) : null}
                      </div>
                    </details>
                    <div className="settings-actions">
                      <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                        {runtimeConfigSaving ? t("settings.saving") : provider ? t("settings.saveProvider", { name: definition.label }) : t("settings.enableProvider", { name: definition.label })}
                      </Button>
                      {provider ? (
                        <Button
                          type="button"
                          variant="outline"
                          disabled={runtimeConfigSaving || runtimeConfigLoading}
                          onClick={() => void removeSearchProviderConfig(definition.id)}
                        >
                          {t("settings.removeConfig")}
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
                          <small>{t("settings.unsavedSearchProvider")}</small>
                        </div>
                        <StatusChip className="settings-status unavailable" tone="error" iconOnly title={t("settings.notSaved")} />
                      </header>
                      <div className="settings-form-row">
                        <label>
                          <span>{t("settings.kindLabel")}</span>
                          <select value={draft.kind} onChange={(event) => updateSearchProviderDraft(providerId, { kind: event.target.value })}>
                            {webSearchProviderKinds.map((kind) => (
                              <option key={kind} value={kind}>
                                {kind}
                              </option>
                            ))}
                          </select>
                        </label>
                        <label>
                          <span>{t("settings.baseUrl")}</span>
                          <input value={draft.baseUrl ?? ""} onChange={(event) => updateSearchProviderDraft(providerId, { baseUrl: event.target.value })} />
                        </label>
                        <label>
                          <span>{t("settings.credentialProfile")}</span>
                          <input value={draft.credentialProfile ?? ""} onChange={(event) => updateSearchProviderDraft(providerId, { credentialProfile: event.target.value })} />
                        </label>
                      </div>
                      <div className="settings-actions">
                        <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                         {runtimeConfigSaving ? t("settings.saving") : t("settings.saveProvider", { name: providerId })}
                        </Button>
                      </div>
                    </form>
                  );
                })}
              <details className="settings-advanced">
                <summary>{t("settings.advancedCustomProvider")}</summary>
                <div className="settings-provider-editor">
                  <header>
                    <div>
                      <strong>{t("settings.addCustomSearchProvider")}</strong>
                      <small>{t("settings.customSearchProviderHint")}</small>
                    </div>
                  </header>
                  <div className="settings-form-row">
                    <label>
                      <span>{t("settings.providerId")}</span>
                      <input value={newSearchProviderId} onChange={(event) => setNewSearchProviderId(event.target.value)} placeholder="custom_search" />
                    </label>
                    <label>
                      <span>{t("settings.kindLabel")}</span>
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
              <h2>{t("settings.modelProviders")}</h2>
            </div>
          </div>
          {!surface ? (
            <div className="settings-callout">
              <strong>{t("settings.providerConfigUnavailable")}</strong>
              <span>{t("settings.connectLiveModelCreds")}</span>
            </div>
          ) : (
            <div className="settings-provider-list">
              <p className="settings-muted">
               {t("settings.providerDesc")}
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
                      <StatusChip className={`settings-status ${provider.credentialConfigured ? "available" : "unavailable"}`} tone={provider.credentialConfigured ? "success" : "error"} iconOnly title={provider.credentialConfigured ? t("settings.credReady") : t("settings.credMissing")} />
                    </header>
                    {provider.credentialConfigured && !providersWithModels.has(provider.id) ? (
                      <p className="settings-provider-hint">
                        {t("settings.noModelsForProvider")}
                      </p>
                    ) : null}
                    {/* Primary: API Key management */}
                    {draft.credentialKind === "api_key" ? (
                      <div className="settings-api-key-section">
                        <div className="settings-api-key-header">
                          <span>{t("settings.apiKeyFor", { profile: effectiveProfile })}</span>
                          <StatusChip
                            className={`settings-status ${isCredentialProfileConfigured(effectiveProfile) ? "available" : "unavailable"}`}
                            tone={isCredentialProfileConfigured(effectiveProfile) ? "success" : "error"}
                            iconOnly
                            title={isCredentialProfileConfigured(effectiveProfile) ? t("settings.keySet") : t("settings.noKey")}
                          />
                        </div>
                        <div className="settings-form-row">
                          <label>
                            <span>API Key{credentialStoreLoading ? " (loading…)" : ""}</span>
                            <input
                              type="password"
                              placeholder={t("settings.pasteApiKeyProfile")}
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
                           {t("settings.saveApiKey")}
                          </Button>
                          {isCredentialProfileConfigured(effectiveProfile) ? (
                            <Button
                              type="button"
                              variant="secondary"
                              onClick={() => void removeApiKey(provider.id, effectiveProfile)}
                            >
                             {t("settings.removeKey")}
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
                            <span>{t("settings.connectedViaOAuth")}</span>
                            <StatusChip className="settings-status available" tone="success" iconOnly title={t("settings.credReady")} />
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
                              Open Device Login Page <ArrowRight size={14} />
                            </Button>
                            <p className="settings-hint">Enter this code on the page:</p>
                            <div className="settings-device-login-code">{codexDeviceLogin.userCode}</div>
                            <p className="settings-muted">Waiting for authorization…</p>
                            <Button type="button" variant="outline" onClick={onClearCodexDeviceLogin}>{t("common.cancel")}</Button>
                          </div>
                        ) : null}
                        {codexDeviceLogin.status === "completed" ? (
                          <div className="settings-device-login-panel">
                            <StatusChip className="settings-status available" tone="success" iconOnly title={t("settings.loginSuccessful")} />
                            <Button type="button" variant="outline" onClick={onClearCodexDeviceLogin}>{t("common.dismiss")}</Button>
                          </div>
                        ) : null}
                      </div>
                    ) : null}
                    {draft.credentialKind !== "api_key" && draft.credentialKind !== "oauth" ? (
                      <p className="settings-hint">
                        This provider uses <code>{draft.credentialKind}</code>{t("settings.authVia")}<code>{draft.credentialSource}</code>{t("settings.configureIn")}<code>{runtimeConfig.configFilePath ?? "config.json"}</code>.
                      </p>
                    ) : null}
                    {/* Advanced: full provider config */}
                    <details className="settings-advanced">
                      <summary>{t("settings.tabAdvanced")}</summary>
                      <div className="settings-form-row">
                        <label>
                          <span>{t("settings.transportLabel")} <small className="settings-muted">{t("settings.readOnlySuffix")}</small></span>
                          <input value={provider.transport} readOnly disabled />
                        </label>
                        <label>
                          <span>{t("settings.baseUrl")}</span>
                          <input value={draft.baseUrl} onChange={(event) => updateProviderDraft(provider.id, { baseUrl: event.target.value })} />
                        </label>
                      </div>
                    </details>
                    <div className="settings-actions">
                      <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                       {runtimeConfigSaving ? t("settings.saving") : t("settings.saveProvider", { name: provider.id })}
                      </Button>
                      <Button
                        type="button"
                        variant="outline"
                        disabled={runtimeConfigSaving || runtimeConfigLoading || !provider.configuredInConfig}
                        title={
                          provider.configuredInConfig
                           ? t("settings.removeConfigHint")
                           : t("settings.removeConfigDisabled")
                        }
                        onClick={() => void removeProviderConfig(provider.id)}
                      >
                       {t("settings.removeConfig")}
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
              <span className="eyebrow">{t("settings.tabAdvanced")}</span>
              <h2>{t("settings.rawConfig")}</h2>
            </div>
            <Button type="button" variant="secondary" disabled={runtimeConfigLoading} onClick={() => void onRefreshRuntimeConfig()}>
              {runtimeConfigLoading ? t("settings.refreshing") : t("common.refresh")}
            </Button>
          </div>
          <div className="settings-callout">
            <strong>{t("settings.rawConfigNotExposed")}</strong>
            <span>{t("settings.rawConfigHint")}</span>
          </div>
          <dl className="settings-list compact">
            <div>
              <dt>{t("settings.configFile")}</dt>
              <dd>{runtimeConfig.configFilePath ?? t("settings.notReported")}</dd>
            </div>
            <div>
              <dt>{t("settings.modelCatalogLabel")}</dt>
              <dd>
                {t("settings.modelCatalogSummary", { models: modelCatalog.options.length, providers: groupedModels.length })}
              </dd>
            </div>
            <div>
              <dt>{t("settings.searchProvidersLabel")}</dt>
              <dd>{surface?.webSearchProviders.map((provider) => provider.id).join(", ") || t("settings.builtinDefaultsValue")}</dd>
            </div>
          </dl>
        </Card>

        {/* ── Model catalog diagnostics ── */}
        <Card className="settings-card settings-models" hidden={activeTab !== "advanced"}>
          <div className="settings-card-head">
            <div>
              <span className="eyebrow">{t("settings.modelsProvidersLabel")}</span>
              <h2>{t("settings.modelCatalogDiagnostics")}</h2>
            </div>
            <Button type="button" variant="secondary" disabled={modelCatalogLoading} onClick={() => void onRefreshModels()}>
              {modelCatalogLoading ? t("settings.refreshing") : t("common.refresh")}
            </Button>
          </div>

          {modelCatalogError ? <div className="settings-error-banner">{modelCatalogError}</div> : null}
          {!modelCatalogLoading && modelCatalog.options.length === 0 ? (
            <EmptyState className="settings-empty" title={t("settings.noModelsReturned")} description={t("settings.noModelsReturnedDesc")} />
          ) : null}

          <p className="settings-muted">{t("settings.modelCatalogDesc")}</p>
          <details className="settings-diagnostics">
            <summary>
              Show {t("settings.modelCatalogSummary", { models: modelCatalog.options.length, providers: groupedModels.length })}
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
                          <StatusChip className={`settings-status ${model.available ? "available" : "unavailable"}`} tone={model.available ? "success" : "error"} iconOnly title={model.available ? t("settings.availableLabel") : t("settings.unavailableLabel")} />
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
