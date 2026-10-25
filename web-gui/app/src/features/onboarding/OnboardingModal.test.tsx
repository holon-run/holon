// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import "../../i18n";
import type {
  AgentTemplateCatalogEntry,
  AgentTemplateCatalogState,
  RuntimeConfigState,
  RuntimeModelCatalog,
} from "../../runtime/types";
import { OnboardingModal } from "./OnboardingModal";
import type { AgentCreationResult } from "../../runtime/agent-creation";

const accepted = async (): Promise<AgentCreationResult> => ({
  kind: "accepted",
  creation: {
    identity: { agent_id: "my-agent", incarnation: 1, status: "active" },
    receipt: { receipt_id: "receipt", agent_id: "my-agent", stage: "provisioning" },
  },
});

const modelCatalog: RuntimeModelCatalog = {
  source: "fixture",
  options: [
    {
      model: "test-model",
      routeRef: "provider/test-model",
      provider: "provider",
      providerFamily: "provider",
      endpoint: "https://model.example/v1",
      routeProvider: "provider",
      displayName: "Test Model",
      available: true,
      supportsImageInput: false,
      supportsImageGeneration: false,
      supportsReasoningEffort: false,
      reasoningEffortOptions: [],
    },
  ],
};

const runtimeConfig: RuntimeConfigState = {
  source: "fixture",
  setup: { status: "ready", defaultModel: "provider/test-model", provider: "provider", actions: [] },
};

function catalogState(catalog: AgentTemplateCatalogEntry[]): AgentTemplateCatalogState {
  return { source: "fixture", catalog, sources: [], diagnostics: [] };
}

const remoteEntry: AgentTemplateCatalogEntry = {
  catalogId: "remote:official:software-developer",
  template: "software-developer",
  templateId: "software-developer",
  source: "remote",
  name: "Software Developer",
  description: "Implementation-focused agent",
  includedSkills: [],
  sourceId: "official",
};

const roots: { root: Root; container: HTMLElement }[] = [];

async function renderOnboardingModal(
  templateCatalog: AgentTemplateCatalogState,
  onCreateAgent: (agentId: string, template: string, name?: string) => Promise<AgentCreationResult>,
  config: RuntimeConfigState = runtimeConfig,
  existingAgents: Array<{ id: string; name?: string }> = [],
): Promise<HTMLElement> {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  const root = createRoot(container);
  roots.push({ root, container });
  await act(async () => {
    root.render(
      <OnboardingModal
        modelCatalog={modelCatalog}
        modelCatalogLoading={false}
        runtimeConfig={config}
        runtimeConfigLoading={false}
        templateCatalog={templateCatalog}
        templateCatalogLoading={false}
        onSaveModel={vi.fn(async () => true)}
        onCreateAgent={onCreateAgent}
        onLookupAgent={async () => ({ kind: "unavailable" })}
        existingAgents={existingAgents}
        onClose={vi.fn()}
      />,
    );
  });
  return container;
}

function setValue(element: HTMLInputElement | HTMLSelectElement, value: string): void {
  const proto =
    element instanceof HTMLSelectElement
      ? window.HTMLSelectElement.prototype
      : window.HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  setter?.call(element, value);
  element.dispatchEvent(
    new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }),
  );
}

async function advanceToAgentStep(container: HTMLElement): Promise<void> {
  await act(async () => {
    container.querySelector<HTMLButtonElement>(".model-select-trigger")!.click();
  });
  await act(async () => {
    container.querySelector<HTMLButtonElement>("button[data-model-choice]")!.click();
  });
  const next = container.querySelector<HTMLButtonElement>(".modal-actions button.bg-accent");
  expect(next).not.toBeNull();
  await act(async () => {
    next!.click();
  });
}

async function submitOnboarding(
  container: HTMLElement,
  agentId: string,
  template?: string,
): Promise<HTMLButtonElement> {
  const select = container.querySelector<HTMLSelectElement>("select");
  expect(select).not.toBeNull();
  if (template != null) {
    await act(async () => {
      setValue(select!, template);
    });
  }
  const input = container.querySelector<HTMLInputElement>("details input");
  expect(input).not.toBeNull();
  await act(async () => {
    setValue(input!, agentId);
  });
  const finish = container.querySelector<HTMLButtonElement>('button[type="submit"]');
  expect(finish).not.toBeNull();
  return finish!;
}

afterEach(async () => {
  while (roots.length > 0) {
    const { root } = roots.pop()!;
    await act(async () => {
      root.unmount();
    });
  }
  vi.unstubAllGlobals();
});

describe("OnboardingModal template selection", () => {
  it("finishes with the built-in holon-default template when the catalog is empty", async () => {
    const onCreateAgent = vi.fn(accepted);
    const container = await renderOnboardingModal(catalogState([]), onCreateAgent);

    await advanceToAgentStep(container);
    const finish = await submitOnboarding(container, "my-agent");

    expect(finish.disabled).toBe(false);
    const select = container.querySelector<HTMLSelectElement>("select")!;
    expect(select.value).toBe("holon-default");
    await act(async () => {
      select.form?.requestSubmit();
    });
    expect(onCreateAgent).toHaveBeenCalledWith("my-agent", "holon-default", "Holon Bot");
  });

  it("lists catalog entries besides the built-in default and creates with the selected template", async () => {
    const onCreateAgent = vi.fn(accepted);
    const container = await renderOnboardingModal(catalogState([remoteEntry]), onCreateAgent);

    await advanceToAgentStep(container);
    const select = container.querySelector<HTMLSelectElement>("select")!;
    expect([...select.options].map((option) => option.value)).toEqual([
      "holon-default",
      "software-developer",
    ]);
    const finish = await submitOnboarding(container, "my-agent", "software-developer");

    expect(finish.disabled).toBe(false);
    await act(async () => {
      select.form?.requestSubmit();
    });
    expect(onCreateAgent).toHaveBeenCalledWith("my-agent", "software-developer", "Holon Bot");
  });

  it("requires an explicit model choice and prefills a separate editable name and stable ID", async () => {
    const create = vi.fn(accepted);
    const container = await renderOnboardingModal(catalogState([]), create);
    expect(container.querySelector<HTMLButtonElement>(".modal-actions button.bg-accent")!.disabled).toBe(true);
    await advanceToAgentStep(container);
    const inputs = container.querySelectorAll<HTMLInputElement>("form input");
    expect(inputs[0].value).toBe("Holon Bot");
    expect(inputs[1].value).toBe("holon-bot");
    await act(async () => setValue(inputs[0], "My Assistant"));
    expect(inputs[1].value).toBe("holon-bot");
    await act(async () => container.querySelector("form")!.requestSubmit());
    expect(create).toHaveBeenCalledWith("holon-bot", "holon-default", "My Assistant");
  });

  it("does not create an Agent while authoritative setup is blocked", async () => {
    const create = vi.fn(accepted);
    const container = await renderOnboardingModal(catalogState([]), create, {
      ...runtimeConfig, setup: { ...runtimeConfig.setup!, status: "blocked", actions: ["configure_model_credential"] },
    });
    await advanceToAgentStep(container);
    expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')!.disabled).toBe(true);
    await act(async () => container.querySelector("form")!.requestSubmit());
    expect(create).not.toHaveBeenCalled();
  });

  it("keeps existing ID collisions explicit and never overwrites the Agent", async () => {
    const create = vi.fn(accepted);
    const container = await renderOnboardingModal(catalogState([]), create, runtimeConfig, [{ id: "holon-bot" }]);
    await advanceToAgentStep(container);
    await act(async () => container.querySelector("form")!.requestSubmit());
    expect(container.querySelector('[role="alert"]')!.textContent).toContain("already in use");
    expect(create).not.toHaveBeenCalled();
  });

  it("does not retry an ambiguous create outcome", async () => {
    const create = vi.fn(async (): Promise<AgentCreationResult> => ({ kind: "uncertain" }));
    const container = await renderOnboardingModal(catalogState([]), create);
    await advanceToAgentStep(container);
    await act(async () => container.querySelector("form")!.requestSubmit());
    await act(async () => container.querySelector("form")!.requestSubmit());
    expect(create).toHaveBeenCalledTimes(1);
    expect(container.querySelector('[role="alert"]')!.textContent).toContain("Check existing Agents");
  });

  it("preserves a rejected request's form inputs and shows its operation-specific reason", async () => {
    const create = vi.fn(async (): Promise<AgentCreationResult> => ({
      kind: "rejected", problem: "deletion_incomplete", status: 409,
    }));
    const container = await renderOnboardingModal(catalogState([]), create);
    await advanceToAgentStep(container);
    const finish = await submitOnboarding(container, "reused-agent");
    await act(async () => container.querySelector("form")!.requestSubmit());
    expect(container.querySelector('[role="alert"]')!.textContent).toContain("Deletion is incomplete");
    expect(container.querySelector('[role="alert"]')!.textContent).toContain("HTTP 409");
    expect(finish.disabled).toBe(false);
    expect(container.querySelector<HTMLInputElement>('input[value="reused-agent"]')?.disabled).toBe(false);
    expect(container.querySelector<HTMLSelectElement>("select")!.value).toBe("holon-default");
    expect(create).toHaveBeenCalledTimes(1);
  });
});
