import type { ButtonHTMLAttributes } from "react";

import { cn } from "../../lib/utils";

type ButtonVariant = "default" | "secondary" | "ghost" | "outline" | "accent";
type ButtonSize = "sm" | "md" | "icon";

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
}

const variantClass: Record<ButtonVariant, string> = {
  default: "border border-line bg-surface text-text shadow-subtle hover:bg-surface-hover",
  secondary: "border border-line bg-surface-soft text-muted hover:bg-surface-hover hover:text-text",
  ghost: "border border-transparent bg-transparent text-muted hover:bg-surface-hover hover:text-text",
  outline: "border border-line bg-transparent text-text hover:bg-surface-hover",
  accent: "border border-accent bg-accent text-white shadow-subtle hover:bg-accent/90",
};

const sizeClass: Record<ButtonSize, string> = {
  sm: "min-h-8 px-3 text-xs",
  md: "min-h-9 px-3.5 text-sm",
  icon: "h-9 w-9 p-0",
};

export function Button({ className, variant = "default", size = "md", type = "button", ...props }: ButtonProps) {
  return (
    <button
      className={cn(
        "inline-flex items-center justify-center gap-2 rounded-ui font-semibold transition-colors disabled:cursor-default disabled:opacity-50",
        variantClass[variant],
        sizeClass[size],
        className,
      )}
      type={type}
      {...props}
    />
  );
}
