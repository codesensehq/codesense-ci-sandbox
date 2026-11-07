// SPDX-License-Identifier: MIT
import { cn } from "@/lib/utils";
import { StatusMark } from "@/shared/components/status-mark";
import { MARK_TONE, type StatusMarkKind } from "@/shared/lib/status-mark";

/**
 * The shared status-pill visual — ONE home for the rounded, tinted chip so every status
 * affordance renders the same pill and can never drift apart: the status mark, then its
 * words in high-contrast ink, with the background tinted by the mark's tone.
 */
export const STATUS_PILL_CLASS =
  "inline-flex w-fit shrink-0 items-center gap-1 rounded-full px-1.5 py-0.5 text-xs font-medium leading-none";

/** The tinted-background formula for a mark — the single source so the `color-mix`
 *  tint can't be re-spelled (and drift) per call site. */
export function statusPillStyle(mark: StatusMarkKind): React.CSSProperties {
  const tone = MARK_TONE[mark];
  return {
    color: "var(--cs-status-text)",
    backgroundColor: `color-mix(in srgb, ${tone} 14%, transparent)`,
  };
}

/** The pill's content: the mark at chip size, then the words. */
export function StatusPillContent({ mark, label }: { mark: StatusMarkKind; label: string }) {
  return (
    <>
      <StatusMark
        mark={mark}
        size={9}
        className={
          mark === "question" || mark === "permission" || mark === "blocked" ? "mx-0.5" : undefined
        }
      />
      {label}
    </>
  );
}

/**
 * A pure, prop-driven status pill. Presentational only — no state, no effects, no
 * timers — so it renders from props alone and never blocks a render.
 */
export function StatusPill({
  mark,
  label,
  className,
  ariaLabel,
}: {
  mark: StatusMarkKind;
  label: string;
  className?: string;
  ariaLabel?: string;
}): React.JSX.Element {
  return (
    <span
      role="img"
      aria-label={ariaLabel ?? `Status: ${label.toLowerCase()}`}
      className={cn(STATUS_PILL_CLASS, className)}
      style={statusPillStyle(mark)}
    >
      <StatusPillContent mark={mark} label={label} />
    </span>
  );
}
