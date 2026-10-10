import { useEffect, useRef, type ReactNode } from "react";

import { Button } from "../../components/ui/Button";

interface ComposerOptionPickerProps {
  kind: "speed" | "thinking";
  title: string;
  label: string;
  triggerLabel: string;
  icon: ReactNode;
  value: string;
  options: { value: string; label: string }[];
  description?: string;
  disabled: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onChange: (value: string) => void;
}

export function ComposerOptionPicker({ kind, title, label, triggerLabel, icon, value, options, description, disabled, open, onOpenChange, onChange }: ComposerOptionPickerProps) {
  const pickerRef = useRef<HTMLDivElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    popoverRef.current?.querySelector<HTMLButtonElement>('[aria-pressed="true"]')?.focus();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!pickerRef.current?.contains(event.target as Node)) onOpenChange(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open, onOpenChange]);

  return (
    <div
      className={`composer-option-picker ${kind}-picker`}
      ref={pickerRef}
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          onOpenChange(false);
          pickerRef.current?.querySelector<HTMLButtonElement>(".composer-option-button")?.focus();
        }
      }}
      onBlur={(event) => {
        if (open && !event.currentTarget.contains(event.relatedTarget)) onOpenChange(false);
      }}
    >
      <Button
        className={`composer-option-button ${kind}-button`}
        type="button"
        variant="ghost"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={triggerLabel}
        title={triggerLabel}
        disabled={disabled}
        onClick={() => onOpenChange(!open)}
      >
        {icon}
        <small>{label}</small>
      </Button>
      {open ? (
        <div className={`composer-option-popover ${kind}-popover`} ref={popoverRef} role="dialog" aria-label={title}>
          <strong className="composer-option-title">{title}</strong>
          <div className="reasoning-options">
            {options.map((option) => (
              <button
                className={value === option.value ? "is-active" : undefined}
                aria-pressed={value === option.value}
                key={option.value}
                type="button"
                disabled={disabled}
                onClick={() => {
                  onOpenChange(false);
                  pickerRef.current?.querySelector<HTMLButtonElement>(".composer-option-button")?.focus();
                  onChange(option.value);
                }}
              >
                {option.label}
              </button>
            ))}
          </div>
          {description ? <p className="composer-option-description">{description}</p> : null}
        </div>
      ) : null}
    </div>
  );
}
