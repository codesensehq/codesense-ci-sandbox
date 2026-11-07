// SPDX-License-Identifier: MIT
import { X } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * The Connections tab's shared chrome: the right-hand RESERVE panel (add · edit ·
 * reconnect · add machine — never a nested modal) and the quiet per-row verb.
 * One shell so every reserve panel reads the same.
 */
export function ReservePanel({
  testId,
  icon,
  title,
  subtitle,
  closeLabel,
  onClose,
  children,
  footer,
}: {
  testId: string;
  icon: React.ReactNode;
  title: string;
  subtitle?: string;
  closeLabel: string;
  onClose: () => void;
  children?: React.ReactNode;
  footer?: React.ReactNode;
}) {
  return (
    <div data-testid={testId} className="flex min-h-0 w-1/2 flex-col overflow-y-auto p-6">
      <div className="flex items-start justify-between">
        <div className="flex items-center gap-2">
          <span className="text-muted-foreground">{icon}</span>
          <div>
            <h3 className="text-sm font-semibold">{title}</h3>
            {subtitle && <p className="mt-0.5 text-xs text-muted-foreground">{subtitle}</p>}
          </div>
        </div>
        <button
          type="button"
          aria-label={closeLabel}
          onClick={onClose}
          className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <X size={14} aria-hidden="true" />
        </button>
      </div>
      {children}
      {footer && <div className="mt-6 flex items-center justify-end gap-2 pt-4">{footer}</div>}
    </div>
  );
}

export function RowAction({
  testid,
  icon,
  label,
  onClick,
  disabled = false,
  destructive = false,
  ref,
}: {
  /** React 19: the button, for a caller that moves focus to it. */
  ref?: React.Ref<HTMLButtonElement>;
  testid: string;
  icon?: React.ReactNode;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  /** The verb removes something — read as such, not as a neutral row verb. */
  destructive?: boolean;
}) {
  return (
    <button
      ref={ref}
      type="button"
      data-testid={testid}
      onClick={onClick}
      disabled={disabled}
      className={cn(
        "flex items-center gap-1 rounded px-2 py-1 text-xs hover:bg-accent disabled:pointer-events-none disabled:opacity-50",
        destructive
          ? "font-medium text-destructive hover:text-destructive"
          : "text-muted-foreground hover:text-foreground",
      )}
    >
      {icon}
      {label}
    </button>
  );
}
