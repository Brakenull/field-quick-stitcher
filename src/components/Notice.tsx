import type { ReactNode } from "react";

export type NoticeVariant = "success" | "warning" | "error" | "info";

const ICON_PATHS: Record<NoticeVariant, string> = {
  success: "M5 10.5l3.5 3.5L15 7",
  warning: "M10 3l8 14H2L10 3zm0 5v4m0 2.5v.5",
  error: "M10 2a8 8 0 100 16 8 8 0 000-16zm-3 5l6 6m0-6l-6 6",
  info: "M10 2a8 8 0 100 16 8 8 0 000-16zm0 7v5m0-8v.5",
};

/** Inline status message (DESIGN.md 4.9): a colored left border plus an icon,
 * so the status never relies on color alone. Errors are announced
 * assertively, everything else politely. */
export function Notice({ variant, children }: { variant: NoticeVariant; children: ReactNode }) {
  return (
    <div className={`notice notice--${variant}`} role={variant === "error" ? "alert" : "status"}>
      <svg className="notice__icon" viewBox="0 0 20 20" aria-hidden="true">
        <path d={ICON_PATHS[variant]} />
      </svg>
      <p className="notice__text">{children}</p>
    </div>
  );
}
