// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "../i18n";
import i18n from "../i18n/config";
import en from "../i18n/resources/en";
import zhCN from "../i18n/resources/zh-CN";
import type { RuntimeNotification } from "../runtime/notification-sync";

const sync = vi.hoisted(() => ({
  ignorePending: vi.fn(),
}));

vi.mock("../runtime/runtime-notifications", () => ({
  runtimeNotificationSync: {
    ignorePending: (id: string) => sync.ignorePending(id),
  },
}));

import { HeaderPendingReminder } from "./HeaderPendingReminder";

function reminder(overrides: Partial<RuntimeNotification> = {}): RuntimeNotification {
  return {
    id: "n1",
    kind: "operator_first_brief",
    created_at: "2026-11-27T00:00:00Z",
    target: { agent_id: "holon-web" },
    ...overrides,
  };
}

let root: Root;
let host: HTMLDivElement;
const onOpenWorkItem = vi.fn();

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  sync.ignorePending.mockReset();
  onOpenWorkItem.mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

async function renderReminder(reminders: RuntimeNotification[]) {
  await act(async () => {
    root.render(<HeaderPendingReminder reminders={reminders} onOpenWorkItem={onOpenWorkItem} />);
  });
}

describe("HeaderPendingReminder", () => {
  it("renders nothing when there are no pending reminders", async () => {
    await renderReminder([]);
    expect(host.querySelector(".header-pending-reminder")).toBeNull();
  });

  it("keeps Ignore available when the first reminder has no work item", async () => {
    await renderReminder([reminder({ id: "no-wi" })]);

    expect(host.querySelector(".header-pending-reminder")).not.toBeNull();
    expect(host.querySelector(".header-pending-open")).toBeNull();
    const ignore = host.querySelector<HTMLButtonElement>(".header-pending-ignore");
    expect(ignore).not.toBeNull();

    act(() => ignore!.click());
    expect(sync.ignorePending).toHaveBeenCalledWith("no-wi");
    expect(onOpenWorkItem).not.toHaveBeenCalled();
  });

  it("shows Open and Ignore when the first reminder targets a work item", async () => {
    await renderReminder([
      reminder({ id: "with-wi", target: { agent_id: "holon-web", work_item_id: "work_42" } }),
      reminder({ id: "second", target: { agent_id: "holon-web" } }),
    ]);

    const open = host.querySelector<HTMLButtonElement>(".header-pending-open");
    const ignore = host.querySelector<HTMLButtonElement>(".header-pending-ignore");
    expect(open).not.toBeNull();
    expect(ignore).not.toBeNull();

    act(() => open!.click());
    expect(onOpenWorkItem).toHaveBeenCalledWith("work_42");

    act(() => ignore!.click());
    expect(sync.ignorePending).toHaveBeenCalledTimes(2);
    expect(sync.ignorePending).toHaveBeenNthCalledWith(1, "with-wi");
    expect(sync.ignorePending).toHaveBeenNthCalledWith(2, "second");
  });

  it("resolves the Ignore title/label through i18n instead of the raw keys", async () => {
    await renderReminder([reminder()]);

    const ignore = host.querySelector<HTMLButtonElement>(".header-pending-ignore")!;
    expect(ignore.getAttribute("title")).toBe(i18n.t("notifications.ignore"));
    expect(ignore.getAttribute("aria-label")).toBe(i18n.t("notifications.ignoreItem"));
    expect(ignore.getAttribute("title")).not.toBe("notifications.ignore");
    expect(ignore.getAttribute("aria-label")).not.toBe("notifications.ignoreItem");
  });
});

describe("common.remove i18n key", () => {
  it("is present in both en and zh-CN resources with localized values", () => {
    expect(en.common.remove).toBe("Remove");
    expect(zhCN.common.remove).toBe("移除");
  });
});

