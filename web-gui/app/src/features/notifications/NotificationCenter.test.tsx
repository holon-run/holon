// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import type { NotificationSnapshot, RuntimeNotification } from "../../runtime/notification-sync";

const runtime = vi.hoisted(() => ({
  snapshot: { status: "ready", pending: [], recent: [], toasts: [] } as NotificationSnapshot,
  permission: "default" as string,
  dismiss: vi.fn(),
  request: vi.fn(async () => {}),
  refresh: vi.fn(),
}));

vi.mock("../../runtime/runtime-notifications", () => ({
  runtimeNotificationSync: {
    dismiss: (id: string) => runtime.dismiss(id),
  },
  useRuntimeNotifications: () => runtime.snapshot,
  useSystemNotificationPermission: (
    selector: (state: { permission: string; request: () => Promise<void>; refresh: () => void }) => unknown,
  ) => selector({ permission: runtime.permission, request: runtime.request, refresh: runtime.refresh }),
}));

import { NotificationCenter, SystemNotificationSettingsCard } from "./NotificationCenter";

const item = (id: string, kind: RuntimeNotification["kind"] = "operator_first_brief"): RuntimeNotification => ({
  id,
  kind,
  created_at: "2026-11-25T23:29:00Z",
  target: { agent_id: "holon-web", work_item_id: "work_1" },
});

let root: Root;
let host: HTMLDivElement;
const navigate = vi.fn();

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.useFakeTimers();
  window.localStorage.clear();
  runtime.snapshot = { status: "ready", pending: [], recent: [], toasts: [] };
  runtime.permission = "default";
  runtime.dismiss.mockReset();
  runtime.request.mockReset();
  navigate.mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.useRealTimers();
});

async function renderCenter() {
  await act(async () => {
    root.render(<NotificationCenter onNavigate={navigate} />);
  });
}

async function renderSettingsCard() {
  await act(async () => {
    root.render(<SystemNotificationSettingsCard />);
  });
}

function toast(): HTMLElement {
  const element = document.querySelector<HTMLElement>('[data-testid="notification-toast"]');
  if (!element) throw new Error("toast not rendered");
  return element;
}

async function advance(ms: number) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
}

describe("notification toasts", () => {
  it("portals one card per toast without rendering a topbar bell button", async () => {
    runtime.permission = "granted";
    runtime.snapshot = {
      ...runtime.snapshot,
      toasts: [item("a", "operator_input_required"), item("b", "work_item_completed")],
    };
    await renderCenter();

    expect(host.querySelector(".notification-trigger")).toBeNull();
    const cards = document.querySelectorAll<HTMLElement>('[data-testid="notification-toast"]');
    expect(cards).toHaveLength(2);
    expect(cards[0].getAttribute("data-tone")).toBe("warning");
    expect(cards[0].querySelector(".lucide-triangle-alert")).not.toBeNull();
    expect(cards[1].getAttribute("data-tone")).toBe("success");
    expect(cards[1].querySelector(".lucide-circle-check")).not.toBeNull();
    expect(cards[0].textContent).toContain("holon-web · work_1");
    expect(document.body.contains(cards[0])).toBe(true);
    expect(host.contains(cards[0])).toBe(false);
  });

  it("dismisses itself after 8s and pauses while focused", async () => {
    runtime.permission = "granted";
    runtime.snapshot = { ...runtime.snapshot, toasts: [item("a")] };
    await renderCenter();

    await advance(7_000);
    expect(runtime.dismiss).not.toHaveBeenCalled();

    await act(async () => { toast().querySelector<HTMLElement>(".notification-toast-dismiss")?.focus(); });
    await advance(30_000);
    expect(runtime.dismiss).not.toHaveBeenCalled();

    await act(async () => { (document.activeElement as HTMLElement | null)?.blur(); });
    await advance(7_900);
    expect(runtime.dismiss).not.toHaveBeenCalled();
    await advance(200);
    expect(runtime.dismiss).toHaveBeenCalledWith("a");
  });

  it("closes the card from its own button and follows the target on body click", async () => {
    runtime.permission = "granted";
    runtime.snapshot = { ...runtime.snapshot, toasts: [item("a")] };
    await renderCenter();

    await act(async () => { toast().querySelector<HTMLElement>(".notification-toast-main")?.click(); });
    expect(runtime.dismiss).toHaveBeenCalledWith("a");
    expect(navigate).toHaveBeenCalledWith({ agent_id: "holon-web", work_item_id: "work_1" });
  });
});

describe("proactive system notification prompt and Settings card", () => {
  it("proactively prompts once when permission is default and remembers Not now", async () => {
    await renderCenter();

    const prompt = document.querySelector<HTMLElement>('[data-testid="system-notification-prompt"]');
    expect(prompt).not.toBeNull();
    expect(prompt?.textContent).toContain("Enable system notifications?");

    const buttons = Array.from(prompt?.querySelectorAll("button") ?? []);
    const notNowButton = buttons.find((b) => b.textContent?.includes("Not now"));
    expect(notNowButton).toBeDefined();

    await act(async () => { notNowButton?.click(); });
    expect(document.querySelector('[data-testid="system-notification-prompt"]')).toBeNull();
    expect(window.localStorage.getItem("holon.webGui.systemNotificationPromptDismissed.v1")).toBe("1");
  });

  it("triggers browser permission request on Enable click", async () => {
    await renderCenter();

    const prompt = document.querySelector<HTMLElement>('[data-testid="system-notification-prompt"]');
    const enableButton = Array.from(prompt?.querySelectorAll("button") ?? [])
      .find((b) => b.textContent?.includes("Enable system notifications"));
    expect(enableButton).toBeDefined();

    await act(async () => { enableButton?.click(); });
    expect(runtime.request).toHaveBeenCalledTimes(1);
    expect(document.querySelector('[data-testid="system-notification-prompt"]')).toBeNull();
  });

  it("renders system notification controls in the Settings card across permission states", async () => {
    runtime.permission = "default";
    await renderSettingsCard();
    expect(host.querySelector(".notification-system")?.textContent).toContain("Enable system notifications");

    runtime.permission = "granted";
    await renderSettingsCard();
    expect(host.querySelector(".notification-system")?.textContent).toContain("are on");

    runtime.permission = "unsupported";
    await renderSettingsCard();
    expect(host.querySelector(".notification-system")).toBeNull();
  });
});
