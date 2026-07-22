import { describe, expect, it } from "vitest";
import { compactModelRouteDisplay } from "./model-route-ref";

describe("compactModelRouteDisplay", () => {
  it("omits only an exact default endpoint", () => {
    expect(compactModelRouteDisplay("openai@default/gpt-5.6")).toBe("openai/gpt-5.6");
    expect(compactModelRouteDisplay("volcengine@plan/glm-5.2")).toBe("volcengine@plan/glm-5.2");
    expect(compactModelRouteDisplay("dashscope@token-plan/qwen3.7-max")).toBe("dashscope@token-plan/qwen3.7-max");
  });

  it("preserves model remainders containing slashes", () => {
    expect(compactModelRouteDisplay("openrouter@default/anthropic/claude-3.5-sonnet")).toBe(
      "openrouter/anthropic/claude-3.5-sonnet",
    );
  });

  it("leaves legacy and malformed values unchanged", () => {
    expect(compactModelRouteDisplay("openai/gpt-5.6")).toBe("openai/gpt-5.6");
    expect(compactModelRouteDisplay("runtime default")).toBe("runtime default");
    expect(compactModelRouteDisplay("openai@default/")).toBe("openai@default/");
  });
});
