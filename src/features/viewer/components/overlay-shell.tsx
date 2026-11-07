// SPDX-License-Identifier: MIT
import { forwardRef, useEffect, useRef } from "react";
import type { ButtonHTMLAttributes, KeyboardEvent, ReactNode, Ref } from "react";

import { cn } from "@/lib/utils";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/**
 * The one floating chrome shared by every per-tile worktree overlay — the source
 * diff, the in-app editor, and the hydration inventory. It owns the surface only:
 * the popover border/background/foreground tokens, the radius + shadow, and the
 * two footprints an overlay can take over its tile:
 *
 *  - `inset` — the expanded reader: `absolute inset-3`, `rounded-lg`, `shadow-xl`.
 *    A flex ROW so a caller can lay out a rail + section.
 *  - `card` — the compact corner card: `absolute right-2 top-2`, `rounded-md`,
 *    `shadow-md`. A flex COLUMN; the caller passes its own `w-*`/`max-h-*`.
 *
 * Extracted (successor to the former `TileInset`) so the three overlays cannot
 * drift on the chrome class string or the `var(--border)` / `--border` token
 * spelling — every color here is a single CSS variable.
 */
export type OverlaySize = "inset" | "card";

// The one popover token set both footprints share (the whole point of the shell —
// no `--border` vs `var(--border)` drift). The size variants add only their own
// positioning / radius / shadow on top.
const OVERLAY_CHROME_BASE =
  "pointer-events-auto absolute z-10 flex overflow-hidden border border-[var(--border)] bg-[var(--popover)] text-[var(--popover-foreground)] opacity-95";

const OVERLAY_CHROME: Record<OverlaySize, string> = {
  inset: cn(OVERLAY_CHROME_BASE, "inset-3 select-text rounded-lg shadow-xl"),
  card: cn(OVERLAY_CHROME_BASE, "right-2 top-2 flex-col rounded-md shadow-md"),
};

export function OverlayShell({
  size = "inset",
  testId,
  ariaLabel,
  tabIndex,
  onKeyDown,
  className,
  children,
  ref,
}: {
  size?: OverlaySize;
  /** The shell element — for an overlay that manages focus on open/close. */
  ref?: Ref<HTMLDivElement>;
  testId: string;
  ariaLabel: string;
  tabIndex?: number;
  onKeyDown?: (e: KeyboardEvent<HTMLDivElement>) => void;
  /** Extra classes — e.g. the `card` size's `w-*`/`max-h-*`/padding. */
  className?: string;
  children: ReactNode;
}) {
  return (
    <div
      ref={ref}
      data-testid={testId}
      role="dialog"
      aria-label={ariaLabel}
      tabIndex={tabIndex}
      onKeyDown={onKeyDown}
      className={cn(OVERLAY_CHROME[size], className)}
    >
      {children}
    </div>
  );
}

/**
 * The shared overlay header row: the `border-b` chrome with an optional leading
 * icon + title, a **stats slot** (the +/− diff counts / status pill an overlay
 * fills later — a slot, not a feature here), and an actions area for the
 * overlay's own {@link ChromeIconButton}s (mode/split/collapse/close …). Gives
 * every overlay one header border + padding so the diff, editor, and hydration
 * headers can't drift on the surrounding chrome while each keeps its own controls.
 */
export function OverlayHeader({
  icon,
  title,
  stats,
  actions,
  className,
}: {
  icon?: ReactNode;
  title?: ReactNode;
  /** The +/− counts / status slot (empty until a diff story fills it). */
  stats?: ReactNode;
  /** The overlay's own header controls (rendered after the stats slot). Give the
   *  `title` slot a `flex-1` element (each overlay already needs one to truncate
   *  its file name) — it absorbs the free space and pushes these to the right. */
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex select-none items-center gap-1.5 border-b border-[var(--border)] px-2.5 py-1.5 text-xs",
        className,
      )}
    >
      {icon}
      {title}
      {stats}
      {actions}
    </div>
  );
}

/**
 * The shared "centered message" body state — the loading / empty / error
 * presentation every overlay reads the same. A single token-driven block: muted
 * copy by default, `text-destructive` for the error tone, with an optional
 * `action` (e.g. a Retry control) beneath the message.
 */
export function OverlayCenteredState({
  tone = "muted",
  role,
  testId,
  ariaHidden,
  action,
  children,
}: {
  tone?: "muted" | "destructive";
  role?: "alert" | "status";
  testId?: string;
  ariaHidden?: boolean;
  /** Rendered under the message — e.g. a Retry button. */
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div
      role={role}
      data-testid={testId}
      aria-hidden={ariaHidden}
      className={cn(
        "flex h-full flex-col items-center justify-center gap-2 px-8 text-center text-xs",
        tone === "destructive" ? "text-destructive" : "text-muted-foreground",
      )}
    >
      {children}
      {action}
    </div>
  );
}

/**
 * The one guarded Escape-to-dismiss every overlay shares (AC-5): a `document`
 * keydown listener that closes the overlay UNLESS a text field / contenteditable
 * owns the key, the focus is inside a terminal (`.xterm` keeps its own Escape),
 * or a tile is zoomed (zoom-restore takes precedence). `enabled` gates the
 * subscription so a hidden / non-focused overlay never listens. `onDismiss` is
 * held in a ref, so the listener always calls the latest one and only ever
 * re-subscribes when `enabled` / `zoomActive` change — callers can pass an inline
 * closure without memoizing it.
 *
 * Overlays with richer Escape semantics — e.g. the editor's dirty-buffer guard
 * that must `stopPropagation` — own their handler on the shell element instead;
 * this hook is the plain "Escape hides me" path the diff and hydration share.
 */
export function useOverlayEscape({
  enabled,
  zoomActive = false,
  onDismiss,
  onTerminalEscape,
}: {
  enabled: boolean;
  zoomActive?: boolean;
  onDismiss: () => void;
  /**
   * SW-FX-140: Escape pressed while focus is in the terminal (`.xterm`) is taken here
   * instead of `onDismiss`, and never reaches the terminal. Provided only by an overlay
   * that owns Escape over a terminal (the diff panel); it decides what that Escape does
   * (e.g. close an open keyboard map first). Omitted, the terminal keeps Escape.
   */
  onTerminalEscape?: () => void;
}) {
  // Keep the latest callbacks in refs (updated in an effect, not during render) so the
  // listeners below only re-subscribe on enabled / zoomActive / owning changes.
  const onDismissRef = useRef(onDismiss);
  const onTerminalEscapeRef = useRef(onTerminalEscape);
  useEffect(() => {
    onDismissRef.current = onDismiss;
    onTerminalEscapeRef.current = onTerminalEscape;
  }, [onDismiss, onTerminalEscape]);
  const owns = onTerminalEscape !== undefined;
  useEffect(() => {
    if (!enabled || zoomActive) return;
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (t?.closest?.(".xterm")) return;
      onDismissRef.current();
    };
    document.addEventListener("keydown", onKey);
    // An owned Escape from the terminal is taken in the capture phase, before the
    // terminal's own handling, so it never reaches the agent input. Other Escapes keep
    // the bubble path above, so a control inside the overlay still gets its Escape first.
    const onOwnedTerminalKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const t = e.target as HTMLElement | null;
      if (!t?.closest?.(".xterm")) return;
      e.stopPropagation();
      e.preventDefault();
      onTerminalEscapeRef.current?.();
    };
    if (owns) document.addEventListener("keydown", onOwnedTerminalKey, true);
    return () => {
      document.removeEventListener("keydown", onKey);
      if (owns) document.removeEventListener("keydown", onOwnedTerminalKey, true);
    };
  }, [enabled, zoomActive, owns]);
}

/** Hit-target presets. `inset` (default) = the original padding-sized control
 *  used by the source-diff / editor insets; `sm`/`md` are the fixed squares the
 *  notes chrome needs (row + header actions / collapsed strip). SW-SN-14. */
const CHROME_ICON_SIZE = {
  inset: "p-0.5",
  sm: "h-6 w-6",
  md: "h-8 w-8",
} as const;

/**
 * The shared header/chrome icon-button: a `text-muted-foreground → foreground`
 * control with a uniform hover, focus-ring, and sizing. Used across the
 * source-diff / editor inset headers AND (SW-SN-14) the notes rail strip, row
 * actions, and panel header — one control instead of four bespoke class strings.
 *
 * `tooltip` is optional: when given, the button is wrapped in a Radix `Tooltip`
 * (needs a `TooltipProvider` ancestor — every inset header and the notes rail
 * already provide one); when omitted, it renders a bare `<button>` so it can be
 * a Radix `asChild` trigger (e.g. `PopoverTrigger`) or carry a native `title`.
 * `forwardRef` + rest-spread let it receive an injected trigger ref and any
 * `aria-*` / `title` / `disabled` a call-site needs.
 */
export const ChromeIconButton = forwardRef<
  HTMLButtonElement,
  {
    testId?: string;
    ariaLabel: string;
    /** Optional — omit for a bare button (row/header actions, asChild triggers). */
    tooltip?: ReactNode;
    tooltipSide?: "top" | "right" | "bottom" | "left";
    size?: keyof typeof CHROME_ICON_SIZE;
    /** Persistent (non-hover) emphasis — e.g. the active pinned chip in the strip. */
    active?: boolean;
    children: ReactNode;
  } & Omit<ButtonHTMLAttributes<HTMLButtonElement>, "aria-label" | "type" | "className">
>(function ChromeIconButton(
  {
    testId,
    ariaLabel,
    tooltip,
    tooltipSide = "top",
    size = "inset",
    active = false,
    children,
    ...rest
  },
  ref,
) {
  const button = (
    <button
      ref={ref}
      type="button"
      data-testid={testId}
      aria-label={ariaLabel}
      className={`inline-flex shrink-0 items-center justify-center rounded ${CHROME_ICON_SIZE[size]} ${
        active
          ? // Keep the filled/toggled look on hover — only a click changes state.
            "bg-accent text-accent-foreground hover:bg-accent/80"
          : "text-muted-foreground hover:bg-muted hover:text-foreground"
      } focus-visible:outline-2 focus-visible:[outline-color:var(--ring)] disabled:pointer-events-none disabled:opacity-40`}
      {...rest}
    >
      {children}
    </button>
  );
  if (!tooltip) return button;
  return (
    <Tooltip>
      <TooltipTrigger asChild>{button}</TooltipTrigger>
      <TooltipContent side={tooltipSide}>{tooltip}</TooltipContent>
    </Tooltip>
  );
});
