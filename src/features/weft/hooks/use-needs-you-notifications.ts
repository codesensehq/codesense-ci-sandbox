// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";

import type { AttentionRowDto } from "@/tauri/commands";
import {
  reportActivityNeedsYou,
  setActivityOwed,
} from "@/features/viewer/lib/attention-alerts-controller";

import { useAttentionBoard } from "./use-attention-board";
import { focusAndRevealBoard } from "./needs-you-notify";

/**
 * SW-WA-E11 — pure de-dup core (AC-5). Given the ids the operator has already been
 * notified about and the current YOU-OWE rows, return the rows that are genuinely
 * NEW (not yet notified) plus the next seen-set. Idempotent on the item id: a
 * re-render or store refresh that re-delivers the same rows yields no new rows.
 *
 * The next seen-set is rebuilt from the CURRENT rows (not accumulated) so a row that
 * leaves YOU-OWE (resolved) is forgotten — if the same id later returns to needs-you,
 * it re-notifies (a genuinely new arrival), never suppressed by a stale memory.
 */
export function selectNewNeedsYou(
  seen: ReadonlySet<number>,
  rows: readonly AttentionRowDto[],
): { newRows: AttentionRowDto[]; nextSeen: Set<number> } {
  const nextSeen = new Set<number>();
  const newRows: AttentionRowDto[] = [];
  for (const row of rows) {
    nextSeen.add(row.id);
    if (!seen.has(row.id)) newRows.push(row);
  }
  return { newRows, nextSeen };
}

/**
 * SW-WA-E11 — report each needs-you (YOU-OWE) item that arrives to the app's one
 * "needs you" stream ({@link reportActivityNeedsYou}), which raises the OS
 * notification only while the window is unfocused, throttled together with the
 * terminal agents' alerts, and lands on the Activity board. Mounted once at app
 * root.
 *
 * Reuses the board's YOU-OWE read ({@link useAttentionBoard}) — no separate source of
 * truth (AC-4). De-duped one-per-arrival on the item id via {@link selectNewNeedsYou}
 * (AC-5). The very first board load SEEDS the seen-set WITHOUT reporting, so pre-existing
 * needs-you items don't all fire at once on mount — only genuine new arrivals do.
 */
export function useNeedsYouNotifications(): void {
  const { board } = useAttentionBoard();
  const seenRef = useRef<Set<number>>(new Set());
  const seededRef = useRef(false);

  useEffect(() => {
    if (!board) return; // not loaded yet
    setActivityOwed(board.youOwe.map((r) => r.id));
    const { newRows, nextSeen } = selectNewNeedsYou(seenRef.current, board.youOwe);
    seenRef.current = nextSeen;
    if (!seededRef.current) {
      seededRef.current = true;
      return;
    }
    if (newRows.length === 0) return;
    // Pass through `need` so the controller can tell a BLOCKING arrival (an agent
    // posting `awaiting-answer` / `needs-approval` / `awaiting-validation` —
    // the human is waiting to be told) from an AMBIENT `needs-attention` post,
    // which stays opt-in. Null `need` keeps the opt-in default (legacy row).
    reportActivityNeedsYou(
      newRows.map((r) => ({ id: r.id, summary: r.summary, need: r.need ?? null })),
      () => void focusAndRevealBoard(),
    );
  }, [board]);
}
