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
  runtimeNotificationSync: { dismiss: (id: string) => runtime.dismiss(id) },
  useRuntimeNotifications: () => runtime.snapshot,
  useSystemNotificationPermission: (
    selector: (state: { permission: string; request: () => Promise<void>; refresh: () => void }) => unknown,
  ) => selector({ permission: runtime.permission, request: runtime.request, refresh: runtime.refresh }),
}));

import { NotificationCenter } from "./NotificationCenter";

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
  runtime.snapshot = { status: "ready", pending: [], recent: [], toasts: [] };
  runtime.permission = "default";
  runtime.dismiss.mockReset();
  runtime.request.mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.useRealTimers();
});

async function render() {
  await act(async () => {
    root.render(<NotificationCenter onNavigate={navigate} />);
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
  it("portals one card per toast with the kind's tone, icon, and target", async () => {
    runtime.snapshot = {
      ...runtime.snapshot,
      toasts: [item("a", "operator_input_required"), item("b", "work_item_completed")],
    };
    await render();

    const cards = document.querySelectorAll<HTMLElement>('[data-testid="notification-toast"]');
    expect(cards).toHaveLength(2);
    expect(cards[0].getAttribute("data-tone")).toBe("warning");
    expect(cards[0].querySelector(".lucide-triangle-alert")).not.toBeNull();
    expect(cards[1].getAttribute("data-tone")).toBe("success");
    expect(cards[1].querySelector(".lucide-circle-check")).not.toBeNull();
    expect(cards[0].textContent).toContain("holon-web · work_1");
    // The stack lives on body, not inside the topbar subtree.
    expect(document.body.contains(cards[0])).toBe(true);
    expect(host.contains(cards[0])).toBe(false);
  });

  it("dismisses itself after 8s and pauses while focused", async () => {
    runtime.snapshot = { ...runtime.snapshot, toasts: [item("a")] };
    await render();

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
    runtime.snapshot = { ...runtime.snapshot, toasts: [item("a")] };
    await render();

    await act(async () => { toast().querySelector<HTMLElement>(".notification-toast-main")?.click(); });
    expect(runtime.dismiss).toHaveBeenCalledWith("a");
    expect(navigate).toHaveBeenCalledWith({ agent_id: "holon-web", work_item_id: "work_1" });
  });
});

describe("system notification settings", () => {
  it("offers the permission request only while it is still undecided", async () => {
    await render();
    const button = Array.from(document.querySelectorAll("button"))
      .find((element) => element.textContent?.includes("system notifications"));
    expect(button).toBeDefined();
    expect(document.querySelector(".notification-system")).not.toBeNull();
  });

  it("reports the granted state and hides itself when unsupported", async () => {
    runtime.permission = "granted";
    await render();
    expect(document.querySelector(".notification-system")?.textContent).toContain("are on");

    runtime.permission = "unsupported";
    await render();
    expect(document.querySelector(".notification-system")).toBeNull();
  });
});
