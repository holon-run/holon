import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createNotificationSync, emptyNotifications, NotificationHttpError, type NotificationSnapshot,
} from "./notification-sync";

function item(id: string, agent_id = "other", kind: "operator_first_brief" | "operator_input_required" = "operator_first_brief") {
  return { id, kind, created_at: "2026-11-22T04:00:00Z", target: { agent_id } };
}

type Lists = Pick<NotificationSnapshot, "pending" | "recent">;
const lists = (recent: Lists["recent"] = [], pending: Lists["pending"] = []): Lists => ({ recent, pending });
const advance = (ms = 500) => vi.advanceTimersByTimeAsync(ms);

function setup() {
  const page = { hidden: false };
  let state: NotificationSnapshot = emptyNotifications;
  const load = vi.fn(async (_signal: AbortSignal) => lists());
  const notify = vi.fn();
  const sync = createNotificationSync({
    load,
    isHidden: () => page.hidden,
    isViewing: (target) => target.agent_id === "current",
    publish: (snapshot) => { state = snapshot; },
    notify,
  });
  sync.start();
  return { load, notify, sync, page, snapshot: () => state };
}

describe("notification synchronization", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("keeps silent waits pending and toasts only new live brief IDs outside the active conversation", async () => {
    const { load, sync, snapshot } = setup();
    const old = item("old");
    const silent = item("silent", "other", "operator_input_required");
    load.mockResolvedValueOnce(lists([old], [silent]));
    expect(load).not.toHaveBeenCalled();
    sync.connected();
    await advance();
    expect(snapshot()).toMatchObject({ pending: [silent], recent: [old], toasts: [] });
    const reply = item("live");
    load.mockResolvedValue(lists([reply, item("current", "current"), old], [silent]));
    sync.hint();
    await advance();
    expect(snapshot().toasts).toEqual([reply]);
    sync.dismiss(reply.id);
    sync.hint();
    await advance();
    expect(snapshot()).toMatchObject({ pending: [silent], toasts: [] });
    load.mockResolvedValue(lists([reply, old]));
    sync.hint();
    await advance();
    expect(snapshot().pending).toEqual([]);
  });

  it("reads hidden hints without replay, then recovers on reconnect and health checks", async () => {
    const { load, notify, sync, page, snapshot } = setup();
    sync.connected();
    await advance();
    page.hidden = true;
    sync.visibilityChanged();
    sync.hint();
    sync.hint();
    await advance();
    // A hidden tab keeps reading; nothing to report yet, so it stays silent.
    expect(load).toHaveBeenCalledTimes(3);
    expect(notify).not.toHaveBeenCalled();
    expect(snapshot().toasts).toEqual([]);
    for (const [id, refresh] of [
      ["hidden", () => { page.hidden = false; sync.visibilityChanged(); }],
      ["gap", () => { sync.disconnected(); expect(snapshot().status).toBe("stale"); sync.connected(); }],
      ["health", sync.healthRefresh],
    ] as const) {
      load.mockResolvedValue(lists([item(id)]));
      refresh();
      await advance();
      expect(snapshot()).toMatchObject({ status: "ready", recent: [item(id)], toasts: [] });
    }
    expect(load).toHaveBeenCalledTimes(6);
    expect(notify).not.toHaveBeenCalled();
  });

  it("hands hidden-page arrivals to the OS instead of the toast stack", async () => {
    const { load, notify, sync, page, snapshot } = setup();
    sync.connected();
    await advance();
    page.hidden = true;
    sync.visibilityChanged();

    const brief = item("hidden-brief");
    load.mockResolvedValue(lists([brief, item("viewed", "current")]));
    sync.hint();
    await advance();
    expect(notify).toHaveBeenCalledTimes(1);
    expect(notify).toHaveBeenCalledWith([brief]);
    expect(snapshot()).toMatchObject({ recent: [brief, item("viewed", "current")], toasts: [] });

    // Coming back is a catchup read: already-delivered items never toast.
    page.hidden = false;
    sync.visibilityChanged();
    load.mockResolvedValue(lists([brief, item("caught-up")]));
    await advance();
    expect(snapshot().toasts).toEqual([]);
    expect(notify).toHaveBeenCalledTimes(1);
  });

  it("coalesces hints during a read and does not pop when that read spans visibility loss", async () => {
    const { load, sync, page, snapshot } = setup();
    sync.connected();
    await advance();
    let settle!: (data: Lists) => void;
    load.mockImplementationOnce(() => new Promise((resolve) => { settle = resolve; }));
    sync.hint();
    sync.hint();
    page.hidden = true;
    sync.visibilityChanged();
    sync.hint();
    page.hidden = false;
    sync.visibilityChanged();
    await advance();
    expect(load).toHaveBeenCalledTimes(2);
    settle(lists([item("in-flight")]));
    load.mockResolvedValue(lists([item("follow-up")]));
    await advance();
    expect(load).toHaveBeenCalledTimes(3);
    expect(snapshot()).toMatchObject({ recent: [item("follow-up")], toasts: [] });
  });

  it("aborts old-generation requests and ignores their late data", async () => {
    const { load, sync, snapshot } = setup();
    let settle!: (data: Lists) => void;
    load.mockImplementationOnce(() => new Promise((resolve) => { settle = resolve; }));
    sync.connected();
    const signal = load.mock.calls[0][0];
    sync.stop();
    expect(signal.aborted).toBe(true);
    expect(snapshot()).toEqual(emptyNotifications);
    sync.start();
    load.mockResolvedValue(lists([item("new-subject")]));
    sync.connected();
    settle(lists([item("old-subject")]));
    await advance();
    expect(snapshot().recent).toEqual([item("new-subject")]);
  });

  it.each([401, 403, 404])("clears both lists when a list returns %i", async (status) => {
    const { load, sync, snapshot } = setup();
    load.mockResolvedValueOnce(lists([item("old-recent")], [item("old-pending")]));
    sync.connected();
    await advance();
    load.mockRejectedValueOnce(new NotificationHttpError(status));
    sync.hint();
    await advance();
    expect(snapshot()).toEqual({ ...emptyNotifications, status: "unavailable" });
    sync.hint();
    await advance();
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("keeps stale data and backs off before a list-only recovery", async () => {
    const { load, sync, snapshot } = setup();
    load.mockResolvedValueOnce(lists([item("old")]));
    sync.connected();
    await advance();
    load.mockRejectedValueOnce(new Error("offline"));
    sync.hint();
    await advance(0);
    expect(snapshot()).toMatchObject({ status: "stale", recent: [item("old")] });
    load.mockResolvedValue(lists([item("recovered")]));
    sync.hint();
    await advance(200);
    expect(load).toHaveBeenCalledTimes(2);
    await advance(20_000);
    expect(snapshot()).toMatchObject({ status: "ready", recent: [item("recovered")], toasts: [] });
  });
});
