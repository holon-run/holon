import { useState } from "react";
import { ArrowLeft, ArrowRight, CircleCheck } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "../../../components/ui/Button";
import { Card } from "../../../components/ui/Card";
import { StatusChip } from "../../../components/ui/StatusChip";
import type { RuntimeConfigState } from "../../../runtime/types";
import {
  defaultSearchProviderDraft,
  type SearchProviderDraft,
  type StandardSearchProviderDefinition,
} from "../settings-helpers";

type Surface = NonNullable<RuntimeConfigState["surface"]>;

interface SearchSettingsSectionProps {
  surface?: Surface;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  credentialStoreLoading: boolean;
  searchEnabled: boolean;
  setSearchEnabled: (value: boolean) => void;
  searchBuiltinProviderEnabled: boolean;
  setSearchBuiltinProviderEnabled: (value: boolean) => void;
  searchProvider: string;
  setSearchProvider: (value: string) => void;
  searchMode: "single" | "fallback" | "aggregate";
  setSearchMode: (value: "single" | "fallback" | "aggregate") => void;
  searchProviders: string;
  setSearchProviders: (value: string) => void;
  searchMaxResults: string;
  setSearchMaxResults: (value: string) => void;
  searchMaxProviderAttempts: string;
  setSearchMaxProviderAttempts: (value: string) => void;
  searchSaveMessage?: string;
  searchProviderSaveMessage?: string;
  standardSearchProviders: StandardSearchProviderDefinition[];
  groupedSearchProviders: Array<{ category: "api" | "selfHosted"; providers: StandardSearchProviderDefinition[] }>;
  customSearchProviderIds: string[];
  webSearchProviderKinds: string[];
  searchProviderDrafts: Record<string, SearchProviderDraft>;
  searchApiKeyDrafts: Record<string, string>;
  setSearchApiKeyDrafts: React.Dispatch<React.SetStateAction<Record<string, string>>>;
  searchCredentialMessages: Record<string, string>;
  newSearchProviderId: string;
  setNewSearchProviderId: (value: string) => void;
  newSearchProviderKind: string;
  setNewSearchProviderKind: (value: string) => void;
  isCredentialProfileConfigured: (profile: string) => boolean;
  onSaveSearchConfig: () => Promise<void>;
  onUpdateSearchProviderDraft: (providerId: string, patch: Partial<SearchProviderDraft>) => void;
  onAddSearchProviderDraft: () => string | undefined;
  onSaveSearchProviderConfig: (providerId: string) => Promise<void>;
  onRemoveSearchProviderConfig: (providerId: string) => Promise<void>;
  onSaveSearchApiKey: (providerId: string, credentialProfile: string) => Promise<void>;
  onRemoveSearchApiKey: (providerId: string, credentialProfile: string) => Promise<void>;
}

export function SearchSettingsSection({
  surface,
  runtimeConfigLoading,
  runtimeConfigSaving,
  credentialStoreLoading,
  searchEnabled,
  setSearchEnabled,
  searchBuiltinProviderEnabled,
  setSearchBuiltinProviderEnabled,
  searchProvider,
  setSearchProvider,
  searchMode,
  setSearchMode,
  searchProviders,
  setSearchProviders,
  searchMaxResults,
  setSearchMaxResults,
  searchMaxProviderAttempts,
  setSearchMaxProviderAttempts,
  searchSaveMessage,
  searchProviderSaveMessage,
  standardSearchProviders,
  groupedSearchProviders,
  customSearchProviderIds,
  webSearchProviderKinds,
  searchProviderDrafts,
  searchApiKeyDrafts,
  setSearchApiKeyDrafts,
  searchCredentialMessages,
  newSearchProviderId,
  setNewSearchProviderId,
  newSearchProviderKind,
  setNewSearchProviderKind,
  isCredentialProfileConfigured,
  onSaveSearchConfig,
  onUpdateSearchProviderDraft,
  onAddSearchProviderDraft,
  onSaveSearchProviderConfig,
  onRemoveSearchProviderConfig,
  onSaveSearchApiKey,
  onRemoveSearchApiKey,
}: SearchSettingsSectionProps) {
  const { t } = useTranslation();
  const [editingSearchProviderId, setEditingSearchProviderId] = useState<string | null>(null);
  const [searchFilter, setSearchFilter] = useState<"configured" | "all">("all");
  const [showCustomForm, setShowCustomForm] = useState(false);

  const webSearch = surface?.webSearch;
  const routingDirty = Boolean(
    webSearch && (
      searchEnabled !== webSearch.enabled
      || searchBuiltinProviderEnabled !== webSearch.builtinProviderEnabled
      || (searchProvider.trim() || "auto") !== (webSearch.provider || "auto")
      || searchMode !== webSearch.mode
      || searchProviders.trim() !== webSearch.providers.join(", ").trim()
      || searchMaxResults.trim() !== String(webSearch.maxResults)
      || searchMaxProviderAttempts.trim() !== String(webSearch.maxProviderAttempts)
    ),
  );

  const getStandardStatus = (definition: StandardSearchProviderDefinition) => {
    const provider = surface?.webSearchProviders.find((entry) => entry.id === definition.id);
    const draft = searchProviderDrafts[definition.id] ?? defaultSearchProviderDraft(definition.id, definition);
    const credentialProfile = draft.credentialProfile?.trim() ?? definition.defaultCredentialProfile ?? "";
    const credentialReady = credentialProfile ? isCredentialProfileConfigured(credentialProfile) : false;
    const explicitlyConfigured = definition.requiresApiKey
      ? credentialReady
      : credentialReady || Boolean(provider);

    if (definition.requiresApiKey) {
      return {
        isConfigured: credentialReady,
        sortRank: credentialReady ? 2 : 0,
        tone: credentialReady ? "success" : "waiting",
        label: credentialReady ? t("modelUi.credentialsPresent") : t("modelUi.credentialsMissing"),
        subLabel: provider ? t("settings.providerSourceConfig") : "",
      };
    }
    if (definition.supportsApiKey) {
      return {
        isConfigured: explicitlyConfigured,
        sortRank: explicitlyConfigured ? 2 : 1,
        tone: credentialReady ? "success" : "idle",
        label: credentialReady
          ? t("modelUi.credentialsPresent")
          : provider
            ? t("modelUi.connected")
            : t("modelUi.noAuth"),
        subLabel: provider && credentialReady ? t("settings.providerSourceConfig") : !credentialReady && provider ? t("modelUi.noAuth") : "",
      };
    }
    return {
      isConfigured: Boolean(provider),
      sortRank: provider ? 2 : 0,
      tone: provider ? "success" : "idle",
      label: provider ? t("modelUi.connected") : t("settings.notConfigured"),
      subLabel: provider?.baseUrl ?? "",
    };
  };

  const editingDefinition = editingSearchProviderId
    ? standardSearchProviders.find((entry) => entry.id === editingSearchProviderId)
    : undefined;

  return (
    <div className="settings-section-stack">
      <Card className="settings-card settings-primary-card">
        <div className="settings-card-head">
          <div>
            <span className="eyebrow">{t("settings.runtimeDefaults")}</span>
            <h2>{t("settings.webSearch")}</h2>
          </div>
          {routingDirty ? <span className="settings-dirty-pill">{t("settings.unsavedChanges")}</span> : null}
        </div>
        {!webSearch || !surface ? (
          <div className="settings-callout">
            <strong>{t("settings.searchConfigUnavailable")}</strong>
            <span>{t("settings.refreshRuntimeHint")}</span>
          </div>
        ) : (
          <form
            className="settings-form"
            onSubmit={(event) => {
              event.preventDefault();
              void onSaveSearchConfig();
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
                  const status = getStandardStatus(provider);
                  const label = t(`settings.searchProviderKinds.${provider.kind}.label`, { defaultValue: provider.kind });
                  return (
                    <option key={provider.id} value={provider.id}>
                      {label} — {status.label}
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
            <p className="settings-hint">{t("settings.searchCredentialHint")}</p>
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
                  <input
                    value={searchProviders}
                    onChange={(event) => setSearchProviders(event.target.value)}
                    placeholder="duckduckgo, brave"
                  />
                </label>
                <label>
                  <span>{t("settings.maxResults")}</span>
                  <input inputMode="numeric" value={searchMaxResults} onChange={(event) => setSearchMaxResults(event.target.value)} />
                </label>
              </div>
              <div className="settings-form-row">
                <label>
                  <span>{t("settings.maxProviderAttempts")}</span>
                  <input
                    inputMode="numeric"
                    value={searchMaxProviderAttempts}
                    onChange={(event) => setSearchMaxProviderAttempts(event.target.value)}
                  />
                </label>
                <label>
                  <span>{t("settings.configuredProviders")}</span>
                  <input
                    readOnly
                    value={surface.webSearchProviders.map((provider) => provider.id).join(", ") || t("settings.builtinDefaultsValue")}
                  />
                </label>
              </div>
            </details>
            <div className="settings-actions">
              <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                {runtimeConfigSaving ? t("settings.saving") : t("settings.save")}
              </Button>
              {searchSaveMessage ? <span className="settings-save-message">{searchSaveMessage}</span> : null}
            </div>
          </form>
        )}
      </Card>

      <Card className="settings-card model-services-card">
        {editingSearchProviderId === null ? (
          <div className="settings-card-head">
            <div>
              <h2>{t("settings.webSearchProviders")}</h2>
            </div>
            {surface ? (
              <div className="settings-card-actions">
                <div className="provider-directory-filters" role="group" aria-label={t("settings.webSearchProviders")}>
                  <button
                    type="button"
                    aria-pressed={searchFilter === "configured"}
                    onClick={() => setSearchFilter("configured")}
                  >
                    {t("modelUi.connected")}
                  </button>
                  <button
                    type="button"
                    aria-pressed={searchFilter === "all"}
                    onClick={() => setSearchFilter("all")}
                  >
                    {t("modelUi.all")}
                  </button>
                </div>
                <Button
                  type="button"
                  variant="secondary"
                  onClick={() => setShowCustomForm((open) => !open)}
                >
                  {t("settings.addCustomSearchProvider")}
                </Button>
              </div>
            ) : null}
          </div>
        ) : null}
        {!surface ? (
          <div className="settings-callout">
            <strong>{t("settings.searchProviderConfigUnavailable")}</strong>
            <span>{t("settings.connectLiveSearchCreds")}</span>
          </div>
        ) : (
          <div className="settings-provider-list">
            {editingSearchProviderId === null ? (
              <>
                <p className="settings-muted">{t("settings.searchProviderMetadataHint")}</p>
                {showCustomForm ? (
                  <div className="settings-provider-editor">
                    <header className="settings-provider-detail-head">
                      <div className="settings-provider-detail-title">
                        <strong>{t("settings.addCustomSearchProvider")}</strong>
                        <small className="settings-muted">{t("settings.customSearchProviderHint")}</small>
                      </div>
                    </header>
                    <div className="settings-form-row">
                      <label>
                        <span>{t("settings.providerId")}</span>
                        <input
                          value={newSearchProviderId}
                          onChange={(event) => setNewSearchProviderId(event.target.value)}
                          placeholder="custom_search"
                        />
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
                      <Button
                        type="button"
                        disabled={!newSearchProviderId.trim()}
                        onClick={() => {
                          const createdId = onAddSearchProviderDraft();
                          if (createdId) {
                            setShowCustomForm(false);
                            setEditingSearchProviderId(createdId);
                          }
                        }}
                      >
                        {t("settings.addCustomDraft")}
                      </Button>
                      <Button type="button" variant="secondary" onClick={() => setShowCustomForm(false)}>
                        {t("common.cancel")}
                      </Button>
                    </div>
                  </div>
                ) : null}

                <div className="settings-builtins">
                  <div>
                    <strong>{t("settings.nativeSearch")}</strong>
                    <span>{t("settings.nativeSearchDesc")}</span>
                  </div>
                  <StatusChip tone={searchBuiltinProviderEnabled ? "success" : "muted"}>
                    {searchBuiltinProviderEnabled ? t("settings.enabled") : t("settings.disabled")}
                  </StatusChip>
                  <div>
                    <strong>DuckDuckGo</strong>
                    <span>{t("settings.duckDuckGoDesc")}</span>
                  </div>
                  <StatusChip tone="idle">{t("modelUi.noAuth")}</StatusChip>
                </div>

                {groupedSearchProviders.map((group) => {
                  const visibleInGroup = group.providers
                    .filter((def) => searchFilter === "all" || getStandardStatus(def).isConfigured)
                    .sort((left, right) => getStandardStatus(right).sortRank - getStandardStatus(left).sortRank);
                  if (visibleInGroup.length === 0) return null;
                  return (
                    <section className="settings-provider-group" key={group.category}>
                      <div className="settings-provider-group-head">
                        <strong>{t(`settings.searchProviderGroups.${group.category}.label`)}</strong>
                        <span>{t(`settings.searchProviderGroups.${group.category}.description`)}</span>
                      </div>
                      <div className="provider-directory">
                        {visibleInGroup.map((definition) => {
                          const status = getStandardStatus(definition);
                          const label = t(`settings.searchProviderKinds.${definition.kind}.label`, {
                            defaultValue: definition.kind,
                          });
                          const description = t(`settings.searchProviderKinds.${definition.kind}.description`, {
                            defaultValue: t("settings.searchProviderDefaultDescription"),
                          });
                          return (
                            <button
                              key={definition.id}
                              type="button"
                              className={`provider-directory-row${status.isConfigured ? " is-configured" : ""}`}
                              data-search-provider={definition.id}
                              onClick={() => setEditingSearchProviderId(definition.id)}
                            >
                              <span>
                                <strong>{label}</strong>
                                <small>{description}</small>
                              </span>
                              <span className="provider-directory-status">
                                <span className={`provider-directory-status-line${status.isConfigured ? " is-configured" : ""}`}>
                                  {status.isConfigured ? <CircleCheck size={13} aria-hidden="true" /> : null}
                                  {status.label}
                                </span>
                                {status.subLabel ? <small>{status.subLabel}</small> : null}
                              </span>
                              <ArrowRight size={16} />
                            </button>
                          );
                        })}
                      </div>
                    </section>
                  );
                })}

                {searchFilter === "configured"
                  && groupedSearchProviders.every((group) =>
                    group.providers.every((def) => !getStandardStatus(def).isConfigured),
                  )
                  && customSearchProviderIds.length === 0 ? (
                  <div className="settings-callout">
                    <span>{t("modelUi.noProviders")}</span>
                    <button
                      type="button"
                      className="model-browser-more"
                      onClick={() => setSearchFilter("all")}
                    >
                      {t("modelUi.allServices")}
                    </button>
                  </div>
                ) : null}

                {customSearchProviderIds.length > 0 ? (
                  <section className="settings-provider-group">
                    <div className="settings-provider-group-head">
                      <strong>{t("settings.advancedCustomProvider")}</strong>
                    </div>
                    <div className="provider-directory">
                      {customSearchProviderIds.map((providerId) => {
                        const persisted = surface.webSearchProviders.find((entry) => entry.id === providerId);
                        const draft = searchProviderDrafts[providerId];
                        if (!draft) return null;
                        return (
                          <button
                            key={providerId}
                            type="button"
                            className="provider-directory-row"
                            data-search-provider={providerId}
                            onClick={() => setEditingSearchProviderId(providerId)}
                          >
                            <span>
                              <strong>{providerId}</strong>
                              <small>
                                {[draft.kind, persisted ? t("settings.providerSourceCustom") : ""].filter(Boolean).join(" · ")}
                              </small>
                            </span>
                            <span className="provider-directory-status">
                              <span className={`provider-directory-status-line${persisted ? " is-configured" : ""}`}>
                                {persisted ? <CircleCheck size={13} aria-hidden="true" /> : null}
                                {persisted ? t("modelUi.connected") : t("settings.unsavedSearchProvider")}
                              </span>
                            </span>
                            <ArrowRight size={16} />
                          </button>
                        );
                      })}
                    </div>
                  </section>
                ) : null}
              </>
            ) : (
              <>
                <div className="settings-provider-back-bar">
                  <button
                    type="button"
                    className="model-browser-more provider-back"
                    onClick={() => setEditingSearchProviderId(null)}
                  >
                    <ArrowLeft size={15} /> {t("settings.backSearchProviders")}
                  </button>
                </div>
                {editingDefinition ? (() => {
                  const definition = editingDefinition;
                  const provider = surface.webSearchProviders.find((entry) => entry.id === definition.id);
                  const draft = searchProviderDrafts[definition.id] ?? defaultSearchProviderDraft(definition.id, definition);
                  const credentialProfile = draft.credentialProfile?.trim() ?? definition.defaultCredentialProfile ?? "";
                  const credentialReady = credentialProfile ? isCredentialProfileConfigured(credentialProfile) : false;
                  const status = getStandardStatus(definition);
                  const label = t(`settings.searchProviderKinds.${definition.kind}.label`, { defaultValue: definition.kind });
                  const description = t(`settings.searchProviderKinds.${definition.kind}.description`, {
                    defaultValue: t("settings.searchProviderDefaultDescription"),
                  });
                  return (
                    <form
                      className="settings-provider-editor"
                      key={definition.id}
                      onSubmit={(event) => {
                        event.preventDefault();
                        void onSaveSearchProviderConfig(definition.id);
                      }}
                    >
                      <header className="settings-provider-detail-head">
                        <div className="settings-provider-detail-title">
                          <h2>{label}</h2>
                          <p className="settings-hint" style={{ margin: 0 }}>{description}</p>
                          <div className="settings-provider-meta-pills">
                            <code>{definition.id}</code>
                            <span>{definition.kind}</span>
                            <span>
                              {provider ? t("settings.providerSourceConfig") : t("settings.builtinDefaults")}
                            </span>
                          </div>
                        </div>
                        <StatusChip tone={status.tone}>{status.label}</StatusChip>
                      </header>
                      {definition.requiresBaseUrl ? (
                        <div className="settings-form-row">
                          <label>
                            <span>{t("settings.baseUrl")} *</span>
                            <input
                              value={draft.baseUrl ?? ""}
                              onChange={(event) => onUpdateSearchProviderDraft(definition.id, { baseUrl: event.target.value })}
                              placeholder={t("settings.selfHostedBaseUrlPlaceholder")}
                            />
                          </label>
                        </div>
                      ) : null}
                      {draft.kind === "google" ? (
                        <label>
                          <span>{t("settings.engineId")} *</span>
                          <input
                            value={draft.engineId ?? ""}
                            onChange={(event) => onUpdateSearchProviderDraft(definition.id, { engineId: event.target.value })}
                            placeholder="017576662512468239146:omuauf_lfve"
                          />
                        </label>
                      ) : null}
                      {definition.supportsApiKey && credentialProfile ? (
                        <div className="settings-api-key-section">
                          <div className="settings-credential-source-bar">
                            <span>
                              {credentialReady
                                ? t("settings.credentialStoredInProfile", { profile: credentialProfile })
                                : t("settings.credentialWillSaveToProfile", { profile: credentialProfile })}
                            </span>
                          </div>
                          <label>
                            <span>{t("settings.apiKeyLabel")}</span>
                            <input
                              type="password"
                              placeholder={
                                credentialReady
                                  ? t("settings.replaceApiKeyPlaceholder")
                                  : t("settings.pasteApiKeySearch")
                              }
                              value={searchApiKeyDrafts[definition.id] ?? ""}
                              onChange={(event) => setSearchApiKeyDrafts((prev) => ({ ...prev, [definition.id]: event.target.value }))}
                            />
                          </label>
                          <div className="settings-actions">
                            <Button
                              type="button"
                              variant="secondary"
                              disabled={!searchApiKeyDrafts[definition.id]?.trim()}
                              onClick={() => void onSaveSearchApiKey(definition.id, credentialProfile)}
                            >
                              {t("settings.saveApiKey")}
                            </Button>
                            {credentialReady ? (
                              <Button type="button" variant="outline" onClick={() => void onRemoveSearchApiKey(definition.id, credentialProfile)}>
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
                        <div className="settings-advanced-body">
                          <div className="settings-form-row">
                            <label>
                              <span>{t("settings.providerId")}</span>
                              <input value={definition.id} readOnly disabled />
                            </label>
                            <label>
                              <span>{t("settings.kindLabel")}</span>
                              <input value={definition.kind} readOnly disabled />
                            </label>
                          </div>
                          <div className="settings-form-row">
                            {definition.supportsApiKey ? (
                              <label>
                                <span>{t("settings.credentialProfile")}</span>
                                <input
                                  value={credentialProfile}
                                  onChange={(event) => onUpdateSearchProviderDraft(definition.id, { credentialProfile: event.target.value })}
                                  placeholder={definition.defaultCredentialProfile}
                                />
                              </label>
                            ) : null}
                            {definition.supportsApiKey ? (
                              <label>
                                <span>{t("settings.baseUrl")}</span>
                                <input
                                  value={draft.baseUrl ?? ""}
                                  onChange={(event) => onUpdateSearchProviderDraft(definition.id, { baseUrl: event.target.value })}
                                  placeholder={t("settings.optionalProviderDefault")}
                                />
                              </label>
                            ) : null}
                          </div>
                        </div>
                      </details>
                      <div className="settings-provider-actions-bar">
                        <div className="settings-actions">
                          <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                            {runtimeConfigSaving
                              ? t("settings.saving")
                              : provider
                                ? t("settings.saveProvider", { name: label })
                                : t("settings.enableProvider", { name: label })}
                          </Button>
                          {searchProviderSaveMessage ? (
                            <span className="settings-save-message">{searchProviderSaveMessage}</span>
                          ) : null}
                        </div>
                        {provider ? (
                          <Button
                            type="button"
                            variant="outline"
                            disabled={runtimeConfigSaving || runtimeConfigLoading}
                            onClick={() => void onRemoveSearchProviderConfig(definition.id)}
                          >
                            {t("settings.removeConfig")}
                          </Button>
                        ) : null}
                      </div>
                    </form>
                  );
                })() : (() => {
                  const providerId = editingSearchProviderId;
                  const draft = searchProviderDrafts[providerId];
                  const persisted = surface.webSearchProviders.find((entry) => entry.id === providerId);
                  if (!draft) return null;
                  return (
                    <form
                      className="settings-provider-editor"
                      key={providerId}
                      onSubmit={(event) => {
                        event.preventDefault();
                        void onSaveSearchProviderConfig(providerId);
                      }}
                    >
                      <header className="settings-provider-detail-head">
                        <div className="settings-provider-detail-title">
                          <h2>{providerId}</h2>
                          <div className="settings-provider-meta-pills">
                            <code>{providerId}</code>
                            <span>{draft.kind}</span>
                            <span>
                              {persisted ? t("settings.providerSourceCustom") : t("settings.unsavedSearchProvider")}
                            </span>
                          </div>
                        </div>
                        <StatusChip tone={persisted ? "success" : "waiting"}>
                          {persisted ? t("modelUi.connected") : t("settings.notSaved")}
                        </StatusChip>
                      </header>
                      <div className="settings-form-row">
                        <label>
                          <span>{t("settings.kindLabel")}</span>
                          <select value={draft.kind} onChange={(event) => onUpdateSearchProviderDraft(providerId, { kind: event.target.value })}>
                            {webSearchProviderKinds.map((kind) => (
                              <option key={kind} value={kind}>
                                {kind}
                              </option>
                            ))}
                          </select>
                        </label>
                        <label>
                          <span>{t("settings.baseUrl")}</span>
                          <input value={draft.baseUrl ?? ""} onChange={(event) => onUpdateSearchProviderDraft(providerId, { baseUrl: event.target.value })} />
                        </label>
                        <label>
                          <span>{t("settings.credentialProfile")}</span>
                          <input value={draft.credentialProfile ?? ""} onChange={(event) => onUpdateSearchProviderDraft(providerId, { credentialProfile: event.target.value })} />
                        </label>
                        {draft.kind === "google" ? (
                          <label>
                            <span>{t("settings.engineId")} *</span>
                            <input value={draft.engineId ?? ""} onChange={(event) => onUpdateSearchProviderDraft(providerId, { engineId: event.target.value })} />
                          </label>
                        ) : null}
                      </div>
                      <div className="settings-provider-actions-bar">
                        <div className="settings-actions">
                          <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                            {runtimeConfigSaving ? t("settings.saving") : t("settings.saveProvider", { name: providerId })}
                          </Button>
                          {searchProviderSaveMessage ? (
                            <span className="settings-save-message">{searchProviderSaveMessage}</span>
                          ) : null}
                        </div>
                        {persisted ? (
                          <Button
                            type="button"
                            variant="outline"
                            disabled={runtimeConfigSaving || runtimeConfigLoading}
                            onClick={async () => {
                              await onRemoveSearchProviderConfig(providerId);
                              setEditingSearchProviderId(null);
                            }}
                          >
                            {t("settings.removeConfig")}
                          </Button>
                        ) : null}
                      </div>
                    </form>
                  );
                })()}
              </>
            )}
            {editingSearchProviderId === null && searchProviderSaveMessage ? (
              <span className="settings-save-message">{searchProviderSaveMessage}</span>
            ) : null}
          </div>
        )}
      </Card>
    </div>
  );
}
