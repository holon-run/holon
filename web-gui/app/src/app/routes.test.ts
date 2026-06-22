import { describe, expect, it } from "vitest";

import { routeFromLocation } from "./routes";

describe("routeFromLocation", () => {
  it("parses conversation event_seq links", () => {
    expect(routeFromLocation({ pathname: "/agents/holon-pm/conversation", search: "?event_seq=740" })).toEqual({
      route: "agent",
      agentId: "holon-pm",
      eventSeq: 740,
    });
  });

  it("ignores invalid event_seq values", () => {
    expect(routeFromLocation({ pathname: "/agents/holon-pm/conversation", search: "?event_seq=latest" })).toEqual({
      route: "agent",
      agentId: "holon-pm",
      eventSeq: undefined,
    });
  });
});
