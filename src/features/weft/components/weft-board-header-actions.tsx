// SPDX-License-Identifier: MIT
import { NO_TEAM_SENTINEL, useActiveTeamKey } from "@/shared/lib/team-key";

import { countNeedsYou } from "../lib/inbox-model";
import { useAttentionBoard } from "../hooks/use-attention-board";
import { useSessionsNeedingYou } from "../hooks/use-sessions-needing-you";

/**
 * SLW-TL-4 — the Inbox header cluster: a single PASSIVE "N need you" count.
 *
 * The old governance toolbar (fleet posture, Arm controller, lease dot) and the
 * Dispatch button + jump control are GONE — the Inbox SHOWS, it does not CONTROL (§6),
 * and the need→action wiring is SLW-TL-7. What remains is a routine count, painted
 * with a NEUTRAL attention color, never the dropped/failure alarm-red (§8.10): "you
 * have items" is not an alarm. Renders nothing at 0, so a quiet Inbox adds no chrome.
 */
export function WeftBoardHeaderActions(): React.JSX.Element | null {
  const { board } = useAttentionBoard();
  const activeTeam = useActiveTeamKey();
  const sessions = useSessionsNeedingYou(activeTeam === NO_TEAM_SENTINEL ? null : activeTeam);
  const needYou = (board ? countNeedsYou(board) : 0) + sessions.length;
  if (needYou <= 0) return null;
  return (
    <span
      data-testid="inbox-needs-you-count"
      aria-label={`${needYou} ${needYou === 1 ? "item needs" : "items need"} you`}
      // NEUTRAL tone (muted), never the alarm-red dropped/failure status color (§8.10).
      className="rounded-full bg-muted px-1.5 py-0.5 text-[0.65rem] font-medium text-muted-foreground"
    >
      {needYou} need you
    </span>
  );
}
