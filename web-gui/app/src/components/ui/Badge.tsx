import type { HTMLAttributes } from "react";

import { cn } from "../../lib/utils";

type BadgeTone = "neutral" | "accent" | "success" | "warning" | "danger" | "muted";

interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: BadgeTone;
}

const toneClass: Record<BadgeTone, string> = {
  neutral: "border-line bg-surface-soft text-muted",
  accent: "border-accent/30 bg-accent-soft text-accent",
  success: "border-success/25 bg-success/10 text-success",
  warning: "border-warning/30 bg-warning/10 text-warning",
  danger: "border-danger/30 bg-danger/10 text-danger",
  muted: "border-line bg-muted/10 text-muted",
};

export function Badge({ className, tone = "neutral", ...props }: BadgeProps) {
  return (
    <span
      className={cn(
        "inline-flex min-h-6 w-fit items-center gap-1 rounded-full border px-2.5 py-0.5 text-xs font-bold leading-none",
        toneClass[tone],
        className,
      )}
      {...props}
    />
  );
}
