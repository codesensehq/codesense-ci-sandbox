// SPDX-License-Identifier: MIT
import { useAgentStateStore, selectSessionExit } from "@/shared/stores/agent-state-store";
import { useCallback, useEffect, useRef, type KeyboardEvent } from "react";

import { ManagedQuestionPanel } from "@/shared/components/managed-question-panel";
import { useOldestPendingAsk } from "@/shared/hooks/use-oldest-pending-ask";
import { isTerminalManagedState } from "@/shared/lib/managed-session";
import {
  selectManagedState,
  useManagedSessionStore,
} from "@/shared/stores/use-managed-session-store";

/** How long the in-place "run ended" note lingers before the row auto-collapses. */
export const CANCELLED_NOTE_MS = 4000;

/** The keys the Agents-list roving-nav consumes on an ancestor — shielded so they
 *  can't be stolen out of the card (arrows out of the answer field, etc.). Other
 *  keys bubble normally so unrelated global shortcuts still fire while a card is open. */
const NAV_KEYS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"]);

/** First keyboard-focusable control within `root`, or null. */
const FOCUSABLE = 'button, textarea, input, [href], [tabindex]:not([tabindex="-1"])';
function firstFocusable(root: HTMLElement | null): HTMLElement | null {
  return root?.querySelector<HTMLElement>(FOCUSABLE) ?? null;
}

/**
 * SW-BA-7 (AC-2/AC-3/AC-4) — the inline card a parked HEADLESS row expands to.
 *
 * A tab-less `run_agent` session (SW-OR-10) has no terminal surface to host the
 * `ask_user` question, so clicking its amber "Needs you" row reveals the REUSED
 * {@link ManagedQuestionPanel} right here, beneath the row — a pull the user chose to
 * service, never a focus-stealing modal.
 *
 * This wrapper owns ONLY what the shared card must not (so the card stays
 * byte-unchanged — AC-5):
 *
 * - **AC-3a/b (keys stay in the card):** the Agents list mounts a roving-tabindex nav
 *   on an ancestor that `preventDefault`s Arrow/Home/End — which would fight typing in
 *   the answer field. We `stopPropagation` exactly those nav keys (and `Escape`) so the
 *   card's keys don't reach it, while other keys still bubble to global shortcuts.
 * - **AC-3c (focus, never a window raise):** on expand, DOM focus moves to the first
 *   control inside the card; on collapse it returns to the row's chip (so a keyboard
 *   user keeps their place); nothing raises/focuses the OS window.
 * - **AC-4 (death cancels in place):** when the run dies while parked the store sheds
 *   the ask, so the card would silently vanish — instead it resolves to a quiet
 *   cancelled note (which takes focus so it's announced), then auto-collapses. When the
 *   run merely resumed (answered → running) it collapses straight away with no note.
 *
 * The answered-vs-died distinction reads the managed session STATE (not the derived
 * chip status), because the store sheds the ask and flips the state in the SAME
 * `set()` — an atomic snapshot — whereas the chip status arrives a render later and
 * would briefly still read `awaiting-answer`, tripping a premature collapse.
 */
export function ParkedQuestionExpansion({
  sessionId,
  onCollapse,
}: {
  sessionId: string;
  onCollapse: () => void;
}): React.JSX.Element {
  const { ask } = useOldestPendingAsk(sessionId);
  const managedState = useManagedSessionStore(selectManagedState(sessionId));
  const exit = useAgentStateStore(selectSessionExit(sessionId));
  const ref = useRef<HTMLDivElement>(null);
  const noteRef = useRef<HTMLParagraphElement>(null);

  // AC-3c: collapse and, IF focus is still inside the card, hand it back to the row's
  // roving-nav chip so a keyboard user never drops to <body>. When focus already
  // moved elsewhere (e.g. a timed auto-collapse the user isn't watching), leave it.
  const collapse = useCallback(() => {
    const root = ref.current;
    const focusInside = !!root && root.contains(document.activeElement);
    const chip = root?.closest("[data-index]")?.querySelector<HTMLElement>("[data-rov]");
    onCollapse();
    if (focusInside) chip?.focus();
  }, [onCollapse]);

  // AC-3c: focus the first control INSIDE the card. On the FIRST open, focus
  // unconditionally (the click that opened it left focus on the row's chip). On a later
  // ask-swap (oldest answered, next queued in — keyed on `correlationId`), refocus ONLY
  // if focus is still inside the card, so a queued ask can never yank focus back from
  // another surface the user moved to. Guarded on `ask` so the cancelled-note state
  // doesn't grab focus here (the note handles its own — below).
  const askId = ask?.correlationId;
  const hasFocusedRef = useRef(false);
  useEffect(() => {
    if (!askId) return;
    const root = ref.current;
    if (!root) return;
    if (!hasFocusedRef.current || root.contains(document.activeElement)) {
      hasFocusedRef.current = true;
      firstFocusable(root)?.focus();
    }
  }, [askId]);

  // AC-4: the ask is gone. Terminal managed state = the run ended → linger on the
  // cancelled note, then collapse. A non-terminal state = it merely resumed (answered) →
  // collapse at once. Move focus to the note ONLY if focus was inside the card (mirror
  // `collapse`'s guard) — a background run dying must not yank focus off wherever the
  // user is; the `role="status"` live region announces the note regardless.
  const ended =
    exit !== undefined || (managedState != null && isTerminalManagedState(managedState));
  useEffect(() => {
    if (ask) return;
    if (!ended) {
      collapse();
      return;
    }
    if (ref.current?.contains(document.activeElement)) noteRef.current?.focus();
    const timer = setTimeout(collapse, CANCELLED_NOTE_MS);
    return () => clearTimeout(timer);
  }, [ask, ended, collapse]);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>): void => {
    if (e.key === "Escape") {
      e.stopPropagation();
      collapse();
      return;
    }
    if (NAV_KEYS.has(e.key)) e.stopPropagation();
  };

  return (
    <div
      ref={ref}
      onKeyDown={onKeyDown}
      // AC-3b: height-capped + internally scrolled, so a long question never pushes the
      // fleet off-screen; a short slide-in softens the reflow instead of snapping. AC-3d:
      // a left amber rail binds the card to its row so a busy list never lets you answer
      // the wrong agent.
      className="ml-1 mt-2 max-h-[33vh] animate-in overflow-auto border-l-2 border-amber-500/40 pl-2 duration-150 fade-in-0 slide-in-from-top-1"
      data-testid="parked-question-expansion"
    >
      {ask ? (
        <>
          {/* AC-2: the stakes/impact hint, when the agent supplied one. Rendered here
              (not in the reused card, which surfaces `category`) so `ManagedQuestionPanel`
              stays byte-unchanged (AC-5). */}
          {ask.stakes?.trim() && (
            <p className="mb-1 text-xs font-medium text-amber-600 dark:text-amber-400">
              {ask.stakes.trim()}
            </p>
          )}
          <ManagedQuestionPanel sessionId={sessionId} />
        </>
      ) : ended ? (
        <p
          ref={noteRef}
          role="status"
          tabIndex={-1}
          className="p-2 text-xs text-muted-foreground outline-none"
        >
          This run ended — your answer wasn&apos;t delivered.
        </p>
      ) : null}
    </div>
  );
}
