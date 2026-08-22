import "fake-indexeddb/auto";

import { describe, expect, it } from "vitest";

import {
  CACHE_SCHEMA_VERSION,
  cacheGetModelCatalog,
  cachePutModelCatalog,
} from "./idb-cache";

describe("model catalog cache", () => {
  it("persists catalogs independently for each runtime key", async () => {
    await cachePutModelCatalog({
      remoteKey: "http://runtime-a.test",
      schemaVersion: CACHE_SCHEMA_VERSION,
      options: [{ routeRef: "openai@default/gpt-5.4" }],
      cachedAt: 10,
    });
    await cachePutModelCatalog({
      remoteKey: "http://runtime-b.test",
      schemaVersion: CACHE_SCHEMA_VERSION,
      options: [{ routeRef: "anthropic@default/claude-sonnet-4-6" }],
      cachedAt: 20,
    });

    expect(await cacheGetModelCatalog("http://runtime-a.test")).toEqual(expect.objectContaining({
      cachedAt: 10,
      options: [{ routeRef: "openai@default/gpt-5.4" }],
    }));
    expect(await cacheGetModelCatalog("http://runtime-b.test")).toEqual(expect.objectContaining({
      cachedAt: 20,
      options: [{ routeRef: "anthropic@default/claude-sonnet-4-6" }],
    }));
  });

  it("ignores catalogs from an incompatible schema", async () => {
    await cachePutModelCatalog({
      remoteKey: "incompatible-schema",
      schemaVersion: CACHE_SCHEMA_VERSION - 1,
      options: [{ routeRef: "openai@default/gpt-5.4" }],
      cachedAt: 10,
    });

    expect(await cacheGetModelCatalog("incompatible-schema")).toBeUndefined();
  });
});
