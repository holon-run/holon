import type { ReactNode } from "react";

import { cn } from "../../lib/utils";

interface EmptyStateProps {
  title: string;
  description: ReactNode;
  icon?: ReactNode;
  action?: ReactNode;
  className?: string;
}

export function EmptyState({ title, description, icon = "◇", action, className }: EmptyStateProps) {
  return (
    <div
      className={cn(
        "grid place-items-center gap-2 rounded-card border border-dashed border-line-strong bg-surface/80 p-8 text-center text-muted shadow-card",
        className,
      )}
      role="status"
    >
      <span className="grid h-11 w-11 place-items-center rounded-2xl border border-line bg-surface-soft text-xl text-faint">
        {icon}
      </span>
      <strong className="text-text">{title}</strong>
      <span className="max-w-lg text-sm leading-6">{description}</span>
      {action ? <div className="empty-state-action">{action}</div> : null}
    </div>
  );
}
