import { describe, expect, it } from "vitest";
import { modelMatches, modelSourceLabel, providerIsConnected, providerPresentation } from "./model-presentation";
import type { RuntimeModelOption, RuntimeProviderSummary } from "../runtime/types";
const model: RuntimeModelOption = { model: "qwen/coder", routeRef: "dashscope@plan/qwen/coder", provider: "dashscope", providerFamily: "dashscope", routeProvider: "dashscope-coding-plan", endpoint: "plan", displayName: "Qwen Coder", available: true, supportsImageInput: false, supportsImageGeneration: false, supportsReasoningEffort: false, reasoningEffortOptions: [] };
describe("model presentation", () => {
  it("searches aliases and model names across brands while preserving route identity", () => {
    expect(modelMatches(model, "百炼 coder")).toBe(true);
    expect(modelMatches(model, "阿里 coding")).toBe(true);
    expect(modelMatches(model, "dashscope@plan/qwen/coder")).toBe(true);
    expect(modelMatches(model, "deepseek")).toBe(false);
    expect(model.routeRef).toBe("dashscope@plan/qwen/coder");
    expect(modelSourceLabel(model)).toContain("Coding Plan · plan");
  });
  it("keeps unknown provider names intact, even object property names", () => {
    for (const id of ["private-coding-plan", "constructor", "__proto__"]) expect(providerPresentation(id).label).toBe(id);
  });
  it("distinguishes configured or used services from all no-auth builtins", () => {
    const provider = { id: "ollama", credentialKind: "none", credentialConfigured: false, configuredInConfig: false } as RuntimeProviderSummary;
    expect(providerIsConnected(provider, new Set())).toBe(false);
    expect(providerIsConnected(provider, new Set(["ollama"]))).toBe(true);
    expect(providerIsConnected({ ...provider, configuredInConfig: true }, new Set())).toBe(true);
    expect(providerIsConnected({ ...provider, credentialConfigured: true, credentialSource: "env" }, new Set())).toBe(true);
  });
});
