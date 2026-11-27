import { useEffect, useMemo, useState } from "react";
import {
  Activity,
  Compass,
  Cpu,
  Globe,
  Image as ImageIcon,
  KeyRound,
  QrCode,
  SlidersHorizontal,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { rememberModel } from "../../lib/model-preferences";
import { providerIsConnected } from "../../lib/model-presentation";
import type {
  CodexDeviceLoginState,
  CredentialMutationResult,
  CredentialStoreState,
  RuntimeConfigState,
  RuntimeConnection,
  RuntimeLocalOnnxPresetStatus,
  RuntimeModelCatalog,
} from "../../runtime/types";
import { SetupStatusCard, setupIsReady } from "../onboarding/SetupStatusCard";
import {
  buildAdvisoryToolConfigUpdates,
  buildDecisionConfigUpdates,
  buildImageGenerationConfigUpdates,
  buildProviderDiscoveryConfigUpdates,
  buildSearchProviderConfigUpdates,
  buildStandardSearchProviderDefinitions,
  buildVisionConfigUpdates,
  defaultSearchProviderDraft,
  groupModelsByProvider,
  isAbsoluteHttpUrl,
  isDecisionCatalogRoute,
  isImageGenerationSelectableModel,
  isProviderKeyCredential,
  isVisionSelectableModel,
  numberFromInput,
  shouldHydrateSettingsDrafts,
  sortProvidersForSettings,
  sortSearchProvidersForSettings,
  splitCsv,
  type CustomProviderDraft,
  type ProviderDraft,
  type SearchProviderDraft,
} from "./settings-helpers";
import { readSettingsTab, rememberSettingsTab, type SettingsTabKey } from "./settings-preferences";
import { AccessSettingsSection } from "./sections/AccessSettingsSection";
import { AdvancedSettingsSection } from "./sections/AdvancedSettingsSection";
import { DecisionSettingsSection } from "./sections/DecisionSettingsSection";
import { GeneralSettingsSection } from "./sections/GeneralSettingsSection";
import { LanguageModelSettingsSection } from "./sections/LanguageModelSettingsSection";
import { ModelProvidersSection } from "./sections/ModelProvidersSection";
import { SearchSettingsSection } from "./sections/SearchSettingsSection";
import { VisionSettingsSection } from "./sections/VisionSettingsSection";
import { useServeStatus } from "./useServeStatus";

export * from "./settings-helpers";

interface SettingsPageProps {
  connection: RuntimeConnection;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  modelCatalogError?: string;
  runtimeConfig: RuntimeConfigState;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  runtimeConfigError?: string;
  onStartOnboarding?: () => void;
  setupTarget?: { action: string; provider: string | null };
  onRefreshModels: () => Promise<void>;
  onRefreshRuntimeConfig: () => Promise<void>;
  onUpdateRuntimeConfig: (updates: Array<{ key: string; value?: unknown; unset?: boolean }>) => Promise<RuntimeConfigState | undefined>;
  onDownloadLocalOnnxPreset: (preset: string) => Promise<RuntimeLocalOnnxPresetStatus>;
  onCancelLocalOnnxPreset: (preset: string) => Promise<RuntimeLocalOnnxPresetStatus>;
  credentialStore: CredentialStoreState;
  credentialStoreLoading: boolean;
  onRefreshCredentialStore: () => Promise<void>;
  onSetCredential: (profile: string, kind: string, material: string) => Promise<CredentialMutationResult | undefined>;
  onDeleteCredential: (profile: string) => Promise<void>;
  codexDeviceLogin: CodexDeviceLoginState;
  onStartCodexDeviceLogin: (providerId?: string) => Promise<void>;
  onClearCodexDeviceLogin: () => void;
  onActiveTabChange?: (tab: SettingsTabKey) => void;
}

interface SettingsNavItem {
  key: SettingsTabKey;
  labelKey: string;
  descriptionKey: string;
  icon: LucideIcon;
  group: "system" | "capabilities" | "advanced";
}

const settingsNavItems: SettingsNavItem[] = [
  { key: "general", labelKey: "settings.tabGeneral", descriptionKey: "settings.tabGeneralDesc", icon: SlidersHorizontal, group: "system" },
  { key: "access", labelKey: "settings.tabAccess", descriptionKey: "settings.tabAccessDesc", icon: QrCode, group: "system" },
  { key: "providers", labelKey: "settings.tabProviders", descriptionKey: "settings.tabProvidersDesc", icon: KeyRound, group: "capabilities" },
  { key: "models", labelKey: "settings.tabModels", descriptionKey: "settings.tabModelsDesc", icon: Cpu, group: "capabilities" },
  { key: "vision", labelKey: "settings.tabVision", descriptionKey: "settings.tabVisionDesc", icon: ImageIcon, group: "capabilities" },
  { key: "search", labelKey: "settings.tabSearch", descriptionKey: "settings.tabSearchDesc", icon: Globe, group: "capabilities" },
  { key: "decision", labelKey: "settings.tabDecision", descriptionKey: "settings.tabDecisionDesc", icon: Compass, group: "capabilities" },
  { key: "advanced", labelKey: "settings.tabAdvanced", descriptionKey: "settings.tabAdvancedDesc", icon: Activity, group: "advanced" },
];

const settingsNavGroups: Array<{ key: SettingsNavItem["group"]; labelKey: string }> = [
  { key: "system", labelKey: "settings.navGroupSystem" },
  { key: "capabilities", labelKey: "settings.navGroupCapabilities" },
  { key: "advanced", labelKey: "settings.navGroupAdvanced" },
];

export function SettingsPage({
  connection,
  modelCatalog,
  modelCatalogLoading,
  modelCatalogError,
  runtimeConfig,
  runtimeConfigLoading,
  runtimeConfigSaving,
  runtimeConfigError,
  onStartOnboarding,
  setupTarget,
  onRefreshModels,
  onRefreshRuntimeConfig,
  onUpdateRuntimeConfig,
  onDownloadLocalOnnxPreset,
  onCancelLocalOnnxPreset,
  credentialStore,
  credentialStoreLoading,
  onRefreshCredentialStore,
  onSetCredential,
  onDeleteCredential,
  codexDeviceLogin,
  onStartCodexDeviceLogin,
  onClearCodexDeviceLogin,
  onActiveTabChange,
}: SettingsPageProps) {
  const { t } = useTranslation();
  const serve = useServeStatus(connection);
  const groupedModels = useMemo(() => groupModelsByProvider(modelCatalog.options), [modelCatalog.options]);
  const decisionModels = useMemo(
    () => modelCatalog.options.filter((model) => model.decisionCapable),
    [modelCatalog.options],
  );
  const surface = runtimeConfig.surface;

  const [modelDefault, setModelDefault] = useState("");
  const [modelFallbacks, setModelFallbacks] = useState<string[]>([]);
  const [visionDefault, setVisionDefault] = useState("");
  const [imageGenDefault, setImageGenDefault] = useState("");
  const [decisionEnabled, setDecisionEnabled] = useState(false);
  const [decisionModel, setDecisionModel] = useState("");
  const [decisionModelDir, setDecisionModelDir] = useState("");
  const [decisionVariant, setDecisionVariant] = useState("q4f16");
  const [decisionNumThreads, setDecisionNumThreads] = useState("1");
  const [decisionChecksum, setDecisionChecksum] = useState("");
  const [decisionPreset, setDecisionPreset] = useState("jev-selector-q4f16");
  const [advisoryToolEnabled, setAdvisoryToolEnabled] = useState(false);
  const [advisoryToolMaxCalls, setAdvisoryToolMaxCalls] = useState("4");
  const [advisoryToolTimeout, setAdvisoryToolTimeout] = useState("1500");
  const [advisoryToolMinConfidence, setAdvisoryToolMinConfidence] = useState("0");
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
  const [providerSearch, setProviderSearch] = useState("");
  const [providerFilter, setProviderFilter] = useState<"connected" | "all">("connected");
  const [editingProvider, setEditingProvider] = useState<string | null>(null);
  const [customProviderDraft, setCustomProviderDraft] = useState<CustomProviderDraft | null>(null);
  const [customProviderMessage, setCustomProviderMessage] = useState<string | undefined>(undefined);
  const [providerDrafts, setProviderDrafts] = useState<Record<string, ProviderDraft>>({});
  const [saveMessage, setSaveMessage] = useState<string | undefined>();
  const [limitsSaveMessage, setLimitsSaveMessage] = useState<string | undefined>();
  const [searchSaveMessage, setSearchSaveMessage] = useState<string | undefined>();
  const [searchProviderSaveMessage, setSearchProviderSaveMessage] = useState<string | undefined>();
  const [visionSaveMessage, setVisionSaveMessage] = useState<string | undefined>();
  const [imageGenSaveMessage, setImageGenSaveMessage] = useState<string | undefined>();
  const [decisionSaveMessage, setDecisionSaveMessage] = useState<string | undefined>();
  const [providerSaveMessage, setProviderSaveMessage] = useState<string | undefined>();
  const [apiKeyDrafts, setApiKeyDrafts] = useState<Record<string, string>>({});
  const [searchApiKeyDrafts, setSearchApiKeyDrafts] = useState<Record<string, string>>({});
  const [credentialMessages, setCredentialMessages] = useState<Record<string, string>>({});
  const [searchCredentialMessages, setSearchCredentialMessages] = useState<Record<string, string>>({});
  const [deviceLoginProviderId, setDeviceLoginProviderId] = useState<string | null>(null);

  const [activeTab, updateActiveTab] = useState<SettingsTabKey>(() => readSettingsTab(window.location.search));

  useEffect(() => {
    onActiveTabChange?.(activeTab);
  }, [activeTab, onActiveTabChange]);

  function setActiveTab(tab: SettingsTabKey) {
    updateActiveTab(tab);
    rememberSettingsTab(tab);
    const url = new URL(window.location.href);
    if (url.searchParams.has("tab")) {
      url.searchParams.set("tab", tab);
      window.history.replaceState(window.history.state, "", url);
    }
  }

  function openProviderEditor(providerId?: string | null) {
    setActiveTab("providers");
    setEditingProvider(providerId ?? null);
    setProviderFilter("all");
    setProviderSearch("");
    requestAnimationFrame(() => {
      const target = document.getElementById("setup-provider-editor");
      target?.scrollIntoView({ block: "center" });
      target?.querySelector<HTMLElement>("input, button, select")?.focus();
    });
  }

  function repairSetup(action: string, provider: string | null) {
    if (action === "configure_model_credential") {
      openProviderEditor(provider);
      return;
    }
    setActiveTab("models");
    requestAnimationFrame(() => {
      const target = document.getElementById("setup-default-model");
      target?.scrollIntoView({ block: "center" });
      target?.querySelector<HTMLElement>("input, button, select")?.focus();
    });
  }

  useEffect(() => {
    if (setupTarget) repairSetup(setupTarget.action, setupTarget.provider);
  }, [setupTarget]);

  const visionModels = useMemo(() => modelCatalog.options.filter(isVisionSelectableModel), [modelCatalog.options]);
  const imageGenModels = useMemo(() => modelCatalog.options.filter(isImageGenerationSelectableModel), [modelCatalog.options]);
  const providersWithModels = useMemo(
    () => new Set(modelCatalog.options.map((model) => model.routeProvider)),
    [modelCatalog.options],
  );
  const sortedProviders = useMemo(
    () => sortProvidersForSettings(surface?.providers ?? []),
    [surface?.providers],
  );
  const usedProviders = useMemo(() => {
    const selected = new Set([surface?.modelDefault, surface?.visionDefault, surface?.imageGenerationDefault, ...(surface?.modelFallbacks ?? [])]);
    return new Set(modelCatalog.options.filter((model) => selected.has(model.routeRef)).map((model) => model.routeProvider));
  }, [surface, modelCatalog.options]);

  const sortedSearchProviders = useMemo(
    () => sortSearchProvidersForSettings(surface?.webSearchProviders ?? []),
    [surface?.webSearchProviders],
  );
  const standardSearchProviders = useMemo(
    () => buildStandardSearchProviderDefinitions(surface?.availableSearchProviderKinds ?? []),
    [surface?.availableSearchProviderKinds],
  );
  const standardSearchProviderById = useMemo(
    () => new Map(standardSearchProviders.map((provider) => [provider.id, provider])),
    [standardSearchProviders],
  );
  const standardSearchProviderIds = useMemo(
    () => new Set(standardSearchProviders.map((provider) => provider.id)),
    [standardSearchProviders],
  );
  const webSearchProviderKinds = useMemo(
    () => surface?.availableSearchProviderKinds.map(({ kind }) => kind) ?? [],
    [surface?.availableSearchProviderKinds],
  );
  const groupedSearchProviders = useMemo(
    () => [
      { category: "api" as const, providers: standardSearchProviders.filter((provider) => provider.category === "api") },
      { category: "selfHosted" as const, providers: standardSearchProviders.filter((provider) => provider.category === "selfHosted") },
    ].filter((group) => group.providers.length > 0),
    [standardSearchProviders],
  );
  const customSearchProviderIds = useMemo(() => {
    if (!surface) return [];
    return sortedSearchProviders
      .filter((provider) => !standardSearchProviderIds.has(provider.id))
      .map((provider) => provider.id)
      .concat(
        Object.keys(searchProviderDrafts).filter(
          (providerId) =>
            !surface.webSearchProviders.some((provider) => provider.id === providerId)
            && !standardSearchProviderIds.has(providerId),
        ),
      );
  }, [searchProviderDrafts, sortedSearchProviders, standardSearchProviderIds, surface]);

  useEffect(() => {
    if (!surface || !shouldHydrateSettingsDrafts(runtimeConfig)) return;
    setModelDefault(surface.modelDefault);
    setModelFallbacks(surface.modelFallbacks ?? []);
    setVisionDefault(surface.visionDefault ?? "");
    setImageGenDefault(surface.imageGenerationDefault ?? "");
    setDecisionEnabled(surface.decision?.enabled ?? false);
    setDecisionModel(surface.decision?.model ?? "");
    setDecisionPreset(surface.decision?.localOnnx.preset ?? "jev-selector-q4f16");
    setDecisionModelDir(surface.decision?.localOnnx.modelDir ?? "");
    setDecisionVariant(surface.decision?.localOnnx.variant ?? "q4f16");
    setDecisionNumThreads(String(surface.decision?.localOnnx.numThreads ?? 1));
    setDecisionChecksum(surface.decision?.localOnnx.checksum ?? "");
    setAdvisoryToolEnabled(surface.decision?.tools.enabled ?? false);
    setAdvisoryToolMaxCalls(String(surface.decision?.tools.maxCallsPerTurn ?? 4));
    setAdvisoryToolTimeout(String(surface.decision?.tools.timeoutMs ?? 1500));
    setAdvisoryToolMinConfidence(String(surface.decision?.tools.minConfidencePercent ?? 0));
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
            engineId: provider.engineId ?? "",
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
            oauthSupported: provider.oauthSupported,
            apiKeySupported: provider.apiKeySupported,
            credentialSource: provider.credentialSource,
            credentialKind: provider.credentialKind,
            credentialEnv: provider.credentialEnv ?? "",
            credentialProfile: provider.credentialProfile ?? "",
            credentialExternal: provider.credentialExternal ?? "",
            discoveryKind: provider.discovery?.kind ?? "",
            discoveryPath: provider.discovery?.path ?? "",
            discoveryDecoder: provider.discovery?.decoder ?? "",
          },
        ]),
      ),
    );
    setSaveMessage(undefined);
    setLimitsSaveMessage(undefined);
    setSearchSaveMessage(undefined);
    setSearchProviderSaveMessage(undefined);
    setVisionSaveMessage(undefined);
    setImageGenSaveMessage(undefined);
  }, [runtimeConfig, surface]);

  useEffect(() => {
    void onRefreshCredentialStore();
  }, [onRefreshCredentialStore]);

  useEffect(() => {
    if (codexDeviceLogin.status === "completed" && deviceLoginProviderId) {
      const providerId = deviceLoginProviderId;
      const draft = providerDrafts[providerId];
      if (draft && draft.credentialKind === "oauth") {
        void onUpdateRuntimeConfig([
          { key: `providers.${providerId}.auth.source`, value: "credential_profile" },
          { key: `providers.${providerId}.auth.kind`, value: "oauth" },
          { key: `providers.${providerId}.auth.profile`, value: draft.credentialProfile?.trim() || providerId },
        ]).then(() => setDeviceLoginProviderId(null));
      }
    }
  }, [codexDeviceLogin.status, deviceLoginProviderId, providerDrafts, onUpdateRuntimeConfig]);

  function isCredentialProfileConfigured(profile: string): boolean {
    return credentialStore.profiles.some((p) => p.profile === profile && p.configured);
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
  const configuredProviderCount =
    surface?.providers.filter((provider) => providerIsConnected(provider, usedProviders)).length ?? 0;

  const visionRouteProvider = modelCatalog.options.find((model) => model.routeRef === visionDefault)?.routeProvider
    ?? visionDefault.split("/")[0];
  const visionProviderReady = visionDefault
    ? surface?.providers.find((provider) => provider.id === visionRouteProvider)?.credentialConfigured
    : undefined;
  const imageGenRouteProvider = modelCatalog.options.find((model) => model.routeRef === imageGenDefault)?.routeProvider
    ?? imageGenDefault.split("/")[0];
  const imageGenProviderReady = imageGenDefault
    ? surface?.providers.find((provider) => provider.id === imageGenRouteProvider)?.credentialConfigured
    : undefined;

  async function saveLanguageModelsConfig() {
    setSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([
      { key: "model.default", value: modelDefault.trim() },
      { key: "model.fallbacks", value: modelFallbacks },
    ]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    if (!rejected.length) [modelDefault, ...modelFallbacks].filter(Boolean).forEach(rememberModel);
    setSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : result.changed
          ? setupIsReady(result)
            ? t("setup.ready")
            : t("setup.savedIncomplete")
          : t("settings.noConfigChanges"),
    );
  }

  async function selectProviderModelAsDefault(routeRef: string) {
    setModelDefault(routeRef);
    setProviderSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([{ key: "model.default", value: routeRef }]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    if (!rejected.length) rememberModel(routeRef);
    setProviderSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : t("settings.defaultModelSaved", { model: routeRef }),
    );
  }

  async function saveRuntimeLimitsConfig() {
    setLimitsSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([
      { key: "runtime.default_tool_output_tokens", value: numberFromInput(defaultToolOutputTokens) },
      { key: "runtime.max_tool_output_tokens", value: numberFromInput(maxToolOutputTokens) },
      { key: "runtime.disable_provider_fallback", value: disableProviderFallback },
    ]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setLimitsSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : result.changed
          ? t("settings.savedConfig")
          : t("settings.noConfigChanges"),
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
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : result.changed
          ? t("settings.savedSearchConfig")
          : t("settings.noSearchConfigChanges"),
    );
  }

  function updateSearchProviderDraft(providerId: string, patch: Partial<SearchProviderDraft>) {
    const definition = standardSearchProviderById.get(providerId);
    setSearchProviderDrafts((drafts) => ({
      ...drafts,
      [providerId]: {
        ...(drafts[providerId] ?? defaultSearchProviderDraft(providerId, definition)),
        ...patch,
      },
    }));
  }

  function addSearchProviderDraft(): string | undefined {
    const providerId = newSearchProviderId.trim();
    if (!providerId) return undefined;
    updateSearchProviderDraft(providerId, {
      kind: newSearchProviderKind,
      credentialProfile: `${providerId}:default`,
    });
    setNewSearchProviderId("");
    setSearchProviderSaveMessage(t("settings.preparedCustomSearchProvider", { provider: providerId }));
    return providerId;
  }

  async function saveSearchProviderConfig(providerId: string) {
    const draft = searchProviderDrafts[providerId] ?? defaultSearchProviderDraft(providerId, standardSearchProviderById.get(providerId));
    const capabilities = surface?.availableSearchProviderKinds.find(({ kind }) => kind === draft.kind)?.capabilities;
    setSearchProviderSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig(buildSearchProviderConfigUpdates(providerId, draft, capabilities));
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setSearchProviderSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : result.changed
          ? t("settings.savedSearchConfig")
          : t("settings.noSearchProviderConfigChanges"),
    );
  }

  async function removeSearchProviderConfig(providerId: string) {
    const confirmed = window.confirm(t("settings.confirmRemoveSearchProvider", { provider: providerId }));
    if (!confirmed) return;
    setSearchProviderSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([{ key: `web.providers.${providerId}`, unset: true }]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setSearchProviderSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
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
    if (!rejected.length) [visionDefault].filter(Boolean).forEach(rememberModel);
    setVisionSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : result.changed
          ? t("settings.savedVisionConfig")
          : t("settings.noVisionConfigChanges"),
    );
  }

  async function saveImageGenConfig() {
    setImageGenSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig(buildImageGenerationConfigUpdates(imageGenDefault));
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    if (!rejected.length && imageGenDefault) rememberModel(imageGenDefault);
    setImageGenSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : result.changed
          ? t("settings.savedImageGenConfig")
          : t("settings.noImageGenConfigChanges"),
    );
  }

  async function saveDecisionConfig() {
    setDecisionSaveMessage(undefined);
    const decisionModelIsCatalogRoute = isDecisionCatalogRoute(decisionModel, decisionModels);
    const result = await onUpdateRuntimeConfig(
      buildDecisionConfigUpdates(
        decisionEnabled,
        decisionModel,
        decisionModelDir,
        decisionVariant,
        decisionNumThreads,
        decisionChecksum,
        decisionPreset,
        decisionModelIsCatalogRoute ? "set" : "preserve",
      ).concat(
        buildAdvisoryToolConfigUpdates(
          advisoryToolEnabled,
          advisoryToolMaxCalls,
          advisoryToolTimeout,
          advisoryToolMinConfidence,
        ),
      ),
    );
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setDecisionSaveMessage(
      rejected.length
        ? t("settings.decisionRejected", { count: rejected.length })
        : result.changed
          ? t("settings.decisionSaved")
          : t("settings.decisionNoChanges"),
    );
  }

  function updateProviderDraft(providerId: string, patch: Partial<ProviderDraft>) {
    setProviderDrafts((drafts) => ({
      ...drafts,
      [providerId]: {
        ...(drafts[providerId] ?? {
          transport: "openai_responses",
          baseUrl: "",
          oauthSupported: false,
          apiKeySupported: true,
          credentialSource: "env",
          credentialKind: "api_key",
          credentialEnv: "",
          credentialProfile: "",
          credentialExternal: "",
          discoveryKind: "",
          discoveryPath: "",
          discoveryDecoder: "",
        }),
        ...patch,
      },
    }));
  }

  async function saveProviderConfig(providerId: string) {
    const draft = providerDrafts[providerId];
    if (!draft) return;
    setProviderSaveMessage(undefined);

    const keyCredential = isProviderKeyCredential(draft.credentialKind);
    const key = keyCredential ? apiKeyDrafts[providerId]?.trim() : undefined;
    const useProfile = keyCredential && (Boolean(key) || draft.credentialSource === "credential_profile");
    const effectiveProfile = useProfile
      ? (draft.credentialProfile?.trim() || `${providerId}:default`)
      : (draft.credentialProfile?.trim() ?? "");
    const effectiveSource = useProfile ? "credential_profile" : draft.credentialSource;
    const effectiveEnv = useProfile ? "" : (draft.credentialEnv?.trim() ?? "");

    if (keyCredential && key) {
      setCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.savingKey") }));
      const credResult = await onSetCredential(effectiveProfile, draft.credentialKind, key);
      if (credResult) {
        setCredentialMessages((prev) => ({
          ...prev,
          [providerId]: credResult.reloadGeneration > 0 ? t("setup.applying") : t("settings.apiKeySaved"),
        }));
        setApiKeyDrafts((prev) => ({ ...prev, [providerId]: "" }));
      } else {
        setCredentialMessages((prev) => ({ ...prev, [providerId]: t("settings.failedSaveKey") }));
        return;
      }
    }

    const result = await onUpdateRuntimeConfig([
      { key: `providers.${providerId}.base_url`, value: draft.baseUrl.trim() },
      { key: `providers.${providerId}.auth.source`, value: effectiveSource },
      { key: `providers.${providerId}.auth.kind`, value: draft.credentialKind },
      { key: `providers.${providerId}.auth.env`, value: effectiveEnv },
      { key: `providers.${providerId}.auth.profile`, value: effectiveProfile },
      { key: `providers.${providerId}.auth.external`, value: useProfile ? "" : (draft.credentialExternal?.trim() ?? "") },
      ...buildProviderDiscoveryConfigUpdates(providerId, draft),
    ]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setProviderSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : result.changed
          ? setupIsReady(result)
            ? t("setup.ready")
            : t("setup.savedIncomplete")
          : t("settings.noProviderConfigChanges"),
    );
  }

  function customProviderDraftProblem(draft: CustomProviderDraft): string | undefined {
    const id = draft.id.trim();
    if (!/^[a-z0-9_-]+$/.test(id)) return t("settings.customProviderInvalidId");
    if (sortedProviders.some((provider) => provider.id === id)) {
      return t("settings.customProviderIdTaken", { provider: id });
    }
    if (!isAbsoluteHttpUrl(draft.baseUrl.trim())) {
      return t("settings.customProviderInvalidBaseUrl");
    }
    const path = draft.discoveryPath.trim();
    if (path && !path.startsWith("/")) {
      return t("settings.customProviderInvalidDiscoveryPath");
    }
    if (draft.discoveryKind === "disabled" && path) {
      return t("settings.customProviderDiscoveryPathWithDisabled");
    }
    return undefined;
  }

  async function createCustomProvider() {
    if (!customProviderDraft) return;
    const draft = customProviderDraft;
    const problem = customProviderDraftProblem(draft);
    if (problem) {
      setCustomProviderMessage(problem);
      return;
    }
    setCustomProviderMessage(undefined);
    const id = draft.id.trim();
    const key = draft.apiKey.trim();
    const profile = `${id}:default`;
    if (key) {
      const credResult = await onSetCredential(profile, "api_key", key);
      if (!credResult) {
        setCustomProviderMessage(t("settings.failedSaveKey"));
        return;
      }
    }
    const discoveryPath = draft.discoveryPath.trim();
    const result = await onUpdateRuntimeConfig([
      { key: `providers.${id}.transport`, value: draft.transport },
      { key: `providers.${id}.base_url`, value: draft.baseUrl.trim() },
      ...(key
        ? [
            { key: `providers.${id}.auth.source`, value: "credential_profile" },
            { key: `providers.${id}.auth.kind`, value: "api_key" },
            { key: `providers.${id}.auth.profile`, value: profile },
          ]
        : [
            { key: `providers.${id}.auth.source`, value: "none" },
            { key: `providers.${id}.auth.kind`, value: "none" },
          ]),
      { key: `providers.${id}.discovery.kind`, value: draft.discoveryKind },
      ...(discoveryPath ? [{ key: `providers.${id}.discovery.path`, value: discoveryPath }] : []),
    ]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    if (rejected.length) {
      setCustomProviderMessage(rejected[0]?.reason ?? t("settings.customProviderRejected"));
      return;
    }
    setCustomProviderMessage(t("settings.customProviderCreated", { provider: id }));
    setCustomProviderDraft(null);
    setProviderFilter("all");
    setProviderSearch("");
    setEditingProvider(id);
  }

  async function removeProviderConfig(providerId: string) {
    const confirmed = window.confirm(t("settings.confirmRemoveProvider", { provider: providerId }));
    if (!confirmed) return;
    setProviderSaveMessage(undefined);
    const result = await onUpdateRuntimeConfig([{ key: `providers.${providerId}`, unset: true }]);
    if (!result) return;
    const rejected = result.results?.filter((entry) => entry.effect === "rejected") ?? [];
    setProviderSaveMessage(
      rejected.length
        ? t("settings.rejectedSettingsCount", { count: rejected.length })
        : result.changed
          ? t("settings.removedProviderConfig", { provider: providerId })
          : t("settings.noProviderConfigRemoved", { provider: providerId }),
    );
  }

  function renderNavBadge(key: SettingsTabKey) {
    if (key === "providers" && surface) {
      return <span className="settings-nav-badge">{configuredProviderCount}</span>;
    }
    if (key === "access" && serve.status?.serving) {
      return <span className="settings-nav-badge is-active">{t("settings.serve.states.serving")}</span>;
    }
    if (key === "search" && surface?.webSearch) {
      return (
        <span className={`settings-nav-badge${surface.webSearch.enabled ? "" : " is-muted"}`}>
          {surface.webSearch.enabled ? t("settings.enabled") : t("settings.disabled")}
        </span>
      );
    }
    return null;
  }

  const isQuietSetupReady = !runtimeConfigLoading && !runtimeConfigSaving && setupIsReady(runtimeConfig) && !onStartOnboarding;

  return (
    <section className="page settings-page workspace-page" aria-label={t("settings.settingsAria")}>
      <div className="page-inner settings-inner workspace-page-content">
        <div className={isQuietSetupReady ? "setup-status-quiet" : undefined}>
          <SetupStatusCard
            config={{ ...runtimeConfig, error: runtimeConfigError ?? runtimeConfig.error }}
            loading={runtimeConfigLoading}
            saving={runtimeConfigSaving}
            onRepair={repairSetup}
            onRetry={() => void onRefreshRuntimeConfig()}
            onStart={onStartOnboarding}
          />
        </div>

        <div className="settings-workspace-layout">
          <aside className="settings-sidebar-nav">
            <div
              className="settings-tabs settings-vertical-tabs"
              role="tablist"
              aria-orientation="vertical"
              aria-label={t("settings.settingsSectionsAria")}
            >
              {settingsNavGroups.map((group) => {
                const items = settingsNavItems.filter((item) => item.group === group.key);
                return (
                  <div className="settings-nav-group" role="presentation" key={group.key}>
                    <div className="settings-nav-group-title" role="presentation">
                      {t(group.labelKey)}
                    </div>
                    {items.map((tab) => {
                      const Icon = tab.icon;
                      return (
                        <button
                          id={`settings-tab-${tab.key}`}
                          aria-controls="settings-panel"
                          aria-selected={activeTab === tab.key}
                          tabIndex={activeTab === tab.key ? 0 : -1}
                          title={t(tab.descriptionKey)}
                          onKeyDown={(event) => {
                            const index = settingsNavItems.findIndex((item) => item.key === tab.key);
                            const next =
                              event.key === "ArrowRight" || event.key === "ArrowDown"
                                ? (index + 1) % settingsNavItems.length
                                : event.key === "ArrowLeft" || event.key === "ArrowUp"
                                  ? (index + settingsNavItems.length - 1) % settingsNavItems.length
                                  : event.key === "Home"
                                    ? 0
                                    : event.key === "End"
                                      ? settingsNavItems.length - 1
                                      : undefined;
                            if (next === undefined) return;
                            event.preventDefault();
                            setActiveTab(settingsNavItems[next].key);
                            document.getElementById(`settings-tab-${settingsNavItems[next].key}`)?.focus();
                          }}
                          className={`settings-tab ${activeTab === tab.key ? "active" : ""}`}
                          key={tab.key}
                          onClick={() => setActiveTab(tab.key)}
                          role="tab"
                          type="button"
                        >
                          <Icon size={15} aria-hidden="true" className="settings-tab-icon" />
                          <span>{t(tab.labelKey)}</span>
                          {renderNavBadge(tab.key)}
                        </button>
                      );
                    })}
                  </div>
                );
              })}
            </div>
          </aside>

          <div
            className="settings-content"
            id="settings-panel"
            role="tabpanel"
            aria-labelledby={`settings-tab-${activeTab}`}
            tabIndex={0}
          >
            {activeTab === "general" ? (
              <GeneralSettingsSection
                connection={connection}
                runtimeConfig={runtimeConfig}
                runtimeConfigLoading={runtimeConfigLoading}
                runtimeConfigError={runtimeConfigError}
                onRefreshRuntimeConfig={onRefreshRuntimeConfig}
              />
            ) : null}

            {activeTab === "access" ? (
              <AccessSettingsSection connection={connection} serve={serve} />
            ) : null}

            {activeTab === "providers" ? (
              <ModelProvidersSection
                surface={surface}
                configFilePath={runtimeConfig.configFilePath}
                modelCatalog={modelCatalog}
                modelCatalogLoading={modelCatalogLoading}
                onRefreshModels={onRefreshModels}
                modelDefault={modelDefault}
                sortedProviders={sortedProviders}
                usedProviders={usedProviders}
                providersWithModels={providersWithModels}
                providerSearch={providerSearch}
                setProviderSearch={setProviderSearch}
                providerFilter={providerFilter}
                setProviderFilter={setProviderFilter}
                editingProvider={editingProvider}
                setEditingProvider={setEditingProvider}
                customProviderDraft={customProviderDraft}
                setCustomProviderDraft={setCustomProviderDraft}
                customProviderMessage={customProviderMessage}
                setCustomProviderMessage={setCustomProviderMessage}
                providerDrafts={providerDrafts}
                apiKeyDrafts={apiKeyDrafts}
                setApiKeyDrafts={setApiKeyDrafts}
                credentialMessages={credentialMessages}
                providerSaveMessage={providerSaveMessage}
                runtimeConfigLoading={runtimeConfigLoading}
                runtimeConfigSaving={runtimeConfigSaving}
                credentialStoreLoading={credentialStoreLoading}
                codexDeviceLogin={codexDeviceLogin}
                onSetDeviceLoginProviderId={setDeviceLoginProviderId}
                onStartCodexDeviceLogin={onStartCodexDeviceLogin}
                onClearCodexDeviceLogin={onClearCodexDeviceLogin}
                isCredentialProfileConfigured={isCredentialProfileConfigured}
                onUpdateProviderDraft={updateProviderDraft}
                onSaveProviderConfig={saveProviderConfig}
                onRemoveProviderConfig={removeProviderConfig}
                onRemoveApiKey={removeApiKey}
                onCreateCustomProvider={createCustomProvider}
                onSelectAsDefaultModel={selectProviderModelAsDefault}
              />
            ) : null}

            {activeTab === "models" ? (
              <LanguageModelSettingsSection
                surface={surface}
                modelCatalog={modelCatalog}
                runtimeConfigLoading={runtimeConfigLoading}
                runtimeConfigSaving={runtimeConfigSaving}
                runtimeConfigError={runtimeConfigError}
                modelDefault={modelDefault}
                setModelDefault={setModelDefault}
                modelFallbacks={modelFallbacks}
                setModelFallbacks={setModelFallbacks}
                saveMessage={saveMessage}
                rejectedResults={rejectedResults}
                onRefreshRuntimeConfig={onRefreshRuntimeConfig}
                onSave={saveLanguageModelsConfig}
                onReset={() => {
                  if (!surface) return;
                  setModelDefault(surface.modelDefault);
                  setModelFallbacks(surface.modelFallbacks ?? []);
                  setSaveMessage(undefined);
                }}
                onOpenProviders={() => setActiveTab("providers")}
              />
            ) : null}

            {activeTab === "vision" ? (
              <VisionSettingsSection
                surface={surface}
                visionModels={visionModels}
                imageGenModels={imageGenModels}
                visionDefault={visionDefault}
                setVisionDefault={setVisionDefault}
                imageGenDefault={imageGenDefault}
                setImageGenDefault={setImageGenDefault}
                visionProviderId={visionRouteProvider}
                visionProviderReady={visionProviderReady}
                imageGenProviderId={imageGenRouteProvider}
                imageGenProviderReady={imageGenProviderReady}
                visionSaveMessage={visionSaveMessage}
                imageGenSaveMessage={imageGenSaveMessage}
                runtimeConfigLoading={runtimeConfigLoading}
                runtimeConfigSaving={runtimeConfigSaving}
                onSaveVision={saveVisionConfig}
                onSaveImageGen={saveImageGenConfig}
                onOpenProvider={(providerId) => openProviderEditor(providerId)}
              />
            ) : null}

            {activeTab === "search" ? (
              <SearchSettingsSection
                surface={surface}
                runtimeConfigLoading={runtimeConfigLoading}
                runtimeConfigSaving={runtimeConfigSaving}
                credentialStoreLoading={credentialStoreLoading}
                searchEnabled={searchEnabled}
                setSearchEnabled={setSearchEnabled}
                searchBuiltinProviderEnabled={searchBuiltinProviderEnabled}
                setSearchBuiltinProviderEnabled={setSearchBuiltinProviderEnabled}
                searchProvider={searchProvider}
                setSearchProvider={setSearchProvider}
                searchMode={searchMode}
                setSearchMode={setSearchMode}
                searchProviders={searchProviders}
                setSearchProviders={setSearchProviders}
                searchMaxResults={searchMaxResults}
                setSearchMaxResults={setSearchMaxResults}
                searchMaxProviderAttempts={searchMaxProviderAttempts}
                setSearchMaxProviderAttempts={setSearchMaxProviderAttempts}
                searchSaveMessage={searchSaveMessage}
                searchProviderSaveMessage={searchProviderSaveMessage}
                standardSearchProviders={standardSearchProviders}
                groupedSearchProviders={groupedSearchProviders}
                customSearchProviderIds={customSearchProviderIds}
                webSearchProviderKinds={webSearchProviderKinds}
                searchProviderDrafts={searchProviderDrafts}
                searchApiKeyDrafts={searchApiKeyDrafts}
                setSearchApiKeyDrafts={setSearchApiKeyDrafts}
                searchCredentialMessages={searchCredentialMessages}
                newSearchProviderId={newSearchProviderId}
                setNewSearchProviderId={setNewSearchProviderId}
                newSearchProviderKind={newSearchProviderKind}
                setNewSearchProviderKind={setNewSearchProviderKind}
                isCredentialProfileConfigured={isCredentialProfileConfigured}
                onSaveSearchConfig={saveSearchConfig}
                onUpdateSearchProviderDraft={updateSearchProviderDraft}
                onAddSearchProviderDraft={addSearchProviderDraft}
                onSaveSearchProviderConfig={saveSearchProviderConfig}
                onRemoveSearchProviderConfig={removeSearchProviderConfig}
                onSaveSearchApiKey={saveSearchApiKey}
                onRemoveSearchApiKey={removeSearchApiKey}
              />
            ) : null}

            {activeTab === "decision" ? (
              <DecisionSettingsSection
                surface={surface}
                decisionModels={decisionModels}
                decisionEnabled={decisionEnabled}
                setDecisionEnabled={setDecisionEnabled}
                decisionModel={decisionModel}
                setDecisionModel={setDecisionModel}
                decisionPreset={decisionPreset}
                setDecisionPreset={setDecisionPreset}
                decisionModelDir={decisionModelDir}
                setDecisionModelDir={setDecisionModelDir}
                decisionVariant={decisionVariant}
                setDecisionVariant={setDecisionVariant}
                decisionNumThreads={decisionNumThreads}
                setDecisionNumThreads={setDecisionNumThreads}
                decisionChecksum={decisionChecksum}
                setDecisionChecksum={setDecisionChecksum}
                advisoryToolEnabled={advisoryToolEnabled}
                setAdvisoryToolEnabled={setAdvisoryToolEnabled}
                advisoryToolMaxCalls={advisoryToolMaxCalls}
                setAdvisoryToolMaxCalls={setAdvisoryToolMaxCalls}
                advisoryToolTimeout={advisoryToolTimeout}
                setAdvisoryToolTimeout={setAdvisoryToolTimeout}
                advisoryToolMinConfidence={advisoryToolMinConfidence}
                setAdvisoryToolMinConfidence={setAdvisoryToolMinConfidence}
                decisionSaveMessage={decisionSaveMessage}
                runtimeConfigLoading={runtimeConfigLoading}
                runtimeConfigSaving={runtimeConfigSaving}
                onSaveDecision={saveDecisionConfig}
                onUpdateRuntimeConfig={onUpdateRuntimeConfig}
                onDownloadLocalOnnxPreset={onDownloadLocalOnnxPreset}
                onCancelLocalOnnxPreset={onCancelLocalOnnxPreset}
              />
            ) : null}

            {activeTab === "advanced" ? (
              <AdvancedSettingsSection
                surface={surface}
                runtimeConfig={runtimeConfig}
                runtimeConfigLoading={runtimeConfigLoading}
                runtimeConfigSaving={runtimeConfigSaving}
                modelCatalog={modelCatalog}
                modelCatalogLoading={modelCatalogLoading}
                modelCatalogError={modelCatalogError}
                groupedModels={groupedModels}
                defaultToolOutputTokens={defaultToolOutputTokens}
                setDefaultToolOutputTokens={setDefaultToolOutputTokens}
                maxToolOutputTokens={maxToolOutputTokens}
                setMaxToolOutputTokens={setMaxToolOutputTokens}
                disableProviderFallback={disableProviderFallback}
                setDisableProviderFallback={setDisableProviderFallback}
                limitsSaveMessage={limitsSaveMessage}
                onSaveLimits={saveRuntimeLimitsConfig}
                onRefreshRuntimeConfig={onRefreshRuntimeConfig}
                onRefreshModels={onRefreshModels}
              />
            ) : null}
          </div>
        </div>
      </div>
    </section>
  );
}
