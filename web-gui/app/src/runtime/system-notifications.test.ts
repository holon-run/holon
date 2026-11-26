import { describe, expect, it, vi } from "vitest";

import type { RuntimeNotification } from "./notification-sync";
import {
  createSystemNotifications,
  type SystemNotificationDeps,
  type SystemNotificationPermission,
} from "./system-notifications";

const item = (id: string): RuntimeNotification => ({
  id,
  kind: "operator_first_brief",
  created_at: "2026-11-25T00:00:00Z",
  target: { agent_id: "other" },
});

function setup(overrides: Partial<SystemNotificationDeps> = {}) {
  const show = vi.fn();
  const deps: SystemNotificationDeps = {
    permission: () => "granted" as SystemNotificationPermission,
    requestPermission: async () => "granted" as SystemNotificationPermission,
    claim: async (_id, deliver) => { deliver(); return true; },
    show,
    ...overrides,
  };
  return { show, notifications: createSystemNotifications(deps) };
}

describe("system notifications", () => {
  it("stays silent without the granted permission", async () => {
    const claim = vi.fn(async (_id: string, deliver: () => void) => { deliver(); return true; });
    const { show, notifications } = setup({ permission: () => "default", claim });
    await notifications.deliver([item("n1")]);
    expect(claim).not.toHaveBeenCalled();
    expect(show).not.toHaveBeenCalled();
  });

  it("delivers each item once, and only from the tab that wins the claim", async () => {
    const claimed: string[] = [];
    const claim = vi.fn(async (id: string, deliver: () => void) => {
      claimed.push(id);
      if (id === "taken") return false;
      deliver();
      return true;
    });
    const { show, notifications } = setup({ claim });
    const first = item("first");

    await notifications.deliver([first, item("taken"), first]);

    expect(claimed).toEqual(["first", "taken"]);
    expect(show).toHaveBeenCalledTimes(1);
    expect(show).toHaveBeenCalledWith(first);

    await notifications.deliver([first]);
    expect(show).toHaveBeenCalledTimes(1);
  });
});
