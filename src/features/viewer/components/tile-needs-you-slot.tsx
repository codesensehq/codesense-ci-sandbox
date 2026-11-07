// SPDX-License-Identifier: MIT
import { StatusMark } from "@/shared/components/status-mark";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";

import { ManagedQuestionPanel } from "@/shared/components/managed-question-panel";
import { byOldestAsk, useOldestPendingAsk } from "@/shared/hooks/use-oldest-pending-ask";
import type { AskUserRequest } from "@/shared/lib/managed-session";
import {
  selectPendingAsks,
  useManagedSessionStore,
} from "@/shared/stores/use-managed-session-store";
import type { TerminalTab } from "@/shared/stores/viewer-store";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { focusWorkspaceTerminal } from "../lib/focus-cascade";
import {
  TILE_QUESTION_ASK_ATTR,
  TILE_QUESTION_ATTR,
  firstQuestionControl,
  focusTileFrame,
} from "../lib/tile-question-focus";

/**
 * The tile's single "the agent needs you" slot: a strip between the tile chrome
 * and its terminal that shows the pending `ask_user` of an agent running in one of
 * the tile's tabs, answered through the shared {@link ManagedQuestionPanel} (the
 * same pending-question store and answer path as the Agents panel, so answering
 * in either place clears both).
 *
 * It is a flex sibling of the terminal body, not an overlay, so the terminal
 * shrinks to make room and no terminal row is ever covered.
 *
 * Keyboard: the strip never takes focus by itself (a question arriving while you
 * type must not swallow your keystrokes). It shows the "Answer Agent Question"
 * chord; that chord or a click moves focus in. Once in: 1–9 pick a choice, arrows
 * move between choices, Enter answers (Shift+Enter is a newline in a free-text
 * answer), Esc goes back to the terminal without answering. Answering here sends
 * focus back to the terminal; a question withdrawn under you (or answered
 * elsewhere) keeps focus in the tile, with a brief note saying why.
 *
 * The slot is a pure view over the pending-question store: whatever removes the
 * ask from the store clears it (an answer from any surface, the agent's call
 * ending unanswered via `mcp:ask-user-closed`, the session dying). An
 * approve/deny prompt for the same agent belongs in this slot too, rather than in
 * a second in-tile surface.
 */
export function TileNeedsYouSlot({
  workspaceId,
  tabs,
  activeTabId,
  onShowTab,
}: {
  workspaceId: string;
  tabs: readonly TerminalTab[];
  activeTabId: string | undefined;
  /** Bring a (background) tab of this tile to the front. */
  onShowTab: (tabId: string) => void;
}): React.JSX.Element | null {
  const pendingAsks = useManagedSessionStore(selectPendingAsks);
  const target = useMemo(
    () => pickAskingTab(tabs, activeTabId, pendingAsks),
    [tabs, activeTabId, pendingAsks],
  );
  const { ask } = useOldestPendingAsk(target?.tab.sessionId ?? "");
  const askId = target ? (ask?.correlationId ?? null) : null;

  const chord = formatKeystrokeDisplay(useEffectiveShortcut("viewer.focus-tile-question"));
  const rootRef = useRef<HTMLDivElement>(null);
  const focusInsideRef = useRef(false);
  // The question the user is focused in (drives the chord hint only).
  const [focusedAskId, setFocusedAskId] = useState<string | null>(null);
  const prevAskIdRef = useRef<string | null>(null);
  // Why the shown question is about to change: this strip sent its answer, or the
  // user asked for the next waiting tab. Anything else (withdrawn by the agent,
  // answered in the Agents panel, the run died) happened TO the user.
  const submittedRef = useRef(false);
  const navigatingRef = useRef(false);
  const [withdrawn, setWithdrawn] = useState(false);

  // Focus never moves INTO the strip on its own. When the shown question changes
  // while you are in the strip:
  // - you answered it here → back to the terminal;
  // - you asked for the next tab's question → into that question;
  // - it went away under you → focus stays in the tile (the strip, or the tile
  //   frame if nothing is left) — never the terminal input, where the rest of what
  //   you were typing would reach the agent's live prompt — with a brief note.
  useEffect(() => {
    const prev = prevAskIdRef.current;
    prevAskIdRef.current = askId;
    const submitted = submittedRef.current;
    const navigating = navigatingRef.current;
    submittedRef.current = false;
    navigatingRef.current = false;
    if (askId === prev || !focusInsideRef.current) return;
    const root = rootRef.current;
    if (submitted) {
      focusInsideRef.current = false;
      focusWorkspaceTerminal(workspaceId);
      return;
    }
    if (navigating && askId && root) {
      const focusNext = () => {
        if (root.isConnected && !root.contains(document.activeElement)) {
          firstQuestionControl(root)?.focus();
        }
      };
      focusNext();
      // Switching tabs can reveal (and focus) that tab's terminal a frame later.
      requestAnimationFrame(focusNext);
      return;
    }
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reacting to the store dropping the question under the user
    setWithdrawn(true);
    if (askId && root) root.focus({ preventScroll: true });
    else {
      focusInsideRef.current = false;
      focusTileFrame(workspaceId);
    }
  }, [askId, workspaceId]);

  useEffect(() => {
    if (!withdrawn) return;
    const timer = setTimeout(() => setWithdrawn(false), WITHDRAWN_NOTE_MS);
    return () => clearTimeout(timer);
  }, [withdrawn]);

  const withdrawnNote = withdrawn ? (
    <p
      role="status"
      className="text-xs text-muted-foreground"
      data-testid="tile-question-withdrawn"
    >
      Question withdrawn — the agent no longer needs this answer.
    </p>
  ) : null;

  if (!target || !ask) {
    return withdrawnNote ? (
      <div
        className="shrink-0 border-b border-l-2 border-b-amber-500/30 border-l-amber-500/50 px-2.5 py-1.5"
        data-testid={`tile-question-${workspaceId}-note`}
      >
        {withdrawnNote}
      </div>
    ) : null;
  }

  const showTab = (tabId: string): void => {
    navigatingRef.current = true;
    onShowTab(tabId);
  };

  const backToTerminal = (): void => {
    focusInsideRef.current = false;
    setFocusedAskId(null);
    focusWorkspaceTerminal(workspaceId);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>): void => {
    const root = rootRef.current;
    if (!root || e.nativeEvent.isComposing) return;
    const el = e.target as HTMLElement;
    const inText = el.tagName === "TEXTAREA" || el.tagName === "INPUT";
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      backToTerminal();
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    // Focus parked on the strip itself (after a question was withdrawn under you):
    // keys you were still typing must not answer the next question.
    if (el === root && ARROW_STEP[e.key] === undefined) return;
    if (inText) {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        e.stopPropagation();
        root.querySelector<HTMLButtonElement>('[data-testid="ask-send"]:not([disabled])')?.click();
      }
      return;
    }
    const options = [...root.querySelectorAll<HTMLElement>('[data-testid="ask-option"]')];
    if (options.length === 0) return;
    if (/^[1-9]$/.test(e.key)) {
      const opt = options[Number(e.key) - 1];
      if (opt) {
        e.preventDefault();
        e.stopPropagation();
        opt.click();
      }
      return;
    }
    const step = ARROW_STEP[e.key];
    if (step !== undefined) {
      e.preventDefault();
      e.stopPropagation();
      const at = options.indexOf(el);
      options[at === -1 ? 0 : (at + step + options.length) % options.length]?.focus();
    }
  };

  const optionCount = ask.options?.length ?? 0;
  const keyHint =
    optionCount > 0
      ? `${optionCount > 1 ? `1–${Math.min(optionCount, 9)}` : "1"} or ←/→ to choose · Enter answers · Esc back to terminal`
      : "Enter sends · Shift+Enter new line · Esc back to terminal";
  const isBackgroundTab = target.tab.id !== activeTabId;

  return (
    <div
      ref={rootRef}
      {...{ [TILE_QUESTION_ATTR]: "", [TILE_QUESTION_ASK_ATTR]: ask.correlationId }}
      role="region"
      aria-label={`Question from ${target.tab.name}`}
      data-testid={`tile-question-${workspaceId}`}
      data-session-id={target.tab.sessionId}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      onClickCapture={(e) => {
        const button = (e.target as HTMLElement).closest("button");
        if (button?.closest('[data-testid="managed-question-panel"]')) submittedRef.current = true;
      }}
      onFocus={() => {
        focusInsideRef.current = true;
        setFocusedAskId(askId);
      }}
      onBlur={(e) => {
        // A null relatedTarget is the focused control being removed on answer (or
        // the window losing focus) — keep the flag so focus can go home.
        const next = e.relatedTarget as Node | null;
        if (next && !e.currentTarget.contains(next)) {
          focusInsideRef.current = false;
          setFocusedAskId(null);
        }
      }}
      // The tile is dark whatever the app theme, so the question reads on dark tokens too.
      data-surface="terminal"
      className="text-foreground flex max-h-[40%] shrink-0 outline-none animate-in flex-col gap-1.5 overflow-auto border-b border-l-2 border-b-amber-500/30 border-l-amber-500 bg-[color-mix(in_oklab,var(--cs-status-needs)_7%,var(--background))] px-2.5 py-2 duration-150 fade-in-0 slide-in-from-top-1"
    >
      <div className="flex items-center gap-2 text-xs" aria-live="polite">
        <StatusMark mark="question" size="md" />
        <span className="min-w-0 truncate font-medium text-amber-600 dark:text-amber-400">
          {target.tab.name} is waiting for your answer
        </span>
        {target.moreCount > 0 &&
          (target.nextTab ? (
            <button
              type="button"
              className="shrink-0 rounded px-1 text-muted-foreground underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-1 focus-visible:outline-amber-500"
              onClick={() => showTab(target.nextTab!.id)}
              title={`Show ${target.nextTab.name}`}
              data-testid="tile-question-more"
            >
              +{target.moreCount} more
            </button>
          ) : (
            <span className="shrink-0 text-muted-foreground" data-testid="tile-question-more">
              +{target.moreCount} more
            </span>
          ))}
        <span className="ml-auto flex shrink-0 items-center gap-2 text-muted-foreground">
          {isBackgroundTab && (
            <button
              type="button"
              className="rounded px-1 underline-offset-2 hover:underline focus-visible:outline focus-visible:outline-1 focus-visible:outline-amber-500"
              onClick={() => showTab(target.tab.id)}
              data-testid="tile-question-show-tab"
            >
              Show tab
            </button>
          )}
          {focusedAskId !== askId && chord && (
            <span data-testid="tile-question-chord">
              <kbd className="rounded border border-border px-1 font-mono text-[10px]">{chord}</kbd>{" "}
              to answer
            </span>
          )}
        </span>
      </div>
      {withdrawnNote}
      {ask.stakes?.trim() && (
        <p className="text-xs font-medium text-amber-600 dark:text-amber-400">
          {ask.stakes.trim()}
        </p>
      )}
      <ManagedQuestionPanel sessionId={target.tab.sessionId} showPendingCount={false} />
      <p className="text-[11px] text-muted-foreground" data-testid="tile-question-keys">
        {keyHint}
      </p>
    </div>
  );
}

/** How long the "Question withdrawn" note stays up. */
export const WITHDRAWN_NOTE_MS = 3000;

const ARROW_STEP: Record<string, number | undefined> = {
  ArrowRight: 1,
  ArrowDown: 1,
  ArrowLeft: -1,
  ArrowUp: -1,
};

/**
 * Which of the tile's tabs the strip shows: the front tab when its agent is
 * asking, otherwise the tab whose question has waited longest. `moreCount` is
 * every other pending question in the tile (the same tab's queue and other
 * tabs'); `nextTab` is the longest-waiting OTHER asking tab, if any.
 */
export function pickAskingTab(
  tabs: readonly TerminalTab[],
  activeTabId: string | undefined,
  pendingAsks: Record<string, AskUserRequest>,
): { tab: TerminalTab; moreCount: number; nextTab: TerminalTab | null } | null {
  const bySession = new Map<string, AskUserRequest[]>();
  for (const a of Object.values(pendingAsks)) {
    const list = bySession.get(a.sessionId);
    if (list) list.push(a);
    else bySession.set(a.sessionId, [a]);
  }
  const oldestOf = (t: TerminalTab): AskUserRequest =>
    [...bySession.get(t.sessionId)!].sort(byOldestAsk)[0]!;
  const asking = tabs
    .filter((t) => bySession.has(t.sessionId))
    .sort((x, y) => byOldestAsk(oldestOf(x), oldestOf(y)));
  if (asking.length === 0) return null;
  const tab = asking.find((t) => t.id === activeTabId) ?? asking[0]!;
  const total = asking.reduce((n, t) => n + bySession.get(t.sessionId)!.length, 0);
  return {
    tab,
    moreCount: total - 1,
    nextTab: asking.find((t) => t.id !== tab.id) ?? null,
  };
}
