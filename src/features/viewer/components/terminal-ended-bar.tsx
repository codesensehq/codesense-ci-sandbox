// SPDX-License-Identifier: MIT
import { StatusMark } from "@/shared/components/status-mark";
import { useEffect, useId, useRef, useState, type FocusEvent, type KeyboardEvent } from "react";
import { X } from "lucide-react";

import type { PaneStartupPhase } from "../hooks/use-pane-startup";

export type EndedOutcome = "ended" | "crashed";

export interface TerminalEndedBarProps {
  /**
   * `session`: the tab's shell is gone; the pane is dead until Restart.
   * `agent`: the agent crashed but the shell under it is still running (the pane
   * stays live, so this bar never takes focus).
   */
  kind: "session" | "agent";
  outcome: EndedOutcome;
  /** A restart or relaunch that failed (`detail` says why), or null. */
  failedAction: "Restart" | "Relaunch" | null;
  /** A relaunch is in flight: the actions are disabled until it settles. */
  busy: boolean;
  /** How it exited (`exitWords`: `exit 0`, `SIGKILL`), the restart failure, or null when the session vanished with no status. */
  detail: string | null;
  /** The agent CodeSense launched in this tab, when its launch line is known. */
  agent: string | null;
  /** Each increase moves keyboard focus onto the bar itself (never onto a
   *  button); 0 leaves focus where it is. */
  focusToken: number;
  onRestart?: () => void;
  onRelaunchAgent?: () => void;
  onClose: () => void;
  onDismiss?: () => void;
  /** Esc: hand focus back to the terminal (to read or copy its scrollback). */
  onEscape?: () => void;
}

/**
 * The pane shows one "not running" state at a time: the ended bar, or the
 * startup overlay (spinner, "Still starting…", "Failed · Retry").
 * - The session truly ended (its shell is gone): the bar, never the overlay —
 *   an ended session is not a failed start, and Retry could not bring it back.
 * - The attach failed while the session may still run: the overlay's Retry, and
 *   no bar (an agent crash seen in a shell the pane cannot show waits for it).
 * - Otherwise each shows as it would alone.
 */
export function paneEndShown(
  bar: TerminalEndedBarProps | null,
  startupPhase: PaneStartupPhase,
): { bar: TerminalEndedBarProps | null; overlay: boolean } {
  if (bar?.kind === "session") return { bar, overlay: false };
  if (startupPhase === "failed") return { bar: null, overlay: true };
  return { bar, overlay: true };
}

/** How long the bar ignores Enter after taking focus, so an Enter already on its
 *  way (typed into the shell as it died) cannot restart it. */
export const ENDED_BAR_ARM_MS = 400;

const BUTTON =
  "inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs border border-[var(--border,#30363d)] " +
  "text-[var(--foreground,#e6edf3)] hover:bg-[var(--accent,#30363d)] " +
  "focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--cs-accent,#388bfd)]";

/** A key that would type something (a printable character or Space). */
function isTyping(e: KeyboardEvent): boolean {
  return e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey;
}

/** The bar's headline: `Ended`, `Crashed`, `Restart failed`, or `claude crashed`. */
export function endedBarLabel(
  p: Pick<TerminalEndedBarProps, "kind" | "outcome" | "failedAction" | "agent">,
): string {
  if (p.failedAction) return `${p.failedAction} failed`;
  if (p.kind === "agent") return `${p.agent ?? "Agent"} crashed`;
  return p.outcome === "crashed" ? "Crashed" : "Ended";
}

/**
 * The bar at the bottom of a pane whose process ended: how it ended, and what
 * to do next. It sits below the terminal (not over it), so the scrollback above
 * stays whole and readable.
 *
 * Keyboard: when the bar takes focus it focuses itself, never a button, so no
 * key typed as the shell died can press one. While it has focus:
 * - Enter restarts (or runs the focused button) — a fresh press only, not a
 *   held key's repeat, and not within {@link ENDED_BAR_ARM_MS} of the bar taking
 *   focus. The Enter is consumed here, so it never reaches the new shell. On
 *   the agent-crash bar only a focused button acts on Enter.
 * - Esc returns focus to the terminal.
 * - Anything typed (Space included) does nothing but show how to go on.
 * The buttons stay clickable and reachable with Tab.
 */
export function TerminalEndedBar({
  kind,
  outcome,
  failedAction,
  busy,
  detail,
  agent,
  focusToken,
  onRestart,
  onRelaunchAgent,
  onClose,
  onDismiss,
  onEscape,
}: TerminalEndedBarProps) {
  const rootRef = useRef<HTMLDivElement>(null);
  // When focus last came into the bar: Enter is armed ENDED_BAR_ARM_MS later.
  const focusedAtRef = useRef(0);
  const [hint, setHint] = useState(false);
  useEffect(() => {
    if (focusToken > 0) rootRef.current?.focus();
  }, [focusToken]);

  const onFocus = (e: FocusEvent<HTMLDivElement>) => {
    const from = e.relatedTarget as Node | null;
    if (!from || !e.currentTarget.contains(from)) focusedAtRef.current = Date.now();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Enter") {
      // Handled here, never as a native button activation: the terminal takes
      // focus during a restart, and an unconsumed Enter would land in it.
      e.preventDefault();
      e.stopPropagation();
      if (busy || e.repeat || Date.now() - focusedAtRef.current < ENDED_BAR_ARM_MS) return;
      const button = (e.target as HTMLElement).closest("button");
      if (button) button.click();
      else if (kind === "session") onRestart?.();
    } else if (e.key === "Escape" && onEscape) {
      e.preventDefault();
      e.stopPropagation();
      onEscape();
    } else if (isTyping(e)) {
      // Space included: it must never press a button either.
      e.preventDefault();
      e.stopPropagation();
      setHint(true);
    }
  };
  const onKeyUp = (e: KeyboardEvent<HTMLDivElement>) => {
    // A button activates on Space's keyup: swallow that too.
    if (e.key === " ") {
      e.preventDefault();
      e.stopPropagation();
    }
  };

  const label = endedBarLabel({ kind, outcome, failedAction, agent });
  const hintId = useId();
  const crashed = outcome === "crashed";
  const hintText =
    kind === "session" && onRestart
      ? "Press ⏎ to restart · Esc to scroll"
      : "Tab to an action · Esc to go back";

  return (
    <div
      ref={rootRef}
      role="group"
      aria-label={`${label}${detail ? ` · ${detail}` : ""}`}
      aria-describedby={hintId}
      aria-busy={busy || undefined}
      data-testid="terminal-ended-bar"
      data-kind={kind}
      data-outcome={outcome}
      tabIndex={-1}
      onFocus={onFocus}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setHint(false);
      }}
      onKeyDown={onKeyDown}
      onKeyUp={onKeyUp}
      className="flex shrink-0 flex-wrap items-center gap-2 border-t border-[var(--border,#30363d)] bg-[var(--popover,#161b22)] px-3 py-1.5 text-xs text-[var(--foreground,#e6edf3)] outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-[#e6edf3]"
    >
      <StatusMark mark={crashed ? "crashed" : "ended"} size="md" />
      <span className="min-w-0 flex-1 truncate">
        <span className="font-medium">{label}</span>
        {detail && <span className="text-[var(--muted-foreground,#8b949e)]"> · {detail}</span>}
        {/* Always there as the bar's description; shown once a key goes nowhere. */}
        <span
          id={hintId}
          className={hint ? "ml-2 text-[var(--muted-foreground,#8b949e)]" : "sr-only"}
          data-testid="terminal-ended-hint"
          data-shown={hint || undefined}
        >
          {hintText}
        </span>
      </span>
      {onRestart && (
        <button
          type="button"
          className={BUTTON}
          disabled={busy}
          onClick={onRestart}
          aria-keyshortcuts="Enter"
          data-testid="terminal-ended-restart"
        >
          Restart{" "}
          <kbd aria-hidden className="font-sans opacity-70">
            ⏎
          </kbd>
        </button>
      )}
      {onRelaunchAgent && agent && (
        <button
          type="button"
          className={BUTTON}
          disabled={busy}
          onClick={onRelaunchAgent}
          title={`Runs ${agent} again in this tab with the line it was launched with. It starts a new ${agent} session; the old one is not resumed.`}
          data-testid="terminal-ended-relaunch"
        >
          Relaunch {agent}
        </button>
      )}
      <button
        type="button"
        className={BUTTON}
        disabled={busy}
        onClick={onClose}
        data-testid="terminal-ended-close"
      >
        Close
      </button>
      {onDismiss && (
        <button
          type="button"
          aria-label="Dismiss"
          className="rounded p-0.5 text-[var(--muted-foreground,#8b949e)] hover:text-[var(--foreground,#e6edf3)]"
          onClick={onDismiss}
          data-testid="terminal-ended-dismiss"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  );
}

/**
 * The pane's always-mounted live regions for its ended bar: a crash is
 * announced assertively, a plain end politely. Mounted with the pane (not with
 * the bar), so screen readers already track them when the text arrives.
 */
export function TerminalEndedAnnouncer({ bar }: { bar: TerminalEndedBarProps | null }) {
  const text = bar ? `${endedBarLabel(bar)}${bar.detail ? ` · ${bar.detail}` : ""}` : "";
  const urgent = bar?.outcome === "crashed";
  return (
    <>
      <span
        className="sr-only"
        aria-live="assertive"
        data-testid="terminal-ended-announce-assertive"
      >
        {urgent ? text : ""}
      </span>
      <span className="sr-only" aria-live="polite" data-testid="terminal-ended-announce-polite">
        {bar && !urgent ? text : ""}
      </span>
    </>
  );
}
