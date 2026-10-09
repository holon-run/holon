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

const runtimeConfig: RuntimeConfigState = { source: "fixture" };

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
  onCreateAgent: (agentId: string, template: string) => Promise<boolean>,
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
        runtimeConfig={runtimeConfig}
        runtimeConfigLoading={false}
        templateCatalog={templateCatalog}
        templateCatalogLoading={false}
        onSaveModel={vi.fn(async () => true)}
        onCreateAgent={onCreateAgent}
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
  const input = container.querySelector<HTMLInputElement>("form input");
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
    const onCreateAgent = vi.fn(async () => true);
    const container = await renderOnboardingModal(catalogState([]), onCreateAgent);

    await advanceToAgentStep(container);
    const finish = await submitOnboarding(container, "my-agent");

    expect(finish.disabled).toBe(false);
    const select = container.querySelector<HTMLSelectElement>("select")!;
    expect(select.value).toBe("holon-default");
    await act(async () => {
      select.form?.requestSubmit();
    });
    expect(onCreateAgent).toHaveBeenCalledWith("my-agent", "holon-default");
  });

  it("lists catalog entries besides the built-in default and creates with the selected template", async () => {
    const onCreateAgent = vi.fn(async () => true);
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
    expect(onCreateAgent).toHaveBeenCalledWith("my-agent", "software-developer");
  });
});
