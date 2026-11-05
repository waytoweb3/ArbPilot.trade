import type { ButtonHTMLAttributes } from "react";

import { cn } from "../../lib/utils";

type Variant = "primary" | "secondary" | "ghost" | "danger";

const variantClasses: Record<Variant, string> = {
  primary:
    "bg-accent-solid text-on-accent hover:bg-accent-solid-hover disabled:bg-accent-solid/40 disabled:text-on-accent/60",
  secondary:
    "border border-line-strong bg-panel text-body hover:border-accent/50 hover:bg-hover disabled:text-subtle",
  ghost: "text-body hover:bg-hover hover:text-fg disabled:text-subtle",
  danger:
    "border border-danger/40 bg-danger/10 text-danger hover:bg-danger/20 disabled:text-danger/40",
};

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: "sm" | "md";
}

export function Button({
  variant = "primary",
  size = "md",
  className,
  ...props
}: ButtonProps) {
  return (
    <button
      className={cn(
        "inline-flex items-center justify-center gap-1.5 rounded-lg font-medium transition-colors disabled:cursor-not-allowed",
        size === "sm" ? "px-2.5 py-1 text-xs" : "px-3.5 py-2 text-sm",
        variantClasses[variant],
        className,
      )}
      {...props}
    />
  );
}
