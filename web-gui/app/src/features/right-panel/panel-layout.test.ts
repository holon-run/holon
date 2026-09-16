import { describe, expect, it } from "vitest";
import { NAV_WIDTH, panelLayout } from "./panel-layout";

describe("adaptive inspector layout", () => {
  it("preserves conversation space before allocating a wide inspector", () => {
    for (const viewport of [390, 768, 1024, 1280, 1440, 1600]) {
      for (const requested of [320, 380, 900, 2000]) {
        const layout = panelLayout(viewport, true, false, false, requested);
        if (!layout.full) expect(viewport - (layout.navCollapsed ? 72 : NAV_WIDTH) - layout.width).toBeGreaterThanOrEqual(640);
      }
    }
  });
  it("temporarily compacts navigation without changing user preference", () => {
    expect(panelLayout(1100, true, false, false, 380).navCollapsed).toBe(true);
    expect(panelLayout(1100, false, false, false, 380).navCollapsed).toBe(false);
    expect(panelLayout(1600, true, false, false, 380).navCollapsed).toBe(false);
    expect(panelLayout(1600, false, false, true, 380).navCollapsed).toBe(true);
  });
  it("uses a complete view at narrow widths, and honors explicit maximization", () => {
    expect(panelLayout(1024, true, false, false, 380).full).toBe(true);
    expect(panelLayout(1600, true, true, false, 380).full).toBe(true);
    expect(panelLayout(1600, true, false, false, 380).width).toBe(380);
  });
});
