// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LoginPage } from "./LoginPage";

vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("../../runtime/conversation-cache-lifecycle", () => ({
  clearConversationCaches: vi.fn(async () => {}),
}));
vi.mock("../../runtime/runtime-store", () => ({
  clearStoredRuntimeConnectionToken: vi.fn(),
}));

let root: Root | undefined;
let container: HTMLDivElement;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  vi.spyOn(window.location, "replace").mockImplementation(() => {});
  vi.spyOn(window.location, "reload").mockImplementation(() => {});
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = undefined;
  window.history.replaceState(null, "", "/");
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function renderLogin(mode: "static" | "oidc", path: string) {
  window.history.replaceState(null, "", path);
  vi.stubGlobal("fetch", vi.fn(async (url: string) =>
    Response.json(url === "/api/auth/method" ? { mode } : {}),
  ));
  root = createRoot(container);
  await act(async () => root!.render(<LoginPage />));
}

describe("login completion destinations", () => {
  const cases = [
    { name: "local path", requested: "/agents/example?tab=briefs#latest", expected: "/agents/example?tab=briefs#latest" },
    { name: "normalized local path", requested: "/agents/../settings", expected: "/settings" },
    { name: "different origin", requested: "https://other.example/settings", expected: "/" },
    { name: "protocol-relative origin", requested: "//other.example/settings", expected: "/" },
    { name: "backslash-normalized origin", requested: "/\\other.example/settings", expected: "/" },
    { name: "tab-normalized origin", requested: "/\t/other.example/settings", expected: "/" },
    { name: "malformed parsed URL", requested: "/\\[", expected: "/" },
    { name: "invalid URL", requested: "http://[", expected: "/" },
  ];

  for (const { name, requested, expected } of cases) {
    it(`uses a same-origin destination after token exchange: ${name}`, async () => {
      await renderLogin("static", `/login?return_to=${encodeURIComponent(requested)}`);
      await act(async () => {
        container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
      });
      expect(window.location.replace).toHaveBeenCalledWith(new URL(expected, window.location.origin).href);
    });

    it(`uses a same-origin destination after pairing: ${name}`, async () => {
      await renderLogin("static", `/login?return_to=${encodeURIComponent(requested)}#pair=test-ticket`);
      expect(window.location.replace).toHaveBeenCalledWith(new URL(expected, window.location.origin).href);
      expect(window.location.hash).toBe("");
    });

    it(`passes the validated destination to OIDC: ${name}`, async () => {
      await renderLogin("oidc", `/login?return_to=${encodeURIComponent(requested)}`);
      expect(window.location.replace).toHaveBeenCalledWith(
        `/api/auth/oidc/start?return_to=${encodeURIComponent(expected)}`,
      );
    });
  }

  it("defaults the login route to the same-origin root", async () => {
    await renderLogin("static", "/login");
    await act(async () => {
      container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    expect(window.location.replace).toHaveBeenCalledWith(new URL("/", window.location.origin).href);
  });

  it("preserves and reloads the current document outside the login route", async () => {
    await renderLogin("static", "/agents/example?tab=briefs#latest");
    const current = window.location.href;
    await act(async () => {
      container.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
    expect(window.location.href).toBe(current);
    expect(window.location.reload).toHaveBeenCalledOnce();
    expect(window.location.replace).not.toHaveBeenCalled();
  });
});
