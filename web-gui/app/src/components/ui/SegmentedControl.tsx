import type { ButtonHTMLAttributes, ReactNode } from "react";

import { cn } from "../../lib/utils";

interface SegmentedControlProps {
  label: string;
  children: ReactNode;
  className?: string;
}

interface SegmentedControlButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  active?: boolean;
}

export function SegmentedControl({ label, children, className }: SegmentedControlProps) {
  return (
    <div
      className={cn("inline-flex items-center gap-1 rounded-xl border border-line bg-surface-soft p-1", className)}
      aria-label={label}
    >
      {children}
    </div>
  );
}

export function SegmentedControlButton({
  active,
  className,
  type = "button",
  ...props
}: SegmentedControlButtonProps) {
  return (
    <button
      className={cn(
        "min-h-7 rounded-lg border-0 px-3 text-xs font-semibold text-muted transition-colors hover:text-text",
        active ? "bg-surface text-text shadow-subtle" : "bg-transparent",
        className,
      )}
      type={type}
      {...props}
    />
  );
}
