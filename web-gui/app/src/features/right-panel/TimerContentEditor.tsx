import { useState } from "react";

interface TimerContentFields {
  title?: string | null;
  instruction?: string | null;
  summary?: string | null;
}

export type TimerContentPatch = {
  title?: string | null;
  instruction?: string | null;
};

export function timerContentValues(timer: TimerContentFields) {
  return {
    title: timer.title ?? "",
    instruction: (timer.instruction === undefined ? timer.summary : timer.instruction) ?? "",
  };
}

export function timerDisplayName(timer: TimerContentFields & { id: string }) {
  const { title, instruction } = timerContentValues(timer);
  return title.trim() || instruction.trim().replace(/\s+/g, " ") || timer.id;
}

export function TimerContentEditor({
  timer,
  labels,
  disabled,
  onSave,
  onCancel,
}: {
  timer: TimerContentFields;
  labels: { title: string; instruction: string; hint: string; blank: string; save: string; cancel: string };
  disabled: boolean;
  onSave: (patch: TimerContentPatch) => void;
  onCancel: () => void;
}) {
  // Compare with what the operator opened, not with a background refresh.
  const [original] = useState(() => timerContentValues(timer));
  const [title, setTitle] = useState(original.title);
  const [instruction, setInstruction] = useState(original.instruction);
  const changedTitle = title !== original.title;
  const changedInstruction = instruction !== original.instruction;
  const blankTitle = changedTitle && title !== "" && title.trim() === "";
  const blankInstruction = changedInstruction && instruction !== "" && instruction.trim() === "";
  const canSave = !disabled && (changedTitle || changedInstruction) && !blankTitle && !blankInstruction;

  return (
    <form className="agent-timer-editor" onSubmit={(event) => {
      event.preventDefault();
      if (!canSave) return;
      const patch: TimerContentPatch = {};
      if (changedTitle) patch.title = title === "" ? null : title;
      if (changedInstruction) patch.instruction = instruction === "" ? null : instruction;
      onSave(patch);
    }}>
      <label>
        {labels.title}
        <input type="text" value={title} disabled={disabled} aria-invalid={blankTitle || undefined}
          onChange={(event) => setTitle(event.currentTarget.value)} />
      </label>
      <label>
        {labels.instruction}
        <textarea rows={3} value={instruction} disabled={disabled} aria-invalid={blankInstruction || undefined}
          onChange={(event) => setInstruction(event.currentTarget.value)} />
      </label>
      <p className="inspector-muted">{labels.hint}</p>
      {blankTitle || blankInstruction ? <p className="error-text" role="alert">{labels.blank}</p> : null}
      <div className="agent-timer-actions">
        <button type="submit" className="secondary-button" disabled={!canSave}>{labels.save}</button>
        <button type="button" className="secondary-button" disabled={disabled} onClick={onCancel}>{labels.cancel}</button>
      </div>
    </form>
  );
}
