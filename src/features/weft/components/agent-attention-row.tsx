// SPDX-License-Identifier: MIT
import { useState } from "react";

import { MoreHorizontal } from "lucide-react";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";

import { LANE_FALLBACK_LABEL, NEED_LABEL, type InboxItem } from "../lib/inbox-model";
import { INBOX_ACTION_LABEL, resolveInboxActions } from "../lib/inbox-actions";

/**
 * SLW-TL-7 — the ONE Inbox row renderer, keyed by the item's DECLARED need. TL-4 shipped
 * the show-only shape (a neutral need chip + the summary + a quiet 🤖 provenance marker);
 * TL-7 adds the ACTION layer so a row closes its loop INLINE with a plain need→action
 * model and a dead-easy CLI door:
 *  - **Open in CLI** — the always-visible, text-labeled, keyboard-reachable PRIMARY door on
 *    every agent-bearing row, with the CONSTANT label "Open in CLI" (never a morphing/
 *    compound verb — §8.12).
 *  - the need-matched control from a fixed named vocabulary — question → **Reply** (always
 *    literally "Reply", §8.5), gate → **Approve** / **Reject** — all text labels, visible
 *    without hover.
 *  - housekeeping (**Reprioritize**, **Dismiss/Snooze**) in ONE explicit overflow, never
 *    inline icon buttons.
 *
 * Still ONE renderer for every lane (§8 rule 2) — the controls are DERIVED from `need`
 * ({@link resolveInboxActions}), never a forked attention-vs-approval renderer. It invents
 * no transport: the CLI door, the reply routing and the consent path are wired by the panel
 * and passed as `handlers`. With no `handlers` it is the TL-4 show-only row (§6).
 */

/** The row's action wiring — provided by the panel, which owns the reuse seams
 *  (buildForegroundTab / useAgentDrill / useAgentSessionOwnership; the run
 *  consent path; the durable reply). Absent ⇒ the row is show-only. */
export interface AgentAttentionRowHandlers {
  /** Open the item's agent CLI — live run → its live terminal (take the wheel); exited /
   *  headless → its transcript with a Reply box. */
  onOpenInCli: (item: InboxItem) => void;
  /** Approve a gate through the EXISTING run consent path (never a new gate). */
  onApprove: (item: InboxItem) => void;
  /** Reject a gate through the same existing consent path. */
  onReject: (item: InboxItem) => void;
  /** Housekeeping — bump the item's priority. */
  onReprioritize: (item: InboxItem) => void;
  /** Housekeeping — dismiss / snooze the item. */
  onDismiss: (item: InboxItem) => void;
  /** Render the inline reply surface for a question row (the row owns open/close). The
   *  panel supplies the composer that routes live vs. exited under the constant "Reply". */
  renderReply: (item: InboxItem, onClose: () => void) => React.ReactNode;
}

/** Shared class for a text-labeled action button — discoverable, keyboard-scannable, works
 *  on touch (never icon-only, never hover-only). */
const ACTION_BTN =
  "inline-flex shrink-0 items-center rounded border border-border px-1.5 py-0.5 text-[0.7rem] font-medium text-foreground transition-colors hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";

export function AgentAttentionRow({
  item,
  handlers,
}: {
  item: InboxItem;
  handlers?: AgentAttentionRowHandlers;
}): React.JSX.Element {
  const { row, need, lane } = item;
  // The declared need's plain label, or the lane's plain fallback for a legacy /
  // running-agent row that declared none. Never a control-plane word.
  const label = need ? NEED_LABEL[need] : LANE_FALLBACK_LABEL[lane];
  const isAgent = row.initiator === "agent";
  const hasBacklink = typeof row.ref === "string" && row.ref.trim() !== "";

  const actions = resolveInboxActions(item);
  const [replyOpen, setReplyOpen] = useState(false);

  return (
    <div
      data-testid="agent-attention-row"
      data-need={need ?? ""}
      data-lane={lane}
      className="flex flex-col"
    >
      <div className="flex items-start gap-2 px-3 py-1.5 text-xs">
        {isAgent && (
          <span
            aria-label="Posted by an agent"
            title="Posted by an agent"
            className="shrink-0 leading-5"
          >
            🤖
          </span>
        )}
        <span className="min-w-0 flex-1">
          <span className="block text-foreground">{row.summary}</span>
        </span>
        {hasBacklink && (
          // A quiet, non-alarming backlink hint that the item links back to a run/subject.
          <span
            aria-label="Has a backlink"
            title={`Links to ${row.ref}`}
            className="shrink-0 leading-5 text-muted-foreground"
          >
            ↗
          </span>
        )}
        {/* The neutral need chip — plain label, muted tone. NEVER the alarm-red
            dropped/failure color (§8.10): a routine need is not an alarm. */}
        <span
          data-testid="agent-attention-row-need"
          className={cn(
            "shrink-0 rounded-full border border-border bg-muted px-1.5 py-0.5 text-[0.65rem] font-medium text-muted-foreground",
          )}
        >
          {label}
        </span>
      </div>

      {/* SLW-TL-7: the action bar — derived from `need` (no manual pick), all text labels,
          visible without hover. Rendered only when the panel wires the seams. */}
      {handlers && (
        <div
          data-testid="agent-attention-row-actions"
          className="flex flex-wrap items-center gap-1.5 px-3 pb-1.5 pl-3"
        >
          {actions.need.includes("reply") && (
            <button
              type="button"
              className={ACTION_BTN}
              aria-label={INBOX_ACTION_LABEL.reply}
              aria-expanded={replyOpen}
              onClick={() => setReplyOpen((v) => !v)}
            >
              {INBOX_ACTION_LABEL.reply}
            </button>
          )}
          {actions.need.includes("approve") && (
            <button
              type="button"
              className={ACTION_BTN}
              aria-label={INBOX_ACTION_LABEL.approve}
              onClick={() => handlers.onApprove(item)}
            >
              {INBOX_ACTION_LABEL.approve}
            </button>
          )}
          {actions.need.includes("reject") && (
            <button
              type="button"
              className={ACTION_BTN}
              aria-label={INBOX_ACTION_LABEL.reject}
              onClick={() => handlers.onReject(item)}
            >
              {INBOX_ACTION_LABEL.reject}
            </button>
          )}
          {/* The PRIMARY, always-visible CLI door on an agent-bearing row. Constant label. */}
          {actions.primary === "open-in-cli" && (
            <button
              type="button"
              className={cn(ACTION_BTN, "border-primary/40 text-foreground")}
              aria-label={INBOX_ACTION_LABEL["open-in-cli"]}
              onClick={() => handlers.onOpenInCli(item)}
            >
              {INBOX_ACTION_LABEL["open-in-cli"]}
            </button>
          )}
          {/* Housekeeping — ONE explicit overflow, never inline icon buttons. */}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button
                type="button"
                aria-label="More actions"
                className="inline-flex shrink-0 items-center rounded border border-transparent p-0.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                <MoreHorizontal className="size-4" aria-hidden />
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => handlers.onReprioritize(item)}>
                {INBOX_ACTION_LABEL.reprioritize}
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={() => handlers.onDismiss(item)}>
                {INBOX_ACTION_LABEL.dismiss}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      )}

      {/* SLW-TL-7: the inline reply surface — the durable "Reply" (§2.3). The panel's
          composer routes live (types into the waiting agent) vs. exited (re-dispatches a
          continuation) under the SAME constant label, and always acknowledges it landed. */}
      {handlers && replyOpen && handlers.renderReply(item, () => setReplyOpen(false))}
    </div>
  );
}
