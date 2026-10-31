import { describe, expect, it, vi } from "vitest";

import { refreshSelectedAgentResources, registerSelectedAgentRefresh } from "./selected-agent-resource-invalidation";

describe("selected resource invalidation owners", () => {
  it("notifies both core and visible detail without replacing either subscription", () => {
    const core = vi.fn();
    const detail = vi.fn();
    const removeCore = registerSelectedAgentRefresh("invalidation-a", core);
    const removeDetail = registerSelectedAgentRefresh("invalidation-a", detail);
    expect(refreshSelectedAgentResources("invalidation-a")).toBe(true);
    expect(core).toHaveBeenCalledOnce();
    expect(detail).toHaveBeenCalledOnce();
    removeCore();
    refreshSelectedAgentResources("invalidation-a");
    expect(core).toHaveBeenCalledOnce();
    expect(detail).toHaveBeenCalledTimes(2);
    removeDetail();
    expect(refreshSelectedAgentResources("invalidation-a")).toBe(false);
  });

  it("does not let an old cleanup remove a replacement owner", () => {
    const removeOld = registerSelectedAgentRefresh("invalidation-b", vi.fn());
    removeOld();
    const replacement = vi.fn();
    const removeNew = registerSelectedAgentRefresh("invalidation-b", replacement);
    removeOld();
    refreshSelectedAgentResources("invalidation-b");
    expect(replacement).toHaveBeenCalledOnce();
    removeNew();
  });
});
