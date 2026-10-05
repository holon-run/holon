// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import type { RuntimeConnection } from "../../runtime/types";
import { TailscaleServeCard } from "./TailscaleServeCard";
import { useServeStatus, type ServeState } from "./useServeStatus";

const connection: RuntimeConnection = {
  mode: "remote", source: "http", summary: "remote", baseUrl: "https://runtime.example",
};

afterEach(() => vi.unstubAllGlobals());

describe("Serve request errors", () => {
  it.each([
    ['{"error":"Tailscale Serve conflicts with existing configuration"}', "Tailscale Serve conflicts with existing configuration"],
    ['{"error":""}', "Serve status request failed"],
    ['{"error":42}', "Serve status request failed"],
    ['null', "Serve status request failed"],
    ['{"message":"ignored","token":"private-value"}', "Serve status request failed"],
    ["<html>private-value</html>", "Serve status request failed"],
    ["", "Serve status request failed"],
  ])("reads a failed response safely: %s", async (body, expected) => {
    const responses: Response[] = [];
    const fetchMock = vi.fn(() => {
      const response = new Response(body, { status: 409 });
      responses.push(response);
      return Promise.resolve(response);
    });
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    let state: ServeState | undefined;
    function Probe() {
      state = useServeStatus(connection);
      return null;
    }
    const root = createRoot(document.createElement("div"));
    try {
      await act(async () => { root.render(<Probe />); });
      expect(state?.error).toBe(expected);
      for (const action of ["enable", "disable"] as const) {
        await act(async () => { await state!.request(action); });
        expect(state?.error).toBe(expected);
        expect(state?.busy).toBe(false);
        expect(state?.status).toBeUndefined();
        expect(fetchMock.mock.calls.length).toBe(action === "enable" ? 2 : 3);
      }
      expect(responses.every((response) => response.bodyUsed)).toBe(true);
      expect(state?.error).not.toContain("private-value");
    } finally {
      await act(async () => root.unmount());
    }
  });

  it("renders the specific error as escaped text", () => {
    const error = 'Serve conflict <img src=x onerror="alert(1)">';
    const markup = renderToStaticMarkup(<TailscaleServeCard connection={connection} serve={{
      status: undefined, error, busy: false, revision: 0, key: "",
      request: async () => undefined, sequence: { current: 0 },
    }} />);
    expect(markup).toContain("Serve conflict &lt;img");
    expect(markup).not.toContain("<img");
    expect(markup).toContain('role="alert"');
  });
});
