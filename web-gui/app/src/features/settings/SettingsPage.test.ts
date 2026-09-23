import { describe, expect, it } from "vitest";

import {
  buildDecisionConfigUpdates,
  buildImageGenerationConfigUpdates,
  buildSearchProviderConfigUpdates,
  buildStandardSearchProviderDefinitions,
  buildVisionConfigUpdates,
  filterFallbackSuggestions,
  isDecisionCatalogRoute,
  providerCredentialReady,
  reorderModelFallbacks,
  runtimeReloadMessage,
  sortProvidersForSettings,
  sortSearchProvidersForSettings,
} from "./SettingsPage";
import type {
  RuntimeModelOption,
  RuntimeProviderSummary,
  RuntimeWebSearchProviderCapabilities,
  RuntimeWebSearchProviderSummary,
} from "../../runtime/types";

function provider(id: string, credentialConfigured: boolean): RuntimeProviderSummary {
  return {
    id,
    oauthSupported: false,
    transport: "openai",
    baseUrl: "https://example.test/v1",
    apiKeySupported: true,
    credentialSource: "credential_profile",
    credentialKind: "api_key",
    credentialProfile: `${id}:default`,
    credentialConfigured,
    configuredInConfig: true,
  };
}

function searchProvider(id: string, credentialConfigured: boolean): RuntimeWebSearchProviderSummary {
  return {
    id,
    kind: "brave",
    credentialProfile: `${id}:default`,
    credentialConfigured,
  };
}

function searchCapabilities(
  auth: RuntimeWebSearchProviderCapabilities["auth"],
  defaultPriority: number,
): RuntimeWebSearchProviderCapabilities {
  return {
    auth,
    costClass: auth === "self_hosted" ? "self_hosted" : auth === "native_provider" ? "provider_metered" : "paid",
    qualityHint: auth === "native_provider" ? "native" : "research",
    supportsDomainFilter: false,
    supportsFreshness: false,
    supportsRegionOrLanguage: false,
    supportsFullContent: false,
    supportsNativeCitations: false,
    defaultPriority,
    status: auth === "native_provider" ? "native_only" : "supported",
  };
}

describe("providerCredentialReady", () => {
  it("treats credential-free providers as ready without stored credentials", () => {
    expect(providerCredentialReady({
      ...provider("ollama", false),
      apiKeySupported: false,
      credentialSource: "none",
      credentialKind: "none",
      credentialProfile: undefined,
    })).toBe(true);
  });

  it("still requires configured credentials for API-key providers", () => {
    expect(providerCredentialReady(provider("openai", false))).toBe(false);
  });
});

describe("isDecisionCatalogRoute", () => {
  const options: RuntimeModelOption[] = [{
    model: "jev-latest",
    routeRef: "typesafe@default/jev-latest",
    provider: "typesafe",
    providerFamily: "typesafe",
    endpoint: "default",
    routeProvider: "typesafe",
    displayName: "JEV latest",
    available: false,
    decisionCapable: true,
    decisionProtocol: "jev",
    supportsImageInput: false,
    supportsImageGeneration: false,
    supportsReasoningEffort: false,
    reasoningEffortOptions: [],
  }];

  it("matches the persisted route reference rather than the bare model name", () => {
    expect(isDecisionCatalogRoute("typesafe@default/jev-latest", options)).toBe(true);
    expect(isDecisionCatalogRoute("jev-latest", options)).toBe(false);
  });

  it("accepts an empty route as the explicit no-remote choice", () => {
    expect(isDecisionCatalogRoute("  ", options)).toBe(true);
  });
});

describe("buildStandardSearchProviderDefinitions", () => {
  it("derives groups and configuration requirements from runtime capabilities", () => {
    const definitions = buildStandardSearchProviderDefinitions([
      { kind: "future_api", capabilities: searchCapabilities("api_key", 90) },
      { kind: "future_self_hosted", capabilities: searchCapabilities("self_hosted", 40) },
      { kind: "duck_duck_go", capabilities: searchCapabilities("none", 10) },
      {
        kind: "future_unsupported",
        capabilities: { ...searchCapabilities("api_key", 100), status: "unsupported" },
      },
    ]);

    expect(definitions.map(({ id, category, requiresApiKey, requiresBaseUrl }) => ({
      id,
      category,
      requiresApiKey,
      requiresBaseUrl,
    }))).toEqual([
      { id: "future-api", category: "api", requiresApiKey: true, requiresBaseUrl: false },
      { id: "future-self-hosted", category: "selfHosted", requiresApiKey: false, requiresBaseUrl: true },
    ]);
  });
});

describe("sortProvidersForSettings", () => {
  it("places credential-configured providers first without reordering peers", () => {
    const sorted = sortProvidersForSettings([
      provider("missing-a", false),
      provider("ready-a", true),
      provider("missing-b", false),
      provider("ready-b", true),
    ]);

    expect(sorted.map((entry) => entry.id)).toEqual(["ready-a", "ready-b", "missing-a", "missing-b"]);
  });
});

describe("sortSearchProvidersForSettings", () => {
  it("places credential-configured search providers first without reordering peers", () => {
    const sorted = sortSearchProvidersForSettings([
      searchProvider("missing-a", false),
      searchProvider("ready-a", true),
      searchProvider("missing-b", false),
      searchProvider("ready-b", true),
    ]);

    expect(sorted.map((entry) => entry.id)).toEqual(["ready-a", "ready-b", "missing-a", "missing-b"]);
  });
});

describe("buildVisionConfigUpdates", () => {
  it("persists a trimmed Vision default model", () => {
    expect(buildVisionConfigUpdates(" openai/gpt-5.1 ")).toEqual([
      { key: "vision.default", value: "openai/gpt-5.1" },
    ]);
  });

  it("unsets Vision default when left empty for auto-discovery", () => {
    expect(buildVisionConfigUpdates("   ")).toEqual([{ key: "vision.default", unset: true }]);
  });
});

describe("buildImageGenerationConfigUpdates", () => {
  it("persists a trimmed image generation default model", () => {
    expect(buildImageGenerationConfigUpdates(" openai/gpt-image-1 ")).toEqual([
      { key: "image_generation.default", value: "openai/gpt-image-1" },
    ]);
  });

  it("unsets image generation default when left empty for auto-selection", () => {
    expect(buildImageGenerationConfigUpdates("   ")).toEqual([{ key: "image_generation.default", unset: true }]);
  });
});

describe("buildDecisionConfigUpdates", () => {
  it("persists a shared model route without duplicating provider settings", () => {
    expect(
      buildDecisionConfigUpdates(true, " jev-decision-1 ", "", "", "", ""),
    ).toEqual([
      { key: "decision.enabled", value: true },
      { key: "decision.model", value: "jev-decision-1" },
      { key: "decision.local_onnx.preset", value: "jev-selector-q4f16" },
      { key: "decision.local_onnx.model_dir", unset: true },
      { key: "decision.local_onnx.variant", unset: true },
      { key: "decision.local_onnx.num_threads", unset: true },
      { key: "decision.local_onnx.checksum", unset: true },
    ]);
  });

  it("unsets every Decision key when disabled and left empty", () => {
    expect(buildDecisionConfigUpdates(false, "", "", "", "", "")).toEqual([
      { key: "decision.enabled", unset: true },
      { key: "decision.model", unset: true },
      { key: "decision.local_onnx.preset", value: "jev-selector-q4f16" },
      { key: "decision.local_onnx.model_dir", unset: true },
      { key: "decision.local_onnx.variant", unset: true },
      { key: "decision.local_onnx.num_threads", unset: true },
      { key: "decision.local_onnx.checksum", unset: true },
    ]);
  });

  it("keeps a shared model route as free text without catalog rewrite", () => {
    expect(buildDecisionConfigUpdates(true, "jev/decision-pro", "", "", "", "")).toEqual([
      { key: "decision.enabled", value: true },
      { key: "decision.model", value: "jev/decision-pro" },
      { key: "decision.local_onnx.preset", value: "jev-selector-q4f16" },
      { key: "decision.local_onnx.model_dir", unset: true },
      { key: "decision.local_onnx.variant", unset: true },
      { key: "decision.local_onnx.num_threads", unset: true },
      { key: "decision.local_onnx.checksum", unset: true },
    ]);
  });

  it("persists local ONNX assets under their independent config namespace", () => {
    expect(
      buildDecisionConfigUpdates(true, "", " /models/decision ", "q4f16", " 4 ", " sha256:abc "),
    ).toEqual([
      { key: "decision.enabled", value: true },
      { key: "decision.model", unset: true },
      { key: "decision.local_onnx.preset", value: "jev-selector-q4f16" },
      { key: "decision.local_onnx.model_dir", value: "/models/decision" },
      { key: "decision.local_onnx.variant", value: "q4f16" },
      { key: "decision.local_onnx.num_threads", value: 4 },
      { key: "decision.local_onnx.checksum", value: "sha256:abc" },
    ]);
  });

  it("keeps local preset fields independent from the shared model route", () => {
    expect(
      buildDecisionConfigUpdates(true, "gpt-4.1", "/models/decision", "q4f16", "4", "sha256:abc"),
    ).toEqual([
      { key: "decision.enabled", value: true },
      { key: "decision.model", value: "gpt-4.1" },
      { key: "decision.local_onnx.preset", value: "jev-selector-q4f16" },
      { key: "decision.local_onnx.model_dir", value: "/models/decision" },
      { key: "decision.local_onnx.variant", value: "q4f16" },
      { key: "decision.local_onnx.num_threads", value: 4 },
      { key: "decision.local_onnx.checksum", value: "sha256:abc" },
    ]);
  });

  it("preserves a configured non-catalog route when saving other Decision settings", () => {
    const updates = buildDecisionConfigUpdates(
      true,
      "legacy/decision-route",
      "",
      "",
      "",
      "",
      "jev-selector-q4f16",
      "preserve",
    );

    expect(updates).not.toContainEqual({ key: "decision.model", value: "legacy/decision-route" });
    expect(updates).not.toContainEqual({ key: "decision.model", unset: true });
  });
});

describe("fallback model settings helpers", () => {
  it("reorders fallback models without mutating the input", () => {
    const models = ["a", "b", "c"];

    expect(reorderModelFallbacks(models, 0, 2)).toEqual(["b", "c", "a"]);
    expect(models).toEqual(["a", "b", "c"]);
    expect(reorderModelFallbacks(models, 1, 1)).toBe(models);
  });

  it("filters fallback suggestions using route and display names", () => {
    const models = [
      {
        model: "gpt-5",
        routeRef: "openai/default/gpt-5",
        provider: "openai",
        providerFamily: "openai",
        endpoint: "default",
        routeProvider: "openai",
        displayName: "GPT-5",
        available: true,
        supportsImageInput: false,
        supportsImageGeneration: false,
        supportsReasoningEffort: false,
        reasoningEffortOptions: [],
      },
      {
        model: "claude-sonnet",
        routeRef: "anthropic/default/claude-sonnet",
        provider: "anthropic",
        providerFamily: "anthropic",
        endpoint: "default",
        routeProvider: "anthropic",
        displayName: "Sonnet",
        available: true,
        supportsImageInput: false,
        supportsImageGeneration: false,
        supportsReasoningEffort: false,
        reasoningEffortOptions: [],
      },
    ];

    expect(filterFallbackSuggestions(models, "son", [])).toEqual([models[1]]);
    expect(filterFallbackSuggestions(models, "gpt", [models[0].routeRef])).toEqual([]);
  });
});

describe("buildSearchProviderConfigUpdates", () => {
  it("persists a standard API-backed provider profile without exposing kind selection to the caller", () => {
    expect(
      buildSearchProviderConfigUpdates("brave", {
        kind: "brave",
        baseUrl: "",
        credentialProfile: " brave:default ",
      }),
    ).toEqual([
      { key: "web.providers.brave.kind", value: "brave" },
      { key: "web.providers.brave.base_url", value: "" },
      { key: "web.providers.brave.credential_profile", value: "brave:default" },
    ]);
  });

  it("does not require a credential profile for no-key providers", () => {
    expect(
      buildSearchProviderConfigUpdates("searxng", {
        kind: "searxng",
        baseUrl: " https://search.example.test ",
        credentialProfile: "",
      }),
    ).toEqual([
      { key: "web.providers.searxng.kind", value: "searxng" },
      { key: "web.providers.searxng.base_url", value: "https://search.example.test" },
      { key: "web.providers.searxng.credential_profile", value: "" },
    ]);
  });

  it("omits base_url for native search providers that do not need one", () => {
    const capabilities = searchCapabilities("native_provider", 65);
    expect(
      buildSearchProviderConfigUpdates("openai-native", {
        kind: "open_ai_native",
        baseUrl: "",
        credentialProfile: "",
      }, capabilities),
    ).toEqual([
      { key: "web.providers.openai-native.kind", value: "open_ai_native" },
      { key: "web.providers.openai-native.credential_profile", value: "" },
    ]);
  });

  it("uses runtime capabilities for future native provider kinds", () => {
    expect(
      buildSearchProviderConfigUpdates("future-native", {
        kind: "future_native",
        baseUrl: "should-not-be-persisted",
        credentialProfile: "",
      }, searchCapabilities("native_provider", 65)),
    ).toEqual([
      { key: "web.providers.future-native.kind", value: "future_native" },
      { key: "web.providers.future-native.credential_profile", value: "" },
    ]);
  });
});

describe("runtimeReloadMessage", () => {
  it("distinguishes applying, completed, and failed reloads", () => {
    expect(runtimeReloadMessage({
      source: "http",
      reload: {
        requestedGeneration: 3,
        completedGeneration: 2,
        state: "applying",
      },
    })).toContain("applying reload generation 3");
    expect(runtimeReloadMessage({
      source: "http",
      reload: {
        requestedGeneration: 3,
        completedGeneration: 3,
        state: "completed",
      },
    })).toContain("generation 3 completed");
    expect(runtimeReloadMessage({
      source: "http",
      reload: {
        requestedGeneration: 4,
        completedGeneration: 3,
        state: "failed",
        lastError: "provider rebuild failed",
      },
    })).toContain("provider rebuild failed");
  });
});
