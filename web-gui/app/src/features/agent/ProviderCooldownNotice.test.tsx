// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import i18n from "i18next";
import "../../i18n";
import type { ProviderCooldownWait } from "../../runtime/types";
import { ProviderCooldownNotice } from "./ProviderCooldownNotice";

const wait: ProviderCooldownWait = {
  turn_id: "turn-current",
  turn_index: 4,
  attempt_id: "attempt-current",
  started_at: "2026-10-27T10:00:00Z",
  not_before: "2026-10-27T10:10:00Z",
  cause: {
    failure_kind: "rate_limited",
    http_status: 429,
    occurred_at: "2026-10-27T09:59:59Z",
    retry_after: { source: "server_header", seconds: 600 },
    incident_id: "incident-safe",
  },
};
let host: HTMLDivElement;
let root: Root;

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  await i18n.changeLanguage("en");
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

describe("provider cooldown notice", () => {
  it("shows the real cause, earliest retry, and working stop/interjection controls", () => {
    const onStop = vi.fn();
    const onInterject = vi.fn();
    act(() => root.render(<ProviderCooldownNotice wait={wait} onStop={onStop} onInterject={onInterject} />));
    expect(host.querySelector('[role="status"]')).not.toBeNull();
    expect(host.textContent).toContain("shared provider cooldown");
    expect(host.textContent).toContain("HTTP 429");
    expect(host.textContent).toContain("not guaranteed recovery");
    expect(host.textContent).toContain("Retry-After header");
    expect(host.textContent).toContain("incident-safe");
    expect(host.querySelector("time")?.dateTime).toBe(wait.not_before);
    const buttons = host.querySelectorAll("button");
    act(() => { buttons[0].click(); buttons[1].click(); });
    expect(onStop).toHaveBeenCalledOnce();
    expect(onInterject).toHaveBeenCalledOnce();
  });

  it("updates a deadline extension and removes the notice when authoritative state clears", () => {
    const props = { onStop: vi.fn(), onInterject: vi.fn() };
    act(() => root.render(<ProviderCooldownNotice {...props} wait={wait} />));
    act(() => root.render(<ProviderCooldownNotice {...props} wait={{ ...wait, not_before: "2026-10-27T11:00:00Z" }} stopping />));
    expect(host.querySelector("time")?.dateTime).toBe("2026-10-27T11:00:00Z");
    expect(host.querySelector("button")?.disabled).toBe(true);
    act(() => root.render(<ProviderCooldownNotice {...props} wait={undefined} />));
    expect(host.innerHTML).toBe("");
  });

  it("degrades honestly without inventing an error or Retry-After origin", async () => {
    await act(async () => { await i18n.changeLanguage("zh-CN"); });
    act(() => root.render(<ProviderCooldownNotice wait={{ ...wait, cause: {} }} onStop={vi.fn()} onInterject={vi.fn()} />));
    expect(host.textContent).toContain("无法获取更详细的冷却原因");
    expect(host.textContent).toContain("不保证服务恢复");
    expect(host.textContent).not.toContain("HTTP");
    expect(host.textContent).not.toContain("Retry-After");
  });
});
