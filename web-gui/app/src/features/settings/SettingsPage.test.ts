import { describe, expect, it } from "vitest";

import { buildVisionConfigUpdates, sortProvidersForSettings } from "./SettingsPage";
import type { RuntimeProviderSummary } from "../../runtime/types";

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
