// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import {
  BOOTSTRAP_STEPS,
  type AgentBootstrap,
  type AgentCreationProgress,
} from "../../runtime/agent-creation";
import { AgentCreationStatus } from "./AgentCreationFeedback";

function bootstrap(status: "pending" | "succeeded" | "failed"): AgentBootstrap {
  return {
    status: status === "succeeded" ? "ready" : status === "failed" ? "degraded" : "provisioning",
    ...Object.fromEntries(BOOTSTRAP_STEPS.map((key) => [key, {
      status, attempts: 1, updated_at: "2026-10-25T00:00:00Z",
      last_error: status === "failed" ? "private SQL /home/secret credential" : null,
    }])),
  } as AgentBootstrap;
}

function progress(
  status: AgentCreationProgress["status"],
  step: "pending" | "succeeded" | "failed",
): AgentCreationProgress {
  const value = bootstrap(step);
  return {
    creation: {
      identity: { agent_id: "new-agent", incarnation: 1, status: "active" },
      receipt: { receipt_id: "receipt-1", agent_id: "new-agent", stage: "provisioning", bootstrap: value },
    },
    status,
    observationRevision: 1,
    bootstrap: value,
  };
}

let root: Root | undefined;

afterEach(async () => {
  await act(async () => root?.unmount());
  root = undefined;
});

async function render(value: AgentCreationProgress, onRefresh = vi.fn(async () => {})) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  root = createRoot(container);
  await act(async () => root!.render(<AgentCreationStatus progress={value} onRefresh={onRefresh} />));
  const details = container.querySelector<HTMLDetailsElement>(".agent-creation-details")!;
  return { container, details, onRefresh };
}

describe("agent creation status card", () => {
  it("collapses completed initialization into an expandable detail", async () => {
    const { container, details } = await render(progress("ready", "succeeded"));
    expect(container.querySelector(".agent-creation-heading")!.textContent).toBe("Agent created");
    expect(container.querySelector(".agent-creation-badge")!.textContent).toBe("Initialization complete");
    expect(details.hasAttribute("open")).toBe(false);
    expect(details.querySelector("summary")!.textContent).toBe("Initialization details");
    expect(container.textContent).toContain("Initialization complete");
    expect(container.querySelector(".agent-creation-actions")).toBeNull();
  });

  it("keeps pending initialization expanded with per-step status", async () => {
    const { container, details } = await render(progress("initializing", "pending"));
    expect(container.querySelector(".agent-creation-badge")!.textContent).toBe("Initializing");
    expect(details.hasAttribute("open")).toBe(true);
    expect(container.querySelectorAll(".agent-creation-step")).toHaveLength(BOOTSTRAP_STEPS.length);
    expect(container.textContent).toContain("Pending");
  });

  it("keeps degraded initialization expanded and offers a refresh action", async () => {
    const { container, details, onRefresh } = await render(progress("degraded", "failed"));
    expect(container.querySelector(".agent-creation-badge")!.textContent).toBe("Initialization incomplete");
    expect(details.hasAttribute("open")).toBe(true);
    expect(container.querySelectorAll(".agent-creation-step--failed")).toHaveLength(BOOTSTRAP_STEPS.length);
    expect(container.textContent).toContain("Failed");
    await act(async () => container.querySelector<HTMLButtonElement>(".agent-creation-actions button")!.click());
    expect(onRefresh).toHaveBeenCalledOnce();
  });

  it("never renders raw bootstrap errors", async () => {
    const { container } = await render(progress("degraded", "failed"));
    expect(container.textContent).not.toContain("SQL");
    expect(container.textContent).not.toContain("/home/secret");
  });
});
