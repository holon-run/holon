/**
 * Match opaque identities as data, never interpolate source IDs into selectors.
 * The same anchor lookup is used for navigation and reading-position recovery.
 */
export interface ConversationNavigation {
  readonly requestId: number;
  readonly sourceRef?: string;
  readonly runtimeId?: string;
  /** Bare legacy sequences cannot be reinterpreted across runtime epochs. */
  readonly eventSeq?: number;
}

export interface ConversationLocation {
  readonly requestId: number;
  readonly kind: "loading" | "ready" | "source_only" | "error";
  readonly anchor?: string;
  readonly turnId?: string;
  readonly error?: string;
  readonly reason?: "unsupported" | "legacy" | "runtime";
}

export function findConversationAnchor(container: HTMLElement, anchor: string): HTMLElement | null {
  return Array.from(container.querySelectorAll<HTMLElement>("[data-conversation-anchor]"))
    .find((element) => element.dataset.conversationAnchor === anchor) ?? null;
}

export function revealConversationAnchor(container: HTMLElement, anchor: string): HTMLElement | null {
  const element = findConversationAnchor(container, anchor);
  if (!element) return null;
  for (let parent: HTMLElement | null = element; parent && parent !== container; parent = parent.parentElement) {
    if (parent instanceof HTMLDetailsElement) parent.open = true;
    // React controls execution disclosures. Wait for the expanded render rather
    // than overriding inert/aria-hidden and positioning invisible content.
    if (parent.getAttribute("aria-hidden") === "true" || parent.hasAttribute("inert")) return null;
  }
  return element;
}

/** Scroll only the conversation viewport, leaving surrounding panels alone. */
export function positionConversationAnchor(container: HTMLElement, element: HTMLElement): boolean {
  const bounds = element.getBoundingClientRect();
  if (bounds.height <= 0) return false;
  const viewport = container.getBoundingClientRect();
  container.scrollTop += bounds.top - viewport.top - Math.max(0, (container.clientHeight - bounds.height) / 2);
  element.tabIndex = -1;
  element.focus({ preventScroll: true });
  return true;
}

export interface ConversationReadingAnchor {
  readonly element: HTMLElement;
  readonly anchor?: string;
  readonly offset: number;
}

export function captureConversationReadingAnchors(container: HTMLElement): ConversationReadingAnchor[] {
  const viewport = container.getBoundingClientRect();
  const candidates = Array.from(container.querySelectorAll<HTMLElement>("[data-conversation-anchor]"))
    .filter((element) => {
      const bounds = element.getBoundingClientRect();
      return bounds.height > 0 && bounds.bottom > viewport.top
        && !element.closest('[aria-hidden="true"], [inert]');
    });
  // A turn ancestor stays put when earlier content inside that turn hydrates.
  // Prefer concrete content, retaining upcoming siblings for page eviction.
  const leaves = candidates.filter((element) => !candidates.some((other) => other !== element && element.contains(other)));
  const target = leaves.find((element) => element.dataset.locationTarget === "true"
    && element.getBoundingClientRect().top < viewport.top + container.clientHeight);
  const ordered = [...(target ? [target] : []), ...leaves.filter((element) => element !== target),
    ...candidates.filter((element) => !leaves.includes(element))];
  return ordered.slice(0, 8).map((element) => ({
    element, anchor: element.dataset.conversationAnchor, offset: element.getBoundingClientRect().top - viewport.top,
  }));
}

export function restoreConversationReadingAnchor(container: HTMLElement, anchors: readonly ConversationReadingAnchor[]): boolean {
  for (const entry of anchors) {
    const element = container.contains(entry.element) ? entry.element
      : entry.anchor ? findConversationAnchor(container, entry.anchor) : null;
    if (!element || element.getBoundingClientRect().height <= 0 || element.closest('[aria-hidden="true"], [inert]')) continue;
    container.scrollTop += element.getBoundingClientRect().top - container.getBoundingClientRect().top - entry.offset;
    return true;
  }
  return false;
}
