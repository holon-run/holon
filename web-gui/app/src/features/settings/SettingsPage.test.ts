import { describe, expect, it } from "vitest";

import {
  buildSearchProviderConfigUpdates,
  buildVisionConfigUpdates,
  sortProvidersForSettings,
  sortSearchProvidersForSettings,
  supportsOAuthDeviceLogin,
} from "./SettingsPage";
import type { RuntimeProviderSummary, RuntimeWebSearchProviderSummary } from "../../runtime/types";

function provider(id: string, credentialConfigured: boolean): RuntimeProviderSummary {
  return {
    id,
    transport: "openai",
    baseUrl: "https://example.test/v1",
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
});

describe("supportsOAuthDeviceLogin", () => {
  it("accepts openai-codex and xai", () => {
    expect(supportsOAuthDeviceLogin("openai-codex")).toBe(true);
    expect(supportsOAuthDeviceLogin("xai")).toBe(true);
  });

  it("rejects other providers", () => {
    expect(supportsOAuthDeviceLogin("openai")).toBe(false);
  });
});
