// SPDX-License-Identifier: MIT
import { Loader2 } from "lucide-react";
import { useEffect, useState } from "react";

import { AgentStatusChip } from "@/shared/components/agent-status-chip";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import type { DoneStatus } from "@/shared/lib/managed-session";
import type { RunAuditHeader } from "@/shared/lib/run-evidence";
import { wvRatify, wvRevoke, type WvPendingAssertionDto } from "@/tauri/commands";

/**
 * SW-WV-F4 (AC-1, AC-5, AC-6) — the single action header above the run-detail tabs.
 *
 * Appears ONLY when the run has pending assertions to judge (AC-1): the operator's
 * verdict chip + the worker's self-verdict sentence + Ratify / Send-back. When there
 * is nothing to judge it renders nothing — no blank bar, no dead buttons (AD-10). One
 * judgment at a time: it targets the FIRST pending assertion; multi-assertion
 * aggregation is SW-WV-F5.
 *
 * Ratify (AC-5) → `wvRatify`; Send-back (AC-6) → `wvRevoke` with an optional inline
 * note (max 280 chars) routed to the worker session by the Rust handler. Both actions
 * enter a spinner/disabled pending state; on resolution the assertion leaves `pending`
 * and the live hook drops it, so the header disappears (never a stale header).
 */
export interface WeftReviewHeaderProps {
  assertions: WvPendingAssertionDto[];
  /** The already-fetched audit header (status + self-verdict summary), or `null`. */
  header: RunAuditHeader | null;
}

/** Max length of the inline send-back note (AC-6). */
const NOTE_MAX = 280;

/**
 * Map the worker's `DoneStatus` verdict to the shared `AgentStatus` chip vocabulary
 * (matches the sessions projection): a verified `success` reads as the celebrated
 * `completed`; `failed` stays `failed`; `needs-attention` maps to `dropped` (the
 * `AgentStatus` for the alerting "needs attention" outcome — there is no
 * `needs-attention` `AgentStatus`).
 */
function mapDoneStatusToAgentStatus(status: DoneStatus): AgentStatus {
  switch (status) {
    case "success":
      return "completed";
    case "failed":
      return "failed";
    case "needs-attention":
      return "dropped";
  }
}

export function WeftReviewHeader({
  assertions,
  header,
}: WeftReviewHeaderProps): React.JSX.Element | null {
  const [pending, setPending] = useState(false);
  const [noteOpen, setNoteOpen] = useState(false);
  const [noteText, setNoteText] = useState("");
  const [sentBack, setSentBack] = useState(false);

  // One judgment at a time (AC): act on the first pending assertion. (`undefined` when
  // the list is empty — guarded before any use below.)
  const target = assertions[0];

  // Reset the per-assertion note/toast state whenever the judged assertion changes, so a
  // second pending assertion never inherits the first's stale note text or "Sent back"
  // badge. Keyed on the target id (stable across re-renders for the same assertion).
  const targetId = target?.id ?? null;
  useEffect(() => {
    setNoteOpen(false);
    setNoteText("");
    setSentBack(false);
    setPending(false);
  }, [targetId]);

  // AC-1: nothing to judge → render nothing (no blank bar). Also covers the still-live
  // case (a non-terminal run has no pending assertion yet). Placed after the hooks so
  // hook order stays stable across renders.
  if (!target) return null;

  const onRatify = () => {
    setPending(true);
    // On success the live hook (the committed assertion removal) drops the row, so
    // the header disappears. On failure the `.catch` re-enables the buttons and leaves the
    // header mounted (the assertion is still pending) — never an unhandled rejection.
    void wvRatify(target.id)
      .catch(() => {})
      .finally(() => setPending(false));
  };

  const onSendBackClick = () => {
    // First click reveals the inline note field; Confirm commits, Cancel collapses.
    setNoteOpen(true);
  };

  const onConfirmSendBack = () => {
    setPending(true);
    setNoteOpen(false);
    const note = noteText.trim() ? noteText.trim() : undefined;
    // Only show the "Sent back" confirmation once the revoke actually commits — a failed
    // IPC must not leave a false toast (the assertion stays pending; the header remains).
    void wvRevoke(target.id, note)
      .then(() => setSentBack(true))
      .catch(() => {})
      .finally(() => setPending(false));
  };

  const onCancelSendBack = () => {
    setNoteOpen(false);
    setNoteText("");
  };

  return (
    <div
      data-testid="weft-review-header"
      className="flex flex-col gap-2 border-b border-border bg-muted/30 px-3 py-2"
    >
      <div className="flex items-center gap-2">
        {header?.status && <AgentStatusChip status={mapDoneStatusToAgentStatus(header.status)} />}
        {header?.summary && (
          <p className="flex-1 truncate text-sm" title={header.summary}>
            {header.summary}
          </p>
        )}
        {!header?.summary && <span className="flex-1" />}
        {sentBack && (
          <span data-testid="weft-review-sent-back" className="text-xs text-muted-foreground">
            Sent back
          </span>
        )}
        <button
          type="button"
          data-testid="weft-review-ratify"
          onClick={onRatify}
          disabled={pending}
          className="inline-flex shrink-0 items-center gap-1 rounded bg-primary px-2.5 py-1 text-xs font-medium text-primary-foreground transition hover:opacity-90 disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {pending && (
            <Loader2
              data-testid="weft-review-spinner"
              size={12}
              className="animate-spin"
              aria-hidden
            />
          )}
          Ratify
        </button>
        <button
          type="button"
          data-testid="weft-review-send-back"
          onClick={noteOpen ? onConfirmSendBack : onSendBackClick}
          disabled={pending}
          className="inline-flex shrink-0 items-center rounded px-2.5 py-1 text-xs font-medium text-muted-foreground transition hover:text-foreground disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {noteOpen ? "Confirm" : "Send back"}
        </button>
        {noteOpen && (
          <button
            type="button"
            data-testid="weft-review-cancel"
            onClick={onCancelSendBack}
            disabled={pending}
            className="inline-flex shrink-0 items-center rounded px-2.5 py-1 text-xs text-muted-foreground transition hover:text-foreground disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            Cancel
          </button>
        )}
      </div>
      {noteOpen && (
        <textarea
          data-testid="weft-review-note"
          value={noteText}
          onChange={(e) => setNoteText(e.target.value.slice(0, NOTE_MAX))}
          maxLength={NOTE_MAX}
          rows={2}
          placeholder="Optional note to route back to the worker…"
          className="w-full resize-none rounded border border-border bg-background px-2 py-1 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        />
      )}
    </div>
  );
}
