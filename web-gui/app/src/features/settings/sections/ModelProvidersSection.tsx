import { useEffect, useRef } from "react";
import { ArrowLeft, ArrowRight, CircleCheck, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";

import { ModelList } from "../../../components/models/ModelList";
import { Button } from "../../../components/ui/Button";
import { Card } from "../../../components/ui/Card";
import { StatusChip } from "../../../components/ui/StatusChip";
import {
  isBuiltinProviderId,
  providerHasConfiguredCredential,
  providerIsConnected,
  providerPresentation,
} from "../../../lib/model-presentation";
import type {
  CodexDeviceLoginState,
  RuntimeConfigState,
  RuntimeModelCatalog,
  RuntimeProviderSummary,
} from "../../../runtime/types";
import {
  CUSTOM_PROVIDER_DISCOVERY_KINDS,
  CUSTOM_PROVIDER_TRANSPORTS,
  EMPTY_CUSTOM_PROVIDER_DRAFT,
  isProviderKeyCredential,
  providerCredentialReady,
  type CustomProviderDraft,
  type ProviderDraft,
} from "../settings-helpers";

const TRANSPORT_LABELS: Record<string, string> = {
  anthropic_messages: "Anthropic Messages",
  openai_responses: "OpenAI Responses",
  openai_chat_completions: "OpenAI Chat Completions",
  openai_codex_responses: "OpenAI Codex Responses",
  gemini_generate_content: "Gemini GenerateContent",
  ai_evaluation_model: "AI Evaluation (Decision)",
};

function formatTransportLabel(transport: string): string {
  return TRANSPORT_LABELS[transport] ?? transport;
}

type Surface = NonNullable<RuntimeConfigState["surface"]>;

interface ModelProvidersSectionProps {
  surface?: Surface;
  configFilePath?: string;
  modelCatalog: RuntimeModelCatalog;
  modelCatalogLoading: boolean;
  onRefreshModels: () => Promise<void>;
  modelDefault: string;
  sortedProviders: RuntimeProviderSummary[];
  usedProviders: Set<string>;
  providersWithModels: Set<string>;
  providerSearch: string;
  setProviderSearch: (value: string) => void;
  providerFilter: "connected" | "all";
  setProviderFilter: (value: "connected" | "all") => void;
  editingProvider: string | null;
  setEditingProvider: (value: string | null) => void;
  customProviderDraft: CustomProviderDraft | null;
  setCustomProviderDraft: React.Dispatch<React.SetStateAction<CustomProviderDraft | null>>;
  customProviderMessage?: string;
  setCustomProviderMessage: (value: string | undefined) => void;
  providerDrafts: Record<string, ProviderDraft>;
  apiKeyDrafts: Record<string, string>;
  setApiKeyDrafts: React.Dispatch<React.SetStateAction<Record<string, string>>>;
  credentialMessages: Record<string, string>;
  providerSaveMessage?: string;
  runtimeConfigLoading: boolean;
  runtimeConfigSaving: boolean;
  credentialStoreLoading: boolean;
  codexDeviceLogin: CodexDeviceLoginState;
  onSetDeviceLoginProviderId: (providerId: string) => void;
  onStartCodexDeviceLogin: (providerId?: string) => Promise<void>;
  onClearCodexDeviceLogin: () => void;
  isCredentialProfileConfigured: (profile: string) => boolean;
  onUpdateProviderDraft: (providerId: string, patch: Partial<ProviderDraft>) => void;
  onSaveProviderConfig: (providerId: string) => Promise<void>;
  onRemoveProviderConfig: (providerId: string) => Promise<void>;
  onRemoveApiKey: (providerId: string, credentialProfile: string) => Promise<void>;
  onCreateCustomProvider: () => Promise<void>;
  onSelectAsDefaultModel: (routeRef: string) => Promise<void>;
}

export function ModelProvidersSection({
  surface,
  configFilePath,
  modelCatalog,
  modelCatalogLoading,
  onRefreshModels,
  modelDefault,
  sortedProviders,
  usedProviders,
  providersWithModels,
  providerSearch,
  setProviderSearch,
  providerFilter,
  setProviderFilter,
  editingProvider,
  setEditingProvider,
  customProviderDraft,
  setCustomProviderDraft,
  customProviderMessage,
  setCustomProviderMessage,
  providerDrafts,
  apiKeyDrafts,
  setApiKeyDrafts,
  credentialMessages,
  providerSaveMessage,
  runtimeConfigLoading,
  runtimeConfigSaving,
  credentialStoreLoading,
  codexDeviceLogin,
  onSetDeviceLoginProviderId,
  onStartCodexDeviceLogin,
  onClearCodexDeviceLogin,
  isCredentialProfileConfigured,
  onUpdateProviderDraft,
  onSaveProviderConfig,
  onRemoveProviderConfig,
  onRemoveApiKey,
  onCreateCustomProvider,
  onSelectAsDefaultModel,
}: ModelProvidersSectionProps) {
  const { t } = useTranslation();
  const providerDirectoryRef = useRef<HTMLDivElement>(null);
  const providerBackRef = useRef<HTMLButtonElement>(null);
  const lastProviderRef = useRef<string | null>(null);

  const visibleProviders = sortedProviders
    .filter((provider) => {
      const searchTarget = `${providerPresentation(provider.id).search} ${provider.baseUrl ?? ""}`.toLowerCase();
      return (
        (providerFilter === "all" || providerIsConnected(provider, usedProviders))
        && providerSearch
          .toLowerCase()
          .trim()
          .split(/\s+/)
          .every((word) => searchTarget.includes(word))
      );
    })
    .sort((a, b) => {
      const connectedDiff =
        Number(providerIsConnected(b, usedProviders)) - Number(providerIsConnected(a, usedProviders));
      return connectedDiff || providerPresentation(a.id).label.localeCompare(providerPresentation(b.id).label);
    });

  useEffect(() => {
    if (editingProvider) providerBackRef.current?.focus();
    else if (lastProviderRef.current) {
      const row = Array.from(providerDirectoryRef.current?.querySelectorAll<HTMLButtonElement>("button[data-provider]") ?? [])
        .find((button) => button.dataset.provider === lastProviderRef.current);
      row?.focus();
    }
  }, [editingProvider]);

  return (
    <div className="settings-section-stack">
      <Card className="settings-card model-services-card">
        {editingProvider === null ? (
          <div className="settings-card-head">
            <div>
              <span className="eyebrow">{t("settings.modelsProvidersLabel")}</span>
              <h2>{t("settings.modelProviders")}</h2>
            </div>
            <Button
              type="button"
              variant="secondary"
              onClick={() => {
                setEditingProvider(null);
                setProviderFilter("all");
                setProviderSearch("");
              }}
            >
              {t("modelUi.connectService")}
            </Button>
          </div>
        ) : null}
        {!surface ? (
          <div className="settings-callout">
            <strong>{t("settings.providerConfigUnavailable")}</strong>
            <span>{t("settings.connectLiveModelCreds")}</span>
          </div>
        ) : (
          <div className="settings-provider-list">
            {editingProvider === null ? (
              <div ref={providerDirectoryRef} className="provider-directory">
                <div className="provider-directory-toolbar">
                  <input
                    aria-label={t("modelUi.searchProviders")}
                    placeholder={t("modelUi.searchProviders")}
                    value={providerSearch}
                    onChange={(event) => setProviderSearch(event.target.value)}
                  />
                  <div className="provider-directory-filters" role="group" aria-label={t("modelUi.services")}>
                    {(["connected", "all"] as const).map((filter) => (
                      <button
                        key={filter}
                        type="button"
                        aria-pressed={providerFilter === filter}
                        onClick={() => setProviderFilter(filter)}
                      >
                        {t(`modelUi.${filter}`)}
                      </button>
                    ))}
                  </div>
                  <Button
                    type="button"
                    variant="secondary"
                    onClick={() => {
                      setCustomProviderMessage(undefined);
                      setCustomProviderDraft((prev) => (prev ? null : { ...EMPTY_CUSTOM_PROVIDER_DRAFT }));
                    }}
                  >
                    {t("settings.addProvider")}
                  </Button>
                </div>
                {customProviderDraft ? (
                  <form
                    className="settings-provider-editor"
                    onSubmit={(event) => {
                      event.preventDefault();
                      void onCreateCustomProvider();
                    }}
                  >
                    <header>
                      <div>
                        <strong>{t("settings.addProvider")}</strong>
                        <small>{t("settings.addProviderHint")}</small>
                      </div>
                    </header>
                    <div className="settings-form-row">
                      <label>
                        <span>{t("settings.providerId")}</span>
                        <input
                          value={customProviderDraft.id}
                          placeholder="my-gateway"
                          onChange={(event) => setCustomProviderDraft({ ...customProviderDraft, id: event.target.value })}
                        />
                      </label>
                      <label>
                        <span>{t("settings.transportLabel")}</span>
                        <select
                          value={customProviderDraft.transport}
                          onChange={(event) =>
                            setCustomProviderDraft({
                              ...customProviderDraft,
                              transport: event.target.value as CustomProviderDraft["transport"],
                            })
                          }
                        >
                          {CUSTOM_PROVIDER_TRANSPORTS.map((transport) => (
                            <option key={transport} value={transport}>
                              {transport}
                            </option>
                          ))}
                        </select>
                      </label>
                    </div>
                    <div className="settings-form-row">
                      <label>
                        <span>{t("settings.baseUrl")}</span>
                        <input
                          value={customProviderDraft.baseUrl}
                          placeholder="https://gateway.example/v1"
                          onChange={(event) => setCustomProviderDraft({ ...customProviderDraft, baseUrl: event.target.value })}
                        />
                      </label>
                      <label>
                        <span>{t("settings.apiKeyLabel")}</span>
                        <input
                          type="password"
                          value={customProviderDraft.apiKey}
                          onChange={(event) => setCustomProviderDraft({ ...customProviderDraft, apiKey: event.target.value })}
                        />
                      </label>
                    </div>
                    <div className="settings-form-row">
                      <label>
                        <span>{t("settings.discoveryKind")}</span>
                        <select
                          value={customProviderDraft.discoveryKind}
                          onChange={(event) =>
                            setCustomProviderDraft({
                              ...customProviderDraft,
                              discoveryKind: event.target.value as (typeof CUSTOM_PROVIDER_DISCOVERY_KINDS)[number],
                            })
                          }
                        >
                          <option value="openai_models">{t("settings.discoveryKindOpenAiModels")}</option>
                          <option value="disabled">{t("settings.discoveryKindDisabled")}</option>
                        </select>
                      </label>
                      <label>
                        <span>{t("settings.discoveryPath")}</span>
                        <input
                          value={customProviderDraft.discoveryPath}
                          placeholder="/v1/models"
                          disabled={customProviderDraft.discoveryKind === "disabled"}
                          onChange={(event) => setCustomProviderDraft({ ...customProviderDraft, discoveryPath: event.target.value })}
                        />
                      </label>
                    </div>
                    <div className="settings-actions">
                      <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                        {t("settings.createProvider")}
                      </Button>
                      <Button
                        type="button"
                        variant="secondary"
                        onClick={() => {
                          setCustomProviderDraft(null);
                          setCustomProviderMessage(undefined);
                        }}
                      >
                        {t("common.cancel")}
                      </Button>
                    </div>
                  </form>
                ) : null}
                {visibleProviders.map((provider) => {
                  const name = providerPresentation(provider.id);
                  const count = modelCatalog.options.filter((model) => model.routeProvider === provider.id && model.available).length;
                  const isCustom = !isBuiltinProviderId(provider.id);
                  const showBaseUrl = isCustom || provider.credentialKind === "none";
                  const sourceNote = isCustom
                    ? t("settings.providerSourceCustom")
                    : provider.configuredInConfig
                      ? t("settings.providerSourceConfig")
                      : provider.credentialSource === "env" && provider.credentialConfigured
                        ? t("settings.providerSourceEnv")
                        : "";
                  const metaParts = [
                    name.plan || (name.name !== provider.id ? provider.id : ""),
                    showBaseUrl ? provider.baseUrl : "",
                    sourceNote,
                  ].filter(Boolean);
                  const isHighlighted =
                    providerHasConfiguredCredential(provider)
                    || (provider.credentialKind === "none" && provider.configuredInConfig);
                  return (
                    <button
                      key={provider.id}
                      type="button"
                      className={`provider-directory-row${isHighlighted ? " is-configured" : ""}`}
                      data-provider={provider.id}
                      onClick={() => {
                        lastProviderRef.current = provider.id;
                        setEditingProvider(provider.id);
                      }}
                    >
                      <span>
                        <strong>{name.name}</strong>
                        {metaParts.length > 0 ? <small>{metaParts.join(" · ")}</small> : null}
                      </span>
                      <span className="provider-directory-status">
                        <span className={`provider-directory-status-line${isHighlighted ? " is-configured" : ""}`}>
                          {isHighlighted ? <CircleCheck size={13} aria-hidden="true" /> : null}
                          {t(
                            provider.credentialKind === "none"
                              ? "modelUi.noAuth"
                              : provider.credentialConfigured
                                ? "modelUi.credentialsPresent"
                                : "modelUi.credentialsMissing",
                          )}
                        </span>
                        <small>{t("modelUi.modelCount", { count })}</small>
                      </span>
                      <ArrowRight size={16} />
                    </button>
                  );
                })}
                {!visibleProviders.length ? (
                  <div className="settings-callout">
                    <span>{t("modelUi.noProviders")}</span>
                    <button
                      type="button"
                      className="model-browser-more"
                      onClick={() => {
                        setProviderFilter("all");
                        setProviderSearch("");
                      }}
                    >
                      {t("modelUi.allServices")}
                    </button>
                  </div>
                ) : null}
                <p className="settings-hint">{t("modelUi.credentialHint")}</p>
              </div>
            ) : (
              <div className="settings-provider-back-bar">
                <button
                  ref={providerBackRef}
                  type="button"
                  className="model-browser-more provider-back"
                  onClick={() => setEditingProvider(null)}
                >
                  <ArrowLeft size={15} />
                  {t("modelUi.backServices")}
                </button>
              </div>
            )}
            {customProviderMessage ? <span className="settings-save-message">{customProviderMessage}</span> : null}
            {sortedProviders
              .filter((provider) => provider.id === editingProvider)
              .map((provider) => {
                const draft = providerDrafts[provider.id];
                if (!draft) return null;
                const effectiveProfile = draft.credentialProfile?.trim() || `${provider.id}:default`;
                const authMode = draft.credentialKind === "oauth" ? "oauth" : "api_key";
                const credentialReady = providerCredentialReady(provider);
                const isCustom = !isBuiltinProviderId(provider.id);
                const profileKeyStored = isCredentialProfileConfigured(effectiveProfile);
                const usingEnvAuth =
                  draft.credentialSource === "env" && Boolean(draft.credentialEnv?.trim());
                const providerModels = modelCatalog.options.filter(
                  (model) => model.routeProvider === provider.id,
                );
                const selectableModelCount = providerModels.filter((model) => model.available).length;
                const statusLabel =
                  draft.credentialKind === "none"
                    ? t("modelUi.noAuth")
                    : credentialReady
                      ? draft.credentialKind === "oauth"
                        ? t("settings.connectedViaOAuth")
                        : usingEnvAuth
                          ? t("settings.providerEnvReady")
                          : t("modelUi.credentialsPresent")
                      : t("modelUi.credentialsMissing");
                const statusTone =
                  draft.credentialKind === "none"
                    ? "idle"
                    : credentialReady
                      ? "success"
                      : "waiting";
                const credentialFeedback = credentialMessages[provider.id];
                const isFeedbackError =
                  credentialFeedback === t("settings.failedSaveKey")
                  || credentialFeedback === t("settings.failedRemoveKey");

                return (
                  <div className="settings-provider-detail" key={provider.id}>
                    <form
                      id="setup-provider-editor"
                      className="settings-provider-editor"
                      onSubmit={(event) => {
                        event.preventDefault();
                        void onSaveProviderConfig(provider.id);
                      }}
                    >
                      <header className="settings-provider-detail-head">
                        <div className="settings-provider-detail-title">
                          <h2>{providerPresentation(provider.id).label}</h2>
                          <div className="settings-provider-meta-pills">
                            <code>{provider.id}</code>
                            <span>{formatTransportLabel(provider.transport)}</span>
                            <span>
                              {isCustom
                                ? t("settings.providerSourceCustom")
                                : provider.configuredInConfig
                                  ? t("settings.providerSourceConfig")
                                  : t("settings.builtinDefaults")}
                            </span>
                          </div>
                        </div>
                        <StatusChip tone={statusTone}>{statusLabel}</StatusChip>
                      </header>

                      {credentialReady && !providersWithModels.has(provider.id) ? (
                        <p className="settings-provider-hint">{t("settings.noModelsForProvider")}</p>
                      ) : null}

                      {draft.credentialKind === "none" ? (
                        <p className="settings-hint">{t("settings.noCredentialsRequired")}</p>
                      ) : null}

                      {draft.apiKeySupported && draft.oauthSupported ? (
                        <div className="settings-form-row">
                          <label>
                            <span>{t("settings.authMode")}</span>
                            <select
                              value={authMode}
                              onChange={(event) => {
                                const nextMode = event.target.value;
                                if (nextMode === "oauth") {
                                  onUpdateProviderDraft(provider.id, {
                                    credentialKind: "oauth",
                                    credentialSource: "credential_profile",
                                    credentialProfile: draft.credentialProfile?.trim() || provider.id,
                                    credentialEnv: "",
                                    credentialExternal: "",
                                  });
                                  return;
                                }
                                onUpdateProviderDraft(provider.id, {
                                  credentialKind: "api_key",
                                  credentialSource: "credential_profile",
                                  credentialProfile: draft.credentialProfile?.trim() || `${provider.id}:default`,
                                  credentialEnv: "",
                                  credentialExternal: "",
                                });
                              }}
                            >
                              <option value="api_key">{t("settings.authModeApiKey")}</option>
                              <option value="oauth">{t("settings.authModeOAuth")}</option>
                            </select>
                          </label>
                        </div>
                      ) : null}

                      {isProviderKeyCredential(draft.credentialKind) ? (
                        <div className="settings-api-key-section">
                          <div className="settings-credential-source-bar">
                            <span>
                              {profileKeyStored
                                ? t("settings.credentialStoredInProfile", { profile: effectiveProfile })
                                : usingEnvAuth
                                  ? provider.credentialConfigured
                                    ? t("settings.credentialUsingEnvReady", { env: draft.credentialEnv })
                                    : t("settings.credentialUsingEnvMissing", { env: draft.credentialEnv })
                                  : t("settings.credentialWillSaveToProfile", { profile: effectiveProfile })}
                            </span>
                          </div>
                          <label>
                            <span>{t("settings.apiKeyLabel")}</span>
                            <input
                              type="password"
                              placeholder={
                                profileKeyStored
                                  ? t("settings.replaceApiKeyPlaceholder")
                                  : t("settings.pasteApiKeyProfile")
                              }
                              value={apiKeyDrafts[provider.id] ?? ""}
                              onChange={(event) =>
                                setApiKeyDrafts((prev) => ({ ...prev, [provider.id]: event.target.value }))
                              }
                            />
                          </label>
                        </div>
                      ) : null}

                      {draft.credentialKind === "oauth" ? (
                        <div className="settings-device-login-section">
                          <div className="settings-credential-source-bar">
                            <span>
                              {provider.credentialConfigured
                                ? t("settings.oauthConnectedHint", {
                                    profile: draft.credentialProfile || provider.id,
                                  })
                                : t("settings.oauthLoginHint")}
                            </span>
                            {(codexDeviceLogin.status === "idle" || codexDeviceLogin.status === "failed") ? (
                              <Button
                                type="button"
                                variant={provider.credentialConfigured ? "outline" : "secondary"}
                                disabled={credentialStoreLoading}
                                onClick={() => {
                                  onSetDeviceLoginProviderId(provider.id);
                                  void onStartCodexDeviceLogin(provider.id);
                                }}
                              >
                                {provider.credentialConfigured ? t("settings.reLogin") : t("settings.loginDeviceFlow")}
                              </Button>
                            ) : null}
                          </div>
                          {codexDeviceLogin.status === "failed" ? (
                            <div className="settings-device-login-error">{codexDeviceLogin.error}</div>
                          ) : null}
                          {codexDeviceLogin.status === "starting" ? (
                            <p className="settings-hint">{t("settings.startingDeviceLogin")}</p>
                          ) : null}
                          {codexDeviceLogin.status === "waiting" ? (
                            <div className="settings-device-login-panel">
                              <div className="settings-actions">
                                <Button
                                  type="button"
                                  variant="secondary"
                                  onClick={() =>
                                    window.open(codexDeviceLogin.verificationUrl, "_blank", "noopener,noreferrer")
                                  }
                                >
                                  {t("settings.openDeviceLoginPage")} <ArrowRight size={14} />
                                </Button>
                                <Button type="button" variant="outline" onClick={onClearCodexDeviceLogin}>
                                  {t("common.cancel")}
                                </Button>
                              </div>
                              <p className="settings-hint">{t("settings.enterDeviceCode")}</p>
                              <div className="settings-device-login-code">{codexDeviceLogin.userCode}</div>
                              <p className="settings-muted">{t("settings.waitingForAuthorization")}</p>
                            </div>
                          ) : null}
                          {codexDeviceLogin.status === "completed" ? (
                            <div className="settings-device-login-panel">
                              <div className="settings-device-login-header" role="status">
                                <StatusChip tone="success">{t("settings.loginSuccessful")}</StatusChip>
                                <Button type="button" variant="outline" onClick={onClearCodexDeviceLogin}>
                                  {t("common.dismiss")}
                                </Button>
                              </div>
                            </div>
                          ) : null}
                        </div>
                      ) : null}

                      {draft.credentialKind !== "none"
                        && !isProviderKeyCredential(draft.credentialKind)
                        && draft.credentialKind !== "oauth" ? (
                        <p className="settings-hint">
                          {t("settings.thisProviderUses")}
                          <code>{draft.credentialKind}</code>
                          {t("settings.authVia")}
                          <code>{draft.credentialSource}</code>
                          {t("settings.configureIn")}
                          <code>{configFilePath ?? "config.json"}</code>.
                        </p>
                      ) : null}

                      <details
                        className="settings-advanced"
                        open={draft.credentialKind === "none" || isCustom || undefined}
                      >
                        <summary>{t("settings.providerAdvancedSummary")}</summary>
                        <div className="settings-advanced-body">
                          <div className="settings-form-row">
                            <label>
                              <span>{t("settings.baseUrl")}</span>
                              <input
                                value={draft.baseUrl}
                                onChange={(event) =>
                                  onUpdateProviderDraft(provider.id, { baseUrl: event.target.value })
                                }
                              />
                            </label>
                            <label>
                              <span>
                                {t("settings.transportLabel")}{" "}
                                <small className="settings-muted">{t("settings.readOnlySuffix")}</small>
                              </span>
                              <input value={provider.transport} readOnly disabled />
                            </label>
                          </div>
                          <div className="settings-form-row">
                            <label>
                              <span>{t("settings.discoveryKind")}</span>
                              <select
                                value={draft.discoveryKind}
                                onChange={(event) => {
                                  const nextKind = event.target.value;
                                  onUpdateProviderDraft(
                                    provider.id,
                                    nextKind === "openai_models"
                                      ? { discoveryKind: nextKind }
                                      : { discoveryKind: nextKind, discoveryPath: "", discoveryDecoder: "" },
                                  );
                                }}
                              >
                                <option value="">{t("settings.discoveryKindDefault")}</option>
                                <option value="openai_models">{t("settings.discoveryKindOpenAiModels")}</option>
                                <option value="disabled">{t("settings.discoveryKindDisabled")}</option>
                              </select>
                            </label>
                            {draft.discoveryKind === "openai_models" ? (
                              <>
                                <label>
                                  <span>{t("settings.discoveryPath")}</span>
                                  <input
                                    value={draft.discoveryPath}
                                    placeholder="/v1/models"
                                    onChange={(event) =>
                                      onUpdateProviderDraft(provider.id, { discoveryPath: event.target.value })
                                    }
                                  />
                                </label>
                                <label>
                                  <span>{t("settings.discoveryDecoder")}</span>
                                  <select
                                    value={draft.discoveryDecoder}
                                    onChange={(event) =>
                                      onUpdateProviderDraft(provider.id, { discoveryDecoder: event.target.value })
                                    }
                                  >
                                    <option value="">{t("settings.discoveryDecoderDefault")}</option>
                                    <option value="openai_compatible">openai_compatible</option>
                                    <option value="magpie">magpie</option>
                                    <option value="hugging_face">hugging_face</option>
                                    <option value="kilo">kilo</option>
                                    <option value="arcee">arcee</option>
                                    <option value="near_ai">near_ai</option>
                                    <option value="open_code_go">open_code_go</option>
                                    <option value="open_router">open_router</option>
                                    <option value="synthetic">synthetic</option>
                                    <option value="tencent_token_hub">tencent_token_hub</option>
                                    <option value="venice">venice</option>
                                    <option value="vercel_ai_gateway">vercel_ai_gateway</option>
                                  </select>
                                </label>
                              </>
                            ) : null}
                          </div>
                        </div>
                      </details>

                      <div className="settings-provider-actions-bar">
                        <div className="settings-actions">
                          <Button type="submit" disabled={runtimeConfigSaving || runtimeConfigLoading}>
                            {runtimeConfigSaving
                              ? t("settings.saving")
                              : t("settings.saveProvider", { name: provider.id })}
                          </Button>
                          {credentialFeedback ? (
                            <span className={`settings-save-message${isFeedbackError ? " settings-error" : ""}`}>
                              {credentialFeedback}
                            </span>
                          ) : providerSaveMessage ? (
                            <span className="settings-save-message">{providerSaveMessage}</span>
                          ) : null}
                        </div>
                        <div className="settings-actions">
                          {isProviderKeyCredential(draft.credentialKind) && profileKeyStored ? (
                            <Button
                              type="button"
                              variant="outline"
                              onClick={() => void onRemoveApiKey(provider.id, effectiveProfile)}
                            >
                              {t("settings.removeKey")}
                            </Button>
                          ) : null}
                          {provider.configuredInConfig ? (
                            <Button
                              type="button"
                              variant="outline"
                              disabled={runtimeConfigSaving || runtimeConfigLoading}
                              title={t("settings.removeConfigHint")}
                              onClick={() => void onRemoveProviderConfig(provider.id)}
                            >
                              {t("settings.removeConfig")}
                            </Button>
                          ) : null}
                        </div>
                      </div>
                    </form>

                    <section className="settings-provider-models-panel">
                      <div className="settings-provider-models-head">
                        <div>
                          <h3>{t("modelUi.providerModels")}</h3>
                          <p className="settings-hint">
                            {providerModels.length > 0
                              ? t("settings.providerModelsImmediateHint")
                              : t("settings.noModelsForProvider")}
                          </p>
                        </div>
                        <div className="settings-actions">
                          <span className="settings-muted">
                            {t("modelUi.modelCount", { count: selectableModelCount })}
                          </span>
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            disabled={modelCatalogLoading}
                            onClick={() => void onRefreshModels()}
                          >
                            <RefreshCw size={13} />
                            {modelCatalogLoading ? t("settings.refreshing") : t("agentPage.refreshModelList")}
                          </Button>
                        </div>
                      </div>
                      {providerModels.length > 0 ? (
                        <ModelList
                          options={providerModels}
                          value={
                            providerModels.some((model) => model.routeRef === modelDefault)
                              ? modelDefault
                              : undefined
                          }
                          onSelect={(model) => {
                            void onSelectAsDefaultModel(model.routeRef);
                          }}
                        />
                      ) : null}
                    </section>
                  </div>
                );
              })}
          </div>
        )}
      </Card>
    </div>
  );
}
