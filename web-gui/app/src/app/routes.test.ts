import { describe, expect, it } from "vitest";

import { isPlainLeftClick, pathForRoute, routeFromLocation, searchQueryForLocation } from "./routes";

describe("routeFromLocation", () => {
  it("restores the search query and advanced scope without carrying conversation locators", () => {
    const query = searchQueryForLocation({ search: "?q=%E4%B8%AD%E6%96%87+%23%2B%25&scope=all&source_ref=message%3A1&event_seq=42" });
    const path = pathForRoute("search", undefined, undefined, query);
    const url = new URL(path, "http://example.test");
    expect(url.pathname).toBe("/search");
    expect([...url.searchParams.entries()]).toEqual([["q", "中文 #+%"], ["scope", "all"]]);
  });

  it("does not introduce an advanced scope when returning to default or empty search", () => {
    expect(pathForRoute("search", undefined, undefined, searchQueryForLocation({ search: "?q=hello" }))).toBe("/search?q=hello");
    expect(pathForRoute("search", undefined, undefined, searchQueryForLocation({ search: "" }))).toBe("/search");
  });

  it("parses conversation event_seq links", () => {
    expect(routeFromLocation({ pathname: "/agents/holon-pm/conversation", search: "?event_seq=740" })).toEqual({
      route: "agent",
      agentId: "holon-pm",
      eventSeq: 740,
      sourceRef: undefined,
      runtimeId: undefined,
    });
  });

  it("ignores invalid event_seq values", () => {
    expect(routeFromLocation({ pathname: "/agents/holon-pm/conversation", search: "?event_seq=latest" })).toEqual({
      route: "agent",
      agentId: "holon-pm",
      eventSeq: undefined,
      sourceRef: undefined,
      runtimeId: undefined,
    });
  });

  it("round-trips opaque source_ref values without creating cursors", () => {
    const sourceRef = "tool_execution:exec/中文:output:batch #1+%";
    const path = pathForRoute("agent", "agent/中文", undefined, { source_ref: sourceRef });
    const url = new URL(path, "http://example.test");
    expect(routeFromLocation(url)).toEqual({
      route: "agent",
      agentId: "agent/中文",
      sourceRef,
      eventSeq: undefined,
      runtimeId: undefined,
    });
    expect([...url.searchParams.keys()]).toEqual(["source_ref"]);
  });

  it("retains source_ref alongside a legacy event_seq without conflating them", () => {
    expect(routeFromLocation({
      pathname: "/agents/alpha",
      search: "?source_ref=message%3Amsg-1&event_seq=42",
    })).toEqual({ route: "agent", agentId: "alpha", sourceRef: "message:msg-1", eventSeq: 42, runtimeId: undefined });
  });

  it("ignores empty source_ref and invalid event_seq", () => {
    expect(routeFromLocation({
      pathname: "/agents/alpha/conversation",
      search: "?source_ref=&event_seq=-1",
    })).toEqual({ route: "agent", agentId: "alpha", sourceRef: undefined, eventSeq: undefined, runtimeId: undefined });
  });

  it("round-trips runtime-bound source links with opaque runtime IDs", () => {
    const runtimeId = "runtime/中文 #1+%";
    const sourceRef = "message:msg-1";
    const path = pathForRoute("agent", "alpha", undefined, { runtime_id: runtimeId, source_ref: sourceRef });
    expect(routeFromLocation(new URL(path, "http://example.test"))).toEqual({
      route: "agent",
      agentId: "alpha",
      runtimeId,
      sourceRef,
      eventSeq: undefined,
    });
  });

  it("retains runtime binding for legacy agent links", () => {
    expect(routeFromLocation({
      pathname: "/agents/alpha",
      search: "?runtime_id=runtime-1&event_seq=42",
    })).toEqual({
      route: "agent",
      agentId: "alpha",
      runtimeId: "runtime-1",
      eventSeq: 42,
      sourceRef: undefined,
    });
  });

  it("treats empty runtime_id as absent", () => {
    expect(routeFromLocation({
      pathname: "/agents/alpha/conversation",
      search: "?runtime_id=",
    }).runtimeId).toBeUndefined();
  });

  it("round-trips the parent agent behind a sub-conversation link", () => {
    const path = pathForRoute("agent", "child", undefined, { incarnation: 3, from: "parent" });
    expect(path).toBe("/agents/child/conversation?incarnation=3&from=parent");
    expect(routeFromLocation({ pathname: "/agents/child/conversation", search: "?incarnation=3&from=parent" })).toEqual({
      route: "agent",
      agentId: "child",
      eventSeq: undefined,
      sourceRef: undefined,
      runtimeId: undefined,
      fromAgentId: "parent",
    });
  });

  it("treats an empty from parameter as absent", () => {
    expect(routeFromLocation({
      pathname: "/agents/alpha/conversation",
      search: "?from=",
    }).fromAgentId).toBeUndefined();
  });

  it("parses the top-level skills route", () => {
    expect(routeFromLocation({ pathname: "/skills", search: "" })).toEqual({
      route: "skills",
    });
  });

  it("parses skill detail links", () => {
    expect(routeFromLocation({ pathname: "/skills/user_global%3Aghx", search: "" })).toEqual({
      route: "skillDetail",
      skillId: "user_global:ghx",
    });
  });

  it("parses agent-scoped skill detail links", () => {
    expect(routeFromLocation({
      pathname: "/agents/holon-dev2/skills/workspace%3Aroot%3Atrace-report-analysis",
      search: "",
    })).toEqual({
      route: "skillDetail",
      agentId: "holon-dev2",
      skillId: "workspace:root:trace-report-analysis",
    });
  });
});

describe("isPlainLeftClick", () => {
  const base = { defaultPrevented: false, button: 0, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false };

  it("accepts an unmodified primary-button activation", () => {
    expect(isPlainLeftClick(base)).toBe(true);
  });

  it("rejects modified, non-primary, or already-handled activations", () => {
    expect(isPlainLeftClick({ ...base, metaKey: true })).toBe(false);
    expect(isPlainLeftClick({ ...base, ctrlKey: true })).toBe(false);
    expect(isPlainLeftClick({ ...base, shiftKey: true })).toBe(false);
    expect(isPlainLeftClick({ ...base, altKey: true })).toBe(false);
    expect(isPlainLeftClick({ ...base, button: 1 })).toBe(false);
    expect(isPlainLeftClick({ ...base, defaultPrevented: true })).toBe(false);
  });
});
