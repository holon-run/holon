import "../i18n";
import { describe, expect, it } from "vitest";
import { hydrateInputActivity, inputInspectorActivity, inputPresentation } from "./conversation-input";

describe("conversation input presentation", () => {
  it("summarizes known JSON bodies and keeps plain text unchanged", () => {
    expect(inputPresentation(JSON.stringify({ type: "json", value: { summary: "Tests completed", status: "success" } })).summary).toBe("Tests completed");
    expect(inputPresentation({ type: "json", value: { task: { kind: "command_task", status: "completed", summary: "Build complete" } } }).summary).toBe("Build complete");
    expect(inputPresentation({ type: "text", text: "Scheduled review\nsecond line" })).toEqual({ summary: "Scheduled review", text: "Scheduled review\nsecond line" });
  });
  it("does not expose a truncated JSON prefix as a friendly summary", () => {
    const preview = JSON.stringify({ type: "text", text: 'wake hint: {"activationId":"secret-id",...' });
    expect(inputPresentation(preview).summary).not.toContain("activationId");
    expect(inputPresentation(preview).text).toContain("activationId");
  });
  it("recovers a wake's original body from metadata rather than its truncated text", () => {
    const activity = inputInspectorActivity({ message_id: "m", preview: 'wake hint: {"act...' }, "system");
    const message = { id: "m", body: { type: "text", text: 'wake hint: {"act...' }, metadata: { wake_hint: {
      body: { type: "json", value: { summary: "Pull request merged", payload: "complete event content" } },
    } } };
    const hydrated = hydrateInputActivity(activity, message);
    expect(hydrated.body).toBe("Pull request merged");
    expect(hydrated.detail?.text).toContain("complete event content");
    expect(hydrated.rawEvent).toBe(message);
    expect(hydrated.messageId).toBe("m");
  });
});

  it("replaces a truncated MessageBody preview with complete plain text", () => {
    const text = "Task result\n\n" + "Details ".repeat(1000) + "END OF MESSAGE";
    const preview = JSON.stringify({ type: "text", text }).slice(0, 100);
    const activity = inputInspectorActivity({ message_id: "long-result", preview });
    expect(activity.body).toBe("Structured event");
    const full = hydrateInputActivity(activity, { id: "long-result", body: { type: "text", text } });
    expect(full.body).toBe("Task result");
    expect(full.detail?.text).toBe(text);
    expect(full.detail?.text).not.toContain('"type":"text"');
  });
