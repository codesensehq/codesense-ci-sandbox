// SPDX-License-Identifier: MIT
import {
  selectGhostHint,
  useTerminalGhostHintStore,
} from "@/features/viewer/stores/terminal-ghost-hint-store";

/**
 * SLW-TL-7 — the ghosted placeholder painted over a FRESH agent terminal so the empty
 * prompt reads as an invitation, not a void (north-star §8 rule 11). Renders NOTHING unless
 * this session was opened with a hint (the common case → a clean no-op), and it is
 * `pointer-events-none` so it never intercepts focus/typing — the first keystroke clears the
 * hint from the store and the overlay disappears. Purely decorative (`aria-hidden`): the
 * real prompt is the live terminal beneath it.
 */
export function TerminalGhostHint({ sessionId }: { sessionId: string }): React.JSX.Element | null {
  const hint = useTerminalGhostHintStore(selectGhostHint(sessionId));
  if (!hint) return null;
  return (
    <div
      data-testid="terminal-ghost-hint"
      aria-hidden
      className="pointer-events-none absolute left-3 top-2 z-10 select-none text-sm italic text-muted-foreground/50"
    >
      {hint}
    </div>
  );
}
