import { describe, expect, it } from "vitest";

import {
  buildImageGenerationConfigUpdates,
  buildSearchProviderConfigUpdates,
  buildStandardSearchProviderDefinitions,
  buildVisionConfigUpdates,
  sortProvidersForSettings,
  sortSearchProvidersForSettings,
} from "./SettingsPage";
import type {
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
