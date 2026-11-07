// SPDX-License-Identifier: MIT
import { memo } from "react";

import {
  AGENT_STATUS_WORDS,
  REACH_IT_VERB,
  type AgentStatus,
} from "@/shared/lib/agent-status-config";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import {
  STATUS_PILL_CLASS,
  StatusPill,
  StatusPillContent,
  statusPillStyle,
} from "@/shared/components/status-pill";
import { cn } from "@/lib/utils";

export interface AgentStatusChipProps {
  status: AgentStatus;
  className?: string;
  /**
   * SW-BA-3 (AC-5): when provided, the chip becomes a NAVIGATION control — it
   * renders as a `<button aria-label="Go to {label}">` that jumps to the agent's
   * home surface (SE-6 panel wires it to `onLocate`; a future SE-5 header reuses
   * the same slot). Absent → the chip stays a pure `role="img"` badge, byte-
   * identical to before (zero regression for every existing caller).
   */
  onClick?: () => void;
  /** SW-BA-3 (AC-5): names the navigation target for the button's accessible name
   *  ("Go to {label}"). SW-WA-E9: in badge mode it OVERRIDES the visible chip label
   *  (and its `aria-label`) when the status tone is reused for a different meaning than
   *  its default label — e.g. the Weft Board's PARKED lane wears the quiet `idle`
   *  mark but must read "Parked". Falls back to the status's words when omitted. */
  label?: string;
  /** SW-AM-5 (AC-2): the roving-tabindex value when the chip is the row's nav
   *  control in a keyboard list (0 for the active row, -1 otherwise). Only used
   *  in button mode; omitted → the button's default tab order (byte-identical for
   *  every existing caller). */
  tabIndex?: number;
}

/**
 * SW-BA-1 (AC-1/AC-5): the ONE agent-status affordance — a pure, prop-driven chip
 * rendering a state's colour + label + a screen-reader `aria-label`. Shared so the
 * SE-6 panel and the SE-5 orchestrator header import the identical renderer (no
 * colour drift). Its look is the shared status mark (`markOfAgentStatus`) and its words
 * {@link AGENT_STATUS_WORDS} — no caller picks a status colour.
 *
 * Default (no `onClick`): `role="img"` + `aria-label` exposes the chip as a SINGLE
 * labelled indicator (not an ARIA live region — this is a per-row badge, not a
 * message area), so a screen reader reads "Status: running" once and skips the
 * decorative icon. SW-BA-3 (AC-5): given an `onClick`, it renders the SAME pill as
 * a `<button>` navigation control instead ("Go to {agent}").
 *
 * Non-blocking by construction: no effects, no state, no timers — it renders from
 * props alone (the tinted background is a static `color-mix`). Memoised because it
 * is a pure leaf rendered per-row in a list that re-renders on a 1s "elapsed"
 * clock; memo skips the chip's re-render on every tick when its props are unchanged.
 */
export const AgentStatusChip = memo(function AgentStatusChip({
  status,
  className,
  onClick,
  label,
  tabIndex,
}: AgentStatusChipProps): React.JSX.Element {
  const words = AGENT_STATUS_WORDS[status];
  const mark = markOfAgentStatus(status);

  if (onClick) {
    // AC-5: navigation control. The pill IS the button (never wrapped by a
    // clickable row — that would nest interactive controls); the status colour is
    // still authoritative, and a hover/focus affordance signals it is clickable.
    // The `aria-label` overrides the visible text, so it MUST carry the status too
    // (else a screen-reader user hears "Go to SW-A" and loses the state the sighted
    // user reads off the pill) — review r1.
    const target = label ?? words;
    return (
      <button
        type="button"
        // SW-AV-7: the reach-it verb comes from the ONE shared constant so the
        // Fleet nav chip and the Activity → go affordance can never drift apart.
        aria-label={`${REACH_IT_VERB} to ${target}, ${words.toLowerCase()}`}
        onClick={onClick}
        tabIndex={tabIndex}
        // SW-AM-5 (AC-2): when this chip is a roving-nav control (a `tabIndex` was
        // supplied), mark it `data-rov` so the list's `focusRow` finds it by the
        // shared roving convention (`[data-index] [data-rov]`) rather than a brittle aria-label text match.
        data-rov={tabIndex === undefined ? undefined : ""}
        className={cn(
          STATUS_PILL_CLASS,
          "cursor-pointer transition hover:brightness-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1",
          className,
        )}
        style={statusPillStyle(mark)}
      >
        <StatusPillContent mark={mark} label={words} />
      </button>
    );
  }

  // The default badge is the shared pill (mark and tint owned by `status-pill`). SW-WA-E9:
  // an explicit `label` overrides the visible text + aria-label while keeping the status
  // mark — for a lane that reuses a mark under a different meaning (PARKED wears the
  // quiet `idle` ring but reads "Parked").
  const shown = label ?? words;
  return (
    <StatusPill
      mark={mark}
      label={shown}
      className={className}
      ariaLabel={`Status: ${shown.toLowerCase()}`}
    />
  );
});
