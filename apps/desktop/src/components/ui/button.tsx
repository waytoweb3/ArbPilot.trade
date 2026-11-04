import type { ButtonHTMLAttributes } from "react";

import { cn } from "../../lib/utils";

type Variant = "primary" | "secondary" | "ghost" | "danger";

const variantClasses: Record<Variant, string> = {
  primary:
    "bg-emerald-500 text-ink-950 hover:bg-emerald-400 disabled:bg-emerald-500/40 disabled:text-ink-950/60",
  secondary:
    "border border-ink-600 bg-ink-800 text-slate-200 hover:border-ink-500 hover:bg-ink-700 disabled:text-slate-500",
  ghost: "text-slate-300 hover:bg-ink-800 hover:text-white disabled:text-slate-600",
  danger:
    "border border-rose-400/40 bg-rose-500/10 text-rose-300 hover:bg-rose-500/20 disabled:text-rose-300/40",
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
