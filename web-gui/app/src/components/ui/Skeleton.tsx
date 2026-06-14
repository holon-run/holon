import type { HTMLAttributes } from "react";

import { cn } from "../../lib/utils";

interface SkeletonProps extends HTMLAttributes<HTMLElement> {
  as?: "div" | "span";
}

export function Skeleton({ as: Component = "div", className, ...props }: SkeletonProps) {
  return (
    <Component
      className={cn(
        "animate-pulse rounded-full bg-[linear-gradient(90deg,var(--surface-soft),color-mix(in_srgb,var(--surface-soft)_72%,#fff),var(--surface-soft))]",
        className,
      )}
      {...props}
    />
  );
}
