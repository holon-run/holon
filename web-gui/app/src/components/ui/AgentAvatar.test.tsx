import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AgentAvatar } from "./AgentAvatar";
import { renderAgentAvatar } from "../../lib/agent-avatar";

describe("AgentAvatar", () => {
  it("renders a decorative local image without replacing the adjacent accessible name", () => {
    const markup = renderToStaticMarkup(<AgentAvatar agentId="holon-product" />);
    expect(markup).toContain('class="agent-badge agent-avatar"');
    expect(markup).toContain('alt=""');
    expect(markup).toContain('aria-hidden="true"');
    expect(markup).toContain('width="36" height="36"');
    expect(markup).toContain(encodeURIComponent(renderAgentAvatar("holon-product", "neutral")));
    expect(markup).not.toContain("/api/");
  });

  it("does not change the image when the Agent display name changes", () => {
    const card = (name: string) => renderToStaticMarkup(
      <div><AgentAvatar agentId="stable-id" /><strong>{name}</strong></div>,
    );
    const image = (markup: string) => markup.match(/<img[^>]+>/)?.[0];
    expect(image(card("Original name"))).toBe(image(card("Renamed agent")));
  });

  it("renders the requested expression without replacing its decorative semantics", () => {
    const markup = renderToStaticMarkup(<AgentAvatar agentId="holon-product" expression="questioning" />);
    expect(markup).toContain('data-expression="questioning"');
    expect(markup).toContain(encodeURIComponent(renderAgentAvatar("holon-product", "questioning")));
    expect(markup).toContain('alt=""');
    expect(markup).toContain('aria-hidden="true"');
  });
});
