import type {
  RuntimeConfigState,
  RuntimeModelOption,
  RuntimeProviderSummary,
  RuntimeWebSearchProviderKindSummary,
  RuntimeWebSearchProviderSummary,
} from "../../runtime/types";

export function splitCsv(value: string): string[] {
  return value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

export function runtimeReloadMessage(runtimeConfig: RuntimeConfigState): string | undefined {
  const reload = runtimeConfig.reload;
  if (!reload || reload.requestedGeneration === 0) return undefined;
  if (reload.state === "failed") {
    return `Configuration was saved, but reload generation ${reload.requestedGeneration} failed${reload.lastError ? `: ${reload.lastError}` : "."}`;
  }
  if (reload.completedGeneration < reload.requestedGeneration) {
    return `Configuration saved; applying reload generation ${reload.requestedGeneration}…`;
  }
  return `Configuration reload generation ${reload.completedGeneration} completed.`;
}

export function shouldHydrateSettingsDrafts(runtimeConfig: RuntimeConfigState): boolean {
  const results = runtimeConfig.results ?? [];
  return !results.some((result) => result.effect === "rejected")
    && runtimeConfig.reload?.state !== "failed";
}

export function numberFromInput(value: string): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
}

export function buildVisionConfigUpdates(visionDefault: string): Array<{ key: string; value?: unknown; unset?: boolean }> {
  const trimmed = visionDefault.trim();
  return [trimmed ? { key: "vision.default", value: trimmed } : { key: "vision.default", unset: true }];
}

export function buildImageGenerationConfigUpdates(imageGenDefault: string): Array<{ key: string; value?: unknown; unset?: boolean }> {
  const trimmed = imageGenDefault.trim();
  return [trimmed ? { key: "image_generation.default", value: trimmed } : { key: "image_generation.default", unset: true }];
}

export function isVisionSelectableModel(model: RuntimeModelOption): boolean {
  return model.viewImageSelectable ?? (model.available && model.supportsImageInput);
}

export function isImageGenerationSelectableModel(model: RuntimeModelOption): boolean {
  return model.generateImageSelectable ?? (model.available && model.supportsImageGeneration);
}

export function buildDecisionConfigUpdates(
  enabled: boolean,
  model: string,
  modelDir: string,
  variant: string,
  numThreads: string,
  checksum: string,
  preset = "jev-selector-q4f16",
  modelUpdate: "set" | "preserve" = "set",
): Array<{ key: string; value?: unknown; unset?: boolean }> {
  const trimmedModel = model.trim();
  const trimmedModelDir = modelDir.trim();
  const trimmedVariant = variant.trim();
  const parsedNumThreads = Number.parseInt(numThreads.trim(), 10);
  const trimmedChecksum = checksum.trim();
  return [
    enabled ? { key: "decision.enabled", value: true } : { key: "decision.enabled", unset: true },
    ...(modelUpdate === "preserve"
      ? []
      : [
          trimmedModel
            ? { key: "decision.model", value: trimmedModel }
            : { key: "decision.model", unset: true },
        ]),
    preset.trim()
      ? { key: "decision.local_onnx.preset", value: preset.trim() }
      : { key: "decision.local_onnx.preset", unset: true },
    trimmedModelDir
      ? { key: "decision.local_onnx.model_dir", value: trimmedModelDir }
      : { key: "decision.local_onnx.model_dir", unset: true },
    trimmedVariant
      ? { key: "decision.local_onnx.variant", value: trimmedVariant }
      : { key: "decision.local_onnx.variant", unset: true },
    Number.isFinite(parsedNumThreads) && parsedNumThreads > 0
      ? { key: "decision.local_onnx.num_threads", value: parsedNumThreads }
      : { key: "decision.local_onnx.num_threads", unset: true },
    trimmedChecksum
      ? { key: "decision.local_onnx.checksum", value: trimmedChecksum }
      : { key: "decision.local_onnx.checksum", unset: true },
  ];
}

export function buildAdvisoryToolConfigUpdates(
  enabled: boolean,
  maxCallsPerTurn: string,
  timeoutMs: string,
  minConfidencePercent: string,
): Array<{ key: string; value?: unknown; unset?: boolean }> {
  const maxCalls = numberFromInput(maxCallsPerTurn);
  const timeout = numberFromInput(timeoutMs);
  const confidence = Math.min(100, numberFromInput(minConfidencePercent));
  return [
    { key: "decision.tools.enabled", value: enabled },
    { key: "decision.tools.max_calls_per_turn", value: Math.max(1, maxCalls) },
    { key: "decision.tools.timeout_ms", value: Math.max(1, timeout) },
    { key: "decision.tools.min_confidence", value: confidence / 100 },
  ];
}

export function reorderModelFallbacks(
  models: string[],
  fromIndex: number,
  toIndex: number,
): string[] {
  if (
    fromIndex === toIndex
    || fromIndex < 0
    || toIndex < 0
    || fromIndex >= models.length
    || toIndex >= models.length
  ) {
    return models;
  }
  const next = [...models];
  const [moved] = next.splice(fromIndex, 1);
  if (moved === undefined) return models;
  next.splice(toIndex, 0, moved);
  return next;
}

export function filterFallbackSuggestions(
  availableModels: readonly RuntimeModelOption[],
  input: string,
  selectedModels: readonly string[],
): RuntimeModelOption[] {
  const query = input.trim().toLowerCase();
  if (!query) return [];
  const selected = new Set(selectedModels);
  return availableModels
    .filter((model) =>
      !selected.has(model.routeRef)
      && (
        model.model.toLowerCase().includes(query)
        || model.routeRef.toLowerCase().includes(query)
        || model.displayName.toLowerCase().includes(query)
      ))
    .slice(0, 10);
}

export function isDecisionCatalogRoute(
  model: string,
  decisionModels: readonly RuntimeModelOption[],
): boolean {
  const trimmed = model.trim();
  return !trimmed || decisionModels.some((option) => option.routeRef === trimmed);
}

export function isProviderKeyCredential(kind: string): boolean {
  return kind === "api_key" || kind === "bearer_token";
}

export const CUSTOM_PROVIDER_TRANSPORTS = [
  "openai_chat_completions",
  "openai_responses",
  "anthropic_messages",
  "gemini_generate_content",
] as const;

export const CUSTOM_PROVIDER_DISCOVERY_KINDS = ["openai_models", "disabled"] as const;

export type CustomProviderDraft = {
  id: string;
  transport: (typeof CUSTOM_PROVIDER_TRANSPORTS)[number];
  baseUrl: string;
  apiKey: string;
  discoveryKind: (typeof CUSTOM_PROVIDER_DISCOVERY_KINDS)[number];
  discoveryPath: string;
};

export const EMPTY_CUSTOM_PROVIDER_DRAFT: CustomProviderDraft = {
  id: "",
  transport: "openai_chat_completions",
  baseUrl: "",
  apiKey: "",
  discoveryKind: "openai_models",
  discoveryPath: "",
};

export function isAbsoluteHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}

export type ProviderDraft = Pick<
  RuntimeProviderSummary,
  "transport" | "baseUrl" | "oauthSupported" | "apiKeySupported" | "credentialSource" | "credentialKind" | "credentialEnv" | "credentialProfile" | "credentialExternal"
> & {
  discoveryKind: string;
  discoveryPath: string;
  discoveryDecoder: string;
};

export type SearchProviderDraft = Pick<RuntimeWebSearchProviderSummary, "kind" | "baseUrl" | "engineId" | "credentialProfile">;

export type StandardSearchProviderDefinition = {
  id: string;
  kind: string;
  category: "api" | "selfHosted";
  requiresApiKey: boolean;
  supportsApiKey: boolean;
  requiresBaseUrl: boolean;
  defaultCredentialProfile?: string;
  capabilities: RuntimeWebSearchProviderKindSummary["capabilities"];
};

export function buildStandardSearchProviderDefinitions(
  kinds: RuntimeWebSearchProviderKindSummary[],
): StandardSearchProviderDefinition[] {
  return kinds
    .filter(({ capabilities }) =>
      capabilities.status !== "unsupported"
      && (capabilities.auth === "api_key" || capabilities.auth === "api_key_optional" || capabilities.auth === "self_hosted")
    )
    .map(({ kind, capabilities }) => {
      const requiresApiKey = capabilities.auth === "api_key";
      const supportsApiKey = requiresApiKey || capabilities.auth === "api_key_optional";
      return {
        id: kind.replaceAll("_", "-"),
        kind,
        category: capabilities.auth === "self_hosted" ? "selfHosted" as const : "api" as const,
        requiresApiKey,
        supportsApiKey,
        requiresBaseUrl: capabilities.auth === "self_hosted",
        defaultCredentialProfile: supportsApiKey ? `${kind}:default` : undefined,
        capabilities,
      };
    })
    .sort((left, right) => right.capabilities.defaultPriority - left.capabilities.defaultPriority);
}

export function defaultSearchProviderDraft(providerId: string, definition?: StandardSearchProviderDefinition): SearchProviderDraft {
  return {
    kind: definition?.kind ?? "brave",
    baseUrl: "",
    credentialProfile: definition?.requiresApiKey ? definition.defaultCredentialProfile ?? `${providerId}:default` : "",
  };
}

export function buildSearchProviderConfigUpdates(
  providerId: string,
  draft: SearchProviderDraft,
  capabilities?: RuntimeWebSearchProviderKindSummary["capabilities"],
): Array<{ key: string; value?: unknown; unset?: boolean }> {
  const shouldSendBaseUrl = capabilities?.auth !== "native_provider";
  const updates: Array<{ key: string; value?: unknown; unset?: boolean }> = [
    { key: `web.providers.${providerId}.kind`, value: draft.kind },
    { key: `web.providers.${providerId}.credential_profile`, value: draft.credentialProfile?.trim() ?? "" },
  ];
  if (shouldSendBaseUrl) {
    updates.splice(1, 0, { key: `web.providers.${providerId}.base_url`, value: draft.baseUrl?.trim() ?? "" });
  }
  if (draft.kind === "google") {
    updates.push({ key: `web.providers.${providerId}.engine_id`, value: draft.engineId?.trim() ?? "" });
  }
  return updates;
}

export function buildProviderDiscoveryConfigUpdates(
  providerId: string,
  draft: Pick<ProviderDraft, "discoveryKind" | "discoveryPath" | "discoveryDecoder">,
): Array<{ key: string; value?: unknown; unset?: boolean }> {
  const base = `providers.${providerId}.discovery`;
  const kind = draft.discoveryKind.trim();
  if (kind === "") {
    return [
      { key: `${base}.path`, unset: true },
      { key: `${base}.decoder`, unset: true },
      { key: `${base}.kind`, unset: true },
    ];
  }
  const updates: Array<{ key: string; value?: unknown; unset?: boolean }> = [
    { key: `${base}.kind`, value: kind },
  ];
  if (kind === "disabled") {
    updates.push({ key: `${base}.path`, unset: true });
    updates.push({ key: `${base}.decoder`, unset: true });
    return updates;
  }
  const path = draft.discoveryPath.trim();
  updates.push(path ? { key: `${base}.path`, value: path } : { key: `${base}.path`, unset: true });
  const decoder = draft.discoveryDecoder.trim();
  updates.push(decoder ? { key: `${base}.decoder`, value: decoder } : { key: `${base}.decoder`, unset: true });
  return updates;
}

export function groupModelsByProvider(options: RuntimeModelOption[]): Array<[string, RuntimeModelOption[]]> {
  const grouped = new Map<string, RuntimeModelOption[]>();
  for (const option of options) {
    const provider = option.endpoint === "default"
      ? option.providerFamily
      : `${option.providerFamily} / ${option.endpoint}`;
    const models = grouped.get(provider) ?? [];
    models.push(option);
    grouped.set(provider, models);
  }
  return Array.from(grouped.entries()).sort(([left], [right]) => left.localeCompare(right));
}

export function sortProvidersForSettings(providers: RuntimeProviderSummary[]): RuntimeProviderSummary[] {
  const rank = (provider: RuntimeProviderSummary) =>
    provider.credentialKind === "none"
      ? Number(provider.configuredInConfig)
      : Number(provider.credentialConfigured);
  return providers
    .map((provider, index) => ({ provider, index }))
    .sort((a, b) => {
      const credentialRank = rank(b.provider) - rank(a.provider);
      return credentialRank || a.index - b.index;
    })
    .map(({ provider }) => provider);
}

export function providerCredentialReady(provider: RuntimeProviderSummary): boolean {
  return provider.credentialKind === "none" || provider.credentialConfigured;
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
