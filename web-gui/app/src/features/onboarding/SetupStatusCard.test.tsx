// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import type { RuntimeConfigState } from "../../runtime/types";
import { SetupStatusCard, setupIsReady } from "./SetupStatusCard";

const ready: RuntimeConfigState = {
  source: "http",
  setup: { status: "ready", defaultModel: "local/model", provider: "local", actions: [] },
  reload: { requestedGeneration: 2, completedGeneration: 2, state: "completed" },
};
let root: Root | undefined;

afterEach(async () => {
  await act(async () => root?.unmount());
  vi.unstubAllGlobals();
});

async function render(config: RuntimeConfigState, saving = false) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  root = createRoot(container);
  const repair = vi.fn();
  const retry = vi.fn();
  const start = vi.fn();
  await act(async () => root!.render(
    <SetupStatusCard config={config} saving={saving} onRepair={repair} onRetry={retry} onStart={start} />,
  ));
  return { container, repair, retry, start };
}

describe("authoritative first-run setup", () => {
  it("shows readiness separately from untested connectivity", async () => {
    const { container, start } = await render(ready);
    expect(container.textContent).toContain("Configuration ready");
    expect(container.textContent).toContain("have not been tested");
    await act(async () => container.querySelector("button")!.click());
    expect(start).toHaveBeenCalledOnce();
  });

  it("directs missing credentials to the default provider, not optional services", async () => {
    const { container, repair } = await render({
      ...ready, setup: { ...ready.setup!, status: "blocked", actions: ["configure_model_credential"] },
    });
    expect(container.textContent).toContain("Setup is incomplete");
    await act(async () => container.querySelector("button")!.click());
    expect(repair).toHaveBeenCalledWith("configure_model_credential", "local");
  });

  it.each(["applying", "failed", "unknown", "stale"] as const)("never displays ready for %s configuration", async (state) => {
    const config = state === "unknown" ? { ...ready, error: "unavailable" }
      : { ...ready, reload: { ...ready.reload!, completedGeneration: 1, state: state === "stale" ? "completed" as const : state } };
    expect(setupIsReady(config)).toBe(false);
    const { container } = await render(config);
    expect(container.textContent).not.toContain("Configuration ready");
    expect(container.textContent).not.toContain("Set up your first Agent");
    expect(container.querySelector("button")!.textContent).toContain("Check setup again");
  });

  it("does not turn missing status into missing credentials", async () => {
    const { container, retry } = await render({ source: "http" });
    expect(container.textContent).toContain("could not be confirmed");
    expect(container.textContent).not.toContain("needs provider credentials");
    await act(async () => container.querySelector("button")!.click());
    expect(retry).toHaveBeenCalledOnce();
  });

  it("displays the applying stage while a save is pending", async () => {
    const { container } = await render(ready, true);
    expect(container.textContent).toContain("applying configuration");
    expect(container.querySelector("button")).toBeNull();
  });
});
