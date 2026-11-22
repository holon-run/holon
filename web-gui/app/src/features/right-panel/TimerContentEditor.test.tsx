// @vitest-environment happy-dom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { TimerContentEditor, timerDisplayName } from "./TimerContentEditor";

describe("timer content editor", () => {
  const labels = {
    title: "Title",
    instruction: "Instruction",
    hint: "Clear a field to restore its default.",
    blank: "Fields cannot contain only whitespace.",
    save: "Save",
    cancel: "Cancel",
  };
  const onSave = vi.fn();
  const onCancel = vi.fn();
  let root: Root;
  let host: HTMLDivElement;

  beforeEach(() => {
    vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
    onSave.mockClear();
    onCancel.mockClear();
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.unstubAllGlobals();
  });

  async function render(timer: Parameters<typeof TimerContentEditor>[0]["timer"], disabled = false) {
    await act(async () => root.render(
      <TimerContentEditor timer={timer} disabled={disabled} labels={labels} onSave={onSave} onCancel={onCancel} />,
    ));
  }

  async function input(field: "input" | "textarea", value: string) {
    const element = host.querySelector(field)!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(Object.getPrototypeOf(element), "value")!.set!.call(element, value);
      element.dispatchEvent(new Event("input", { bubbles: true }));
    });
  }

  async function submit() {
    await act(async () => {
      host.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
    });
  }

  it("shows title, then an instruction preview (including legacy), then the timer ID", () => {
    expect(timerDisplayName({ id: "timer-1", title: "Report", instruction: "Draft report" })).toBe("Report");
    expect(timerDisplayName({ id: "timer-1", instruction: "Draft\n  report" })).toBe("Draft report");
    expect(timerDisplayName({ id: "timer-1", summary: "Legacy instruction" })).toBe("Legacy instruction");
    expect(timerDisplayName({ id: "timer-1", instruction: null, summary: "Not current" })).toBe("timer-1");
  });

  it("sends only the edited title despite a refresh of an untouched legacy instruction", async () => {
    await render({ title: "Original", summary: "Legacy instruction" });
    expect(host.querySelector("textarea")?.value).toBe("Legacy instruction");
    expect(host.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled).toBe(true);
    await input("input", "");
    await render({ title: "Original", instruction: "New instruction" });
    await submit();
    expect(onSave).toHaveBeenCalledExactlyOnceWith({ title: null });
  });

  it("rejects whitespace-only edits, supports clearing instruction, and preserves meaningful whitespace", async () => {
    await render({ title: "Original", instruction: "Original instruction" });
    await input("textarea", " \n ");
    expect(host.querySelector('[role="alert"]')?.textContent).toBe(labels.blank);
    await submit();
    expect(onSave).not.toHaveBeenCalled();
    await input("textarea", "");
    await submit();
    expect(onSave).toHaveBeenLastCalledWith({ instruction: null });
    await input("textarea", "First line\n  Second line ");
    await submit();
    expect(onSave).toHaveBeenLastCalledWith({ instruction: "First line\n  Second line " });
    await render({ title: "Original", instruction: "Original instruction" }, true);
    await submit();
    expect(onSave).toHaveBeenCalledTimes(2);
  });
});
