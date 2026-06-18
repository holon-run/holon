import { describe, expect, it } from "vitest";

import { sortProvidersForSettings } from "./SettingsPage";
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
