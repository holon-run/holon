import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { AGENT_AVATAR_VERSION, agentAvatarIndex, renderAgentAvatar, type AgentAvatarExpression } from "./agent-avatar";

// Original V8 seed indices, with compact vivid artwork and neutral default eyes.
const vectors = [
  ["holon-product", 607, "da13eb4780e9f0119736872fdb912b9db5fcd1015b24fd87ea3da73cad7debe4"],
  ["holon-auto-dev", 181, "5292e3cb8a474e235e0a41e7c41c489deffdd88ee97c8d90bc0cb76d8bc59b71"],
  ["holon-integrations", 515, "e8298aad6a1f32194e9ba1d9c5ba941d1282f4969899cd626701979a5ecbba7d"],
  ["bootstrap-agent", 173, "0be8c9238df7891739ded55a609bb6fc7c00a2d56d72ad553e04d62e939c923d"],
  ["智能助手", 1016, "27184e020e3fad89d4fa323c2852011ea8d1887e332d314693f9897060de1752"],
  ["agent-🤖", 477, "08b13a9bb9bf223eb2c1da31cac4ed5e5e06e0d503fc284d1fcfbe2fbe39e10b"],
  ["", 1133, "04b9e88c937703dfc2142038e4375559c9e2ef74a42676d2a8fc6ba2bdc8e3c2"],
  ["holonbot", 502, "9f07b9e5c1ca0a0e5dfc7436dc2fcb9929e0e211e20281ddb92516c28512796a"],
] as const;

describe("deterministic Agent avatars and brand artwork", () => {
  it("freezes the vivid rendering version independently of the identity seed namespace", () => {
    expect(AGENT_AVATAR_VERSION).toBe("light-mech-1-vivid-2");
  });

  it.each(vectors)("defaults to the same neutral expression as the UI for %s", (id) => {
    expect(renderAgentAvatar(id)).toBe(renderAgentAvatar(id, "neutral"));
  });

  it("keeps the exported brand SVG byte-identical to the holonbot avatar", () => {
    const brand = readFileSync(new URL("../../../../docs/website/assets/holonbot.svg", import.meta.url), "utf8");
    expect(brand).toBe(renderAgentAvatar("holonbot"));
    expect(brand).toContain('fill="#36cf9e"');
    expect(renderAgentAvatar("Holonbot")).not.toBe(brand);
  });

  it.each(vectors)("preserves the reviewed mapping and SVG for %s", (id, index, digest) => {
    expect(agentAvatarIndex(id)).toBe(index);
    expect(createHash("sha256").update(renderAgentAvatar(id)).digest("hex")).toBe(digest);
    expect(renderAgentAvatar(id)).toBe(renderAgentAvatar(id));
  });

  it("uses IDs only as hashes, never as SVG content or external resources", () => {
    const attack = '\"><script>alert("avatar")</script><image href="https://evil.invalid"/>';
    const svg = renderAgentAvatar(attack);
    expect(svg).not.toContain(attack);
    expect(svg).not.toMatch(/<script|<image|href=|onload=|evil\.invalid|NaN|undefined/);
    expect(svg).toContain('viewBox="0 0 100 100"');
  });

  it.each(vectors)("changes only the eyes, never the identity artwork for %s", (id) => {
    const expressions: AgentAvatarExpression[] = ["neutral", "focused", "questioning", "waiting", "sleeping"];
    const images = expressions.map((expression) => renderAgentAvatar(id, expression));
    const identity = (svg: string) => svg.replace(/<g data-avatar-part="expression">.*?<\/g>/, "");
    expect(new Set(images.map(identity)).size).toBe(1);
    expect(new Set(images).size).toBe(expressions.length);
    expect(images.join("")).not.toMatch(/<animate|<script|href=|NaN|undefined/);
  });

  it("does not interpolate an unrecognized expression into SVG", () => {
    expect(renderAgentAvatar("agent", '"><script>evil</script>' as AgentAvatarExpression))
      .toBe(renderAgentAvatar("agent", "neutral"));
  });
});
