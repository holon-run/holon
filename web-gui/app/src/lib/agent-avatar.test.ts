import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";
import { AGENT_AVATAR_VERSION, agentAvatarIndex, renderAgentAvatar, type AgentAvatarExpression } from "./agent-avatar";

// Golden vectors from the reviewed V8.1 vivid generator, excluding its descriptive title.
const vectors = [
  ["holon-product", 607, "357693e47a8fe69cb23bdadf4437a0ad96363e8fbc8ab15ae26426745125c2de"],
  ["holon-auto-dev", 181, "65b3aaaf4babb480098cc2203206774ec7a6d7f1e653d67f05e108423346931a"],
  ["holon-integrations", 515, "9a77f1a344fb0484ed1b43c04e83c554886d7e846d966e668204c521a7094c2b"],
  ["bootstrap-agent", 173, "0f7988a7725274c1c7b289d4d18adc151cb0d316234e16831d50da2112c5d2a9"],
  ["智能助手", 1016, "f550c7bd806ab36bcb1de9ed0ee07f44655c597cad0842aa6c1495df7a4d5217"],
  ["agent-🤖", 477, "ec6ebcc01094d1eb07d0f7cc315d9bdca1907f4e71964ec880cf54bca5696fe9"],
  ["", 1133, "923906b98f600ba04c118b7a863cc84faba48c040e2f4167e30316c660637e17"],
] as const;

describe("fixed V8.1 Agent avatars", () => {
  it("freezes the vivid rendering version independently of the identity seed namespace", () => {
    expect(AGENT_AVATAR_VERSION).toBe("light-mech-1-vivid");
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
