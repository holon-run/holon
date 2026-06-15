import { describe, expect, it } from "vitest";

import { projectModelOptions } from "./client";

describe("projectModelOptions", () => {
  it("detects reasoning effort support from runtime available model capabilities", () => {
    const options = projectModelOptions({
      available_models: [
        {
          model: "openai-codex/gpt-5.5",
          provider: "openai-codex",
          capabilities: {
            reasoning_summaries: true,
          },
        },
      ],
    });

    expect(options).toEqual([
      expect.objectContaining({
        model: "openai-codex/gpt-5.5",
        provider: "openai-codex",
        supportsReasoningEffort: true,
      }),
    ]);
  });
});
