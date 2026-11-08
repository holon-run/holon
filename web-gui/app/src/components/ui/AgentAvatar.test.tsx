// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AgentAvatar } from "./AgentAvatar";
import { renderAgentAvatar } from "../../lib/agent-avatar";
import { AGENT_AVATAR_CAPABILITY, agentAvatarResources } from "../../runtime/agent-avatar-resources";

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

describe("AgentAvatar authenticated resource lifecycle", () => {
  let root: Root;
  let container: HTMLDivElement;
  beforeEach(() => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    agentAvatarResources.reset();
    let sequence = 0;
    vi.spyOn(URL, "createObjectURL").mockImplementation(() => `blob:avatar-${++sequence}`);
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });
  afterEach(() => {
    act(() => root.unmount());
    agentAvatarResources.reset();
    container.remove();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });
  const image = () => container.querySelector("img")!;
  const blob = () => new Blob(["png"], { type: "image/png" });

  it("does not block names or list rendering while the authenticated image is pending", async () => {
    let resolve!: (value: { blob: Blob }) => void;
    const loader = vi.fn(() => new Promise<{ blob: Blob }>((done) => { resolve = done; }));
    agentAvatarResources.configure(loader, [AGENT_AVATAR_CAPABILITY]);
    await act(async () => {
      root.render(<div><AgentAvatar agentId="a" /><span>Visible name</span></div>);
    });
    expect(container.textContent).toBe("Visible name");
    expect(image().getAttribute("src")).toMatch(/^data:image\/svg\+xml/);
    await act(async () => { resolve({ blob: blob() }); });
    expect(image().getAttribute("src")).toBe("blob:avatar-1");
    expect(image().getAttribute("alt")).toBe("");
    expect(image().getAttribute("aria-hidden")).toBe("true");
    expect(loader).toHaveBeenCalledOnce();
  });

  it("deduplicates roster and navigation images and preserves URLs through a display-name rename", async () => {
    const loader = vi.fn(async () => ({ blob: blob() }));
    agentAvatarResources.configure(loader, [AGENT_AVATAR_CAPABILITY]);
    const card = (name: string) => <div>
      <AgentAvatar agentId="a" /><span>{name}</span><AgentAvatar agentId="a" />
    </div>;
    await act(async () => { root.render(card("Original")); });
    expect([...container.querySelectorAll("img")].map((item) => item.getAttribute("src")))
      .toEqual(["blob:avatar-1", "blob:avatar-1"]);
    await act(async () => { root.render(card("Renamed")); });
    expect(loader).toHaveBeenCalledOnce();
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    await act(async () => root.render(null));
    expect(URL.revokeObjectURL).toHaveBeenCalledExactlyOnceWith("blob:avatar-1");
  });

  it("keeps expression-specific local fallback visible until the new image arrives", async () => {
    let resolve!: (value: { blob: Blob }) => void;
    const loader = vi.fn().mockResolvedValueOnce({ blob: blob() })
      .mockImplementationOnce(() => new Promise<{ blob: Blob }>((done) => { resolve = done; }));
    agentAvatarResources.configure(loader, [AGENT_AVATAR_CAPABILITY]);
    await act(async () => { root.render(<AgentAvatar agentId="a" />); });
    expect(image().getAttribute("src")).toBe("blob:avatar-1");
    await act(async () => { root.render(<AgentAvatar agentId="a" expression="questioning" />); });
    expect(image().getAttribute("data-expression")).toBe("questioning");
    expect(image().getAttribute("src")).toContain(encodeURIComponent(renderAgentAvatar("a", "questioning")));
    await act(async () => { resolve({ blob: blob() }); });
    expect(image().getAttribute("src")).toBe("blob:avatar-2");
    expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:avatar-1");
  });

  it("immediately hides old-scope bytes on logout and ignores delayed old-scope image results", async () => {
    const loader = vi.fn(async () => ({ blob: blob() }));
    agentAvatarResources.configure(loader, [AGENT_AVATAR_CAPABILITY]);
    await act(async () => { root.render(<AgentAvatar agentId="a" />); });
    expect(image().getAttribute("src")).toBe("blob:avatar-1");
    act(() => agentAvatarResources.reset());
    expect(image().getAttribute("src")).toMatch(/^data:image\/svg\+xml/);
    expect(URL.revokeObjectURL).toHaveBeenCalledExactlyOnceWith("blob:avatar-1");
    let resolve!: (value: { blob: Blob }) => void;
    const oldLoader = vi.fn(() => new Promise<{ blob: Blob }>((done) => { resolve = done; }));
    await act(async () => agentAvatarResources.configure(oldLoader, [AGENT_AVATAR_CAPABILITY]));
    act(() => agentAvatarResources.reset());
    await act(async () => { resolve({ blob: blob() }); });
    expect(image().getAttribute("src")).toMatch(/^data:image\/svg\+xml/);
    expect(URL.createObjectURL).toHaveBeenCalledOnce();
  });

  it("uses local artwork for old daemons and decode errors without inserting an API image URL", async () => {
    const loader = vi.fn(async () => ({ blob: blob() }));
    agentAvatarResources.configure(loader, []);
    await act(async () => { root.render(<AgentAvatar agentId="a" />); });
    expect(loader).not.toHaveBeenCalled();
    expect(image().getAttribute("src")).toMatch(/^data:image\/svg\+xml/);
    await act(async () => agentAvatarResources.configure(loader, [AGENT_AVATAR_CAPABILITY]));
    expect(image().getAttribute("src")).toBe("blob:avatar-1");
    act(() => { image().dispatchEvent(new Event("error")); });
    expect(image().getAttribute("src")).toMatch(/^data:image\/svg\+xml/);
    expect(container.innerHTML).not.toContain("/api/");
  });
});
