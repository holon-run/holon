// @vitest-environment happy-dom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  controller: { start: vi.fn(), stop: vi.fn() },
  factory: vi.fn((_deps: { onHint: () => void }) => mocks.controller),
  refreshRosterFromStreamHint: vi.fn(),
  state: {
    bootstrap: {
      connection: { error: undefined as string | undefined, source: "http" as const },
    },
  },
}));
vi.mock("./runtime-store", () => ({
  getRuntimeConnectionConfig: () => ({ mode: "local" as const }),
  refreshRosterFromStreamHint: mocks.refreshRosterFromStreamHint,
  useRuntimeStore: Object.assign(
    (select: (state: typeof mocks.state) => unknown) => select(mocks.state),
    {},
  ),
}));
vi.mock("./roster-hint-stream", () => ({ createRosterHintStream: mocks.factory }));

let useRosterHintStream: () => void;

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
let root: Root;
let container: HTMLDivElement;

function Probe(): null {
  useRosterHintStream();
  return null;
}

async function render(): Promise<void> {
  await act(async () => {
    root.render(createElement(Probe));
  });
}

beforeEach(async () => {
  vi.resetModules();
  vi.clearAllMocks();
  mocks.state.bootstrap.connection = { error: undefined, source: "http" };
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  ({ useRosterHintStream } = await import("./useRosterHintStream"));
});

afterEach(async () => {
  await act(async () => {
    root.unmount();
  });
  container.remove();
});

describe("useRosterHintStream", () => {
  it("builds one controller wired to the store hint refresh and starts it", async () => {
    await render();
    expect(mocks.factory).toHaveBeenCalledTimes(1);
    const deps = mocks.factory.mock.calls[0][0];
    expect(deps.onHint).toBe(mocks.refreshRosterFromStreamHint);
    expect(mocks.controller.start).toHaveBeenCalledTimes(1);
    expect(mocks.controller.stop).not.toHaveBeenCalled();
  });

  it("stops when the connection turns unhealthy and restarts on recovery", async () => {
    await render();
    expect(mocks.controller.start).toHaveBeenCalledTimes(1);
    mocks.state.bootstrap.connection = { error: "connect failed", source: "http" };
    await render();
    expect(mocks.controller.stop).toHaveBeenCalled();
    mocks.controller.start.mockClear();
    mocks.controller.stop.mockClear();
    mocks.state.bootstrap.connection = { error: undefined, source: "http" };
    await render();
    expect(mocks.controller.start).toHaveBeenCalledTimes(1);
  });

  it("never starts for fixture connections", async () => {
    mocks.state.bootstrap.connection = { error: undefined, source: "fixture" as "http" };
    await render();
    expect(mocks.controller.start).not.toHaveBeenCalled();
    expect(mocks.controller.stop).toHaveBeenCalled();
  });

  it("stops on unmount", async () => {
    await render();
    mocks.controller.stop.mockClear();
    await act(async () => {
      root.unmount();
    });
    expect(mocks.controller.stop).toHaveBeenCalledTimes(1);
  });
});
