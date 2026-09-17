import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { RuntimeModelOption } from "../../runtime/types";
import { modelSourceLabel } from "../../lib/model-presentation";
import { ModelList } from "./ModelList";

export function ModelSelect({ label, options, value, onChange, allowEmpty = false }: {
  label: string; options: readonly RuntimeModelOption[]; value: string;
  onChange: (value: string) => void; allowEmpty?: boolean;
}) {
  const { t } = useTranslation();
  const id = useId();
  const ref = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const selected = options.find((option) => option.routeRef === value);
  function close() { setOpen(false); trigger.current?.focus(); }
  useEffect(() => {
    if (!open) return;
    const click = (event: PointerEvent) => { if (!ref.current?.contains(event.target as Node)) setOpen(false); };
    document.addEventListener("pointerdown", click);
    return () => document.removeEventListener("pointerdown", click);
  }, [open]);
  return <div className="model-select" ref={ref} onKeyDown={(event) => {
    if (event.key === "Escape" && open) { event.stopPropagation(); close(); }
  }}>
    <span id={`${id}-label`} className="model-select-label">{label}</span>
    <button ref={trigger} type="button" className="model-select-trigger" aria-labelledby={`${id}-label ${id}-value`} aria-expanded={open} aria-controls={id} onClick={() => setOpen(!open)}>
      <span id={`${id}-value`}>{selected ? `${selected.displayName} · ${modelSourceLabel(selected)}` : value || t(allowEmpty ? "modelUi.automatic" : "modelUi.chooseModel")}</span><span aria-hidden>⌄</span>
    </button>
    {open ? <div id={id} className="model-select-panel">
      {allowEmpty ? <button type="button" className="model-browser-more" onClick={() => { onChange(""); close(); }}>{t("modelUi.automatic")}</button> : null}
      <ModelList options={options} value={value} autoFocus onSelect={(model) => { onChange(model.routeRef); close(); }} />
      <details className="model-manual"><summary>{t("modelUi.manualRoute")}</summary>
        <label>{t("modelUi.route")}<input aria-label={`${label}: ${t("modelUi.route")}`} value={value} onChange={(event) => onChange(event.target.value)} placeholder="provider@endpoint/model" /></label>
      </details>
      <button type="button" className="model-browser-more" onClick={close}>{t("common.close")}</button>
    </div> : null}
  </div>;
}
