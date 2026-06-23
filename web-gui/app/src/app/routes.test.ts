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
});
