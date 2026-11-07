// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";
// eslint-disable-next-line no-restricted-imports -- the one place the spinning icon is wrapped
import { Loader2 } from "lucide-react";

import { AppButton } from "@/shared/components/app-button";
import { cn } from "@/lib/utils";

/** How long a wait may run before it offers a way out (Cancel, Retry, Open logs). */
export const WAIT_ESCAPE_AFTER_MS = 30_000;

const SIZES = { sm: "size-3.5", md: "size-4", lg: "size-8" } as const;

/** Waiting copy writes an ellipsis, never three dots. */
function ellipsis(label: string): string {
  return label.replace(/\.{3}/g, "…");
}

/**
 * The one spinner: a short present-tense label ("Cloning acme/context…") beside an
 * icon that spins only when motion is allowed. A polite status, so a screen reader
 * hears what the app is doing. See the consistency spec, Rule 4.
 */
export function Spinner({
  label,
  size = "sm",
  decorative = false,
  className,
  testId,
}: {
  label?: string;
  size?: keyof typeof SIZES;
  /** Just the icon, hidden from assistive tech — for a control that already says
   *  what it is doing (a busy button). */
  decorative?: boolean;
  className?: string;
  testId?: string;
}) {
  if (decorative) {
    return (
      <Loader2
        data-slot="spinner"
        data-testid={testId}
        aria-hidden
        className={cn(SIZES[size], "shrink-0 motion-safe:animate-spin", className)}
      />
    );
  }
  const text = label ? ellipsis(label) : undefined;
  return (
    <span
      role="status"
      aria-live="polite"
      aria-label={text ? undefined : "Loading…"}
      data-slot="spinner"
      data-testid={testId}
      className={cn("inline-flex items-center gap-2 text-muted-foreground", className)}
    >
      <Loader2 className={cn(SIZES[size], "shrink-0 motion-safe:animate-spin")} aria-hidden />
      {text && <span>{text}</span>}
    </span>
  );
}

/**
 * True once a wait has run past {@link WAIT_ESCAPE_AFTER_MS}, so the surface can
 * offer its way out (Rule 4). Resets when the wait ends.
 */
export function useSlowWait(active: boolean): boolean {
  // One token per wait: the timer marks that wait slow, and a new wait (a new
  // token) starts fresh without resetting state inside the effect.
  const wait = useMemo(() => (active ? {} : null), [active]);
  const [slowWait, setSlowWait] = useState<object | null>(null);
  useEffect(() => {
    if (!wait) return;
    const timer = setTimeout(() => setSlowWait(wait), WAIT_ESCAPE_AFTER_MS);
    return () => clearTimeout(timer);
  }, [wait]);
  return wait !== null && slowWait === wait;
}

/**
 * A centered wait for a whole surface: the spinner, its label, an optional line on
 * what is happening, and — once the wait passes {@link WAIT_ESCAPE_AFTER_MS} — the
 * way out it was given.
 */
export function LoadingState({
  label,
  detail,
  escape,
  testId,
  className,
}: {
  label: string;
  detail?: string;
  escape?: { label: string; onClick: () => void; testId?: string };
  testId?: string;
  className?: string;
}) {
  // Keyed on whether there is a way out, not on the (per-render) object, so a
  // re-render never restarts the clock.
  const slow = useSlowWait(escape !== undefined);
  return (
    <div
      data-slot="loading-state"
      data-testid={testId}
      className={cn("flex flex-col items-center gap-2 px-6 py-10 text-center text-sm", className)}
    >
      <Spinner label={label} size="md" />
      {detail && <p className="max-w-xs text-xs text-muted-foreground">{detail}</p>}
      {slow && escape && (
        <AppButton
          type="button"
          variant="outline"
          size="sm"
          onClick={escape.onClick}
          data-testid={escape.testId}
        >
          {escape.label}
        </AppButton>
      )}
    </div>
  );
}
