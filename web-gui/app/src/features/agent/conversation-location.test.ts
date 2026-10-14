// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from "vitest";
import { captureConversationReadingAnchors, findConversationAnchor, positionConversationAnchor, revealConversationAnchor, restoreConversationReadingAnchor } from "./conversation-location";

afterEach(() => { document.body.replaceChildren(); });

describe("canonical conversation DOM positioning", () => {
  it("matches opaque IDs exactly without selector interpolation or leaving the container", () => {
    const outside = document.createElement("div");
    const container = document.createElement("div");
    const target = document.createElement("article");
    target.dataset.conversationAnchor = 'input:msg"]#异';
    outside.dataset.conversationAnchor = target.dataset.conversationAnchor;
    container.append(target);
    document.body.append(outside, container);
    expect(findConversationAnchor(container, 'input:msg"]#异')).toBe(target);
    expect(findConversationAnchor(container, "input:msg")).toBeNull();
  });

  it("opens source disclosures but waits for React-controlled execution expansion", () => {
    const container = document.createElement("div");
    container.innerHTML = '<details><div data-conversation-anchor="input:m"></div></details>';
    document.body.append(container);
    const target = findConversationAnchor(container, "input:m")!;
    expect(revealConversationAnchor(container, "input:m")).toBe(target);
    expect(container.querySelector("details")!.open).toBe(true);
    target.parentElement!.setAttribute("aria-hidden", "true");
    expect(revealConversationAnchor(container, "input:m")).toBeNull();
  });

  it("centres only the chat viewport and focuses without a second document scroll", () => {
    const container = document.createElement("div");
    const target = document.createElement("article");
    container.append(target);
    document.body.append(container);
    Object.defineProperty(container, "clientHeight", { value: 400 });
    container.getBoundingClientRect = () => ({ top: 100 } as DOMRect);
    target.getBoundingClientRect = () => ({ top: 900, height: 100 } as DOMRect);
    container.scrollTop = 20;
    expect(positionConversationAnchor(container, target)).toBe(true);
    expect(container.scrollTop).toBe(670);
    expect(document.activeElement).toBe(target);
    target.getBoundingClientRect = () => ({ top: 900, height: 0 } as DOMRect);
    expect(positionConversationAnchor(container, target)).toBe(false);
  });

  it("preserves reading offsets through page replacement with the same canonical ID", () => {
    const container = document.createElement("div");
    const old = document.createElement("div");
    old.dataset.conversationAnchor = "input:stable";
    const replacement = old.cloneNode() as HTMLElement;
    container.append(replacement);
    document.body.append(container);
    container.getBoundingClientRect = () => ({ top: 50 } as DOMRect);
    replacement.getBoundingClientRect = () => ({ top: 250, height: 30 } as DOMRect);
    container.scrollTop = 100;
    expect(restoreConversationReadingAnchor(container, [{ element: old, anchor: "input:stable", offset: 70 }])).toBe(true);
    expect(container.scrollTop).toBe(230);
  });

  it("keeps a located source fixed when earlier content in the same turn hydrates", () => {
    const container = document.createElement("div");
    container.innerHTML = '<section data-conversation-anchor="turn:t"><div data-conversation-anchor="input:earlier"></div><article data-conversation-anchor="brief:target" data-location-target="true"></article></section>';
    document.body.append(container);
    Object.defineProperty(container, "clientHeight", { value: 400 });
    container.getBoundingClientRect = () => ({ top: 100 } as DOMRect);
    let growth = 0;
    const turn = container.firstElementChild as HTMLElement;
    const earlier = turn.firstElementChild as HTMLElement;
    const target = turn.lastElementChild as HTMLElement;
    const bounds = (top: number, height: number) => ({ top: 100 + top - container.scrollTop, bottom: 100 + top + height - container.scrollTop, height } as DOMRect);
    turn.getBoundingClientRect = () => bounds(-100, 600 + growth);
    earlier.getBoundingClientRect = () => bounds(20, 30);
    target.getBoundingClientRect = () => bounds(200 + growth, 100);
    const anchors = captureConversationReadingAnchors(container);
    expect(anchors[0].element).toBe(target);
    growth = 300;
    expect(restoreConversationReadingAnchor(container, anchors)).toBe(true);
    expect(container.scrollTop).toBe(300);
    expect(target.getBoundingClientRect().top).toBe(300);
  });

  it("captures the newly selected reading offset after a real user scroll", () => {
    const container = document.createElement("div");
    container.innerHTML = '<section data-conversation-anchor="turn:t"><div data-conversation-anchor="input:old" data-location-target="true"></div><div data-conversation-anchor="input:new"></div></section>';
    document.body.append(container);
    Object.defineProperty(container, "clientHeight", { value: 400 });
    container.getBoundingClientRect = () => ({ top: 0 } as DOMRect);
    let growth = 0;
    const [turn, old, next] = Array.from(container.querySelectorAll<HTMLElement>("[data-conversation-anchor]"));
    const bounds = (top: number, height: number) => ({ top: top - container.scrollTop, bottom: top + height - container.scrollTop, height } as DOMRect);
    turn.getBoundingClientRect = () => bounds(0, 1000);
    old.getBoundingClientRect = () => bounds(50, 50);
    next.getBoundingClientRect = () => bounds(600 + growth, 50);
    container.scrollTop = 500;
    const anchors = captureConversationReadingAnchors(container);
    expect(anchors[0].element).toBe(next);
    growth = 80;
    restoreConversationReadingAnchor(container, anchors);
    expect(container.scrollTop).toBe(580);
    expect(next.getBoundingClientRect().top).toBe(100);
  });

  it("uses a surviving concrete sibling after browse page eviction, not its turn ancestor", () => {
    const container = document.createElement("div");
    container.innerHTML = '<section data-conversation-anchor="turn:a"><div data-conversation-anchor="input:evicted"></div></section><section data-conversation-anchor="turn:b"><div data-conversation-anchor="brief:survivor"></div></section>';
    document.body.append(container);
    container.getBoundingClientRect = () => ({ top: 0 } as DOMRect);
    container.scrollTop = 300;
    let displacement = 0;
    const elements = Array.from(container.querySelectorAll<HTMLElement>("[data-conversation-anchor]"));
    for (const [index, element] of elements.entries()) {
      element.getBoundingClientRect = () => ({
        top: (index < 2 ? 320 : 800) + displacement - container.scrollTop,
        bottom: (index < 2 ? 400 : 900) + displacement - container.scrollTop,
        height: 80,
      } as DOMRect);
    }
    const anchors = captureConversationReadingAnchors(container);
    expect(anchors.slice(0, 2).map((entry) => entry.anchor)).toEqual(["input:evicted", "brief:survivor"]);
    elements[0].remove();
    displacement = -200;
    restoreConversationReadingAnchor(container, anchors);
    expect(container.scrollTop).toBe(100);
    expect(elements[3].getBoundingClientRect().top).toBe(500);
  });

  it("does not record zero-height, hidden or inert content as a reading target", () => {
    const container = document.createElement("div");
    container.innerHTML = '<div aria-hidden="true"><span data-conversation-anchor="input:hidden"></span></div><div inert><span data-conversation-anchor="input:inert"></span></div><div data-conversation-anchor="input:empty"></div><div data-conversation-anchor="input:visible"></div>';
    container.getBoundingClientRect = () => ({ top: 0 } as DOMRect);
    for (const element of container.querySelectorAll<HTMLElement>("[data-conversation-anchor]")) {
      element.getBoundingClientRect = () => ({ top: 10, bottom: 30, height: element.dataset.conversationAnchor === "input:empty" ? 0 : 20 } as DOMRect);
    }
    expect(captureConversationReadingAnchors(container).map((entry) => entry.anchor)).toEqual(["input:visible"]);
  });
});
