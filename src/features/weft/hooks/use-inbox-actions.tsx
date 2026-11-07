// SPDX-License-Identifier: MIT
import { useCallback, useMemo } from "react";

import { openAgentResult } from "@/features/viewer/components/agent-result-panel";
import { useAgentDrill } from "@/features/viewer/hooks/use-agent-drill";
import {
  findTabLocationBySessionId,
  selectRunningTerminalTabIndex,
  useViewerStore,
} from "@/shared/stores/viewer-store";
import { logger } from "@/shared/lib/logger";
import { wvSettleApproval } from "@/tauri/commands";

import { agentSessionFromRow } from "../lib/agent-session-from-row";
import { routeAttentionRow } from "../components/weft-attention-row";
import { useInboxTriageStore } from "../stores/inbox-triage-store";
import type { InboxItem } from "../lib/inbox-model";
import type { AgentAttentionRowHandlers } from "../components/agent-attention-row";
import { InboxReplyMount } from "../components/inbox-reply-mount";

/**
 * SLW-TL-7 — the Inbox row's action wiring. It reuses the shipped response mechanisms and
 * invents no new transport (§2.3, §6): the CLI door is `useAgentDrill`
 * (`onBringToForeground` take-the-wheel) / the run transcript (`openAgentResult`); the
 * consent path is `wvSettleApproval` (NOT a new gate); the durable reply routes live
 * (`sendTextToSession`) vs. exited (a continuation re-dispatch) inside {@link InboxReplyMount}.
 * Housekeeping is per-viewer triage (dismiss / pin), never a durable board mutation.
 *
 * @param teamKey the right-pane host's team (for the drill), or `null`.
 * @param reload  re-read the board so a settled item clears (the acknowledgement).
 */
export function useInboxActions(
  teamKey: string | null,
  reload: () => void,
): AgentAttentionRowHandlers {
  const { onBringToForeground } = useAgentDrill(teamKey);
  const dismiss = useInboxTriageStore((s) => s.dismiss);
  const pin = useInboxTriageStore((s) => s.pin);

  // Open the item's agent CLI: a live, tab-reachable run → drop into its live terminal and
  // drive it (take the wheel, via the SAME drill the fleet uses); an exited / headless run
  // → open its transcript (read what happened; the Reply box is inline on the row). Never a
  // dead-end — an unresolved handle routes to where the row otherwise resolves.
  const onOpenInCli = useCallback(
    (item: InboxItem) => {
      const liveId = item.row.liveSessionId ?? null;
      const reachable =
        !!liveId &&
        !!findTabLocationBySessionId(
          selectRunningTerminalTabIndex(useViewerStore.getState()),
          liveId,
        );
      if (liveId && reachable) {
        onBringToForeground(agentSessionFromRow(liveId));
        return;
      }
      // Exited / headless: open the run's transcript when a run id resolves (`source="run"`
      // → `ref`), else route to where the row otherwise resolves.
      const runId = item.row.source === "run" && item.row.ref ? item.row.ref : null;
      if (runId) {
        openAgentResult({ run_id: runId, story_id: null });
        return;
      }
      routeAttentionRow(item.row);
    },
    [onBringToForeground],
  );

  // Approve / Reject through the EXISTING weft consent path (`wv_settle_approval`) — the
  // human settles the pending decision with their own hands; CodeSense adds no gate (§6).
  // The board reload is the acknowledgement (the settled item clears).
  const settle = useCallback(
    (item: InboxItem, disposition: "ratified" | "revoked") => {
      void wvSettleApproval(item.row.id, disposition)
        .then(() => reload())
        .catch((e) =>
          logger.warn("inbox", `settle ${disposition} failed for ${item.row.id}: ${String(e)}`),
        );
    },
    [reload],
  );
  const onApprove = useCallback((item: InboxItem) => settle(item, "ratified"), [settle]);
  const onReject = useCallback((item: InboxItem) => settle(item, "revoked"), [settle]);

  // The exited continuation. A FAITHFUL continuation must re-dispatch the automation as a
  // fresh run that CARRIES the reply + original context as input — that backend command is
  // SLW-TL-11 (SEAM below). Until it lands we do the honest thing for EVERY exited row,
  // automation-sourced or not: we NEVER fire a reply-less run (a bare re-fire drops the
  // human's answer AND can re-trigger real side effects — re-posting to Slack, etc.), and we
  // NEVER claim a delivery that didn't happen. We reject so the composer withholds its "Sent"
  // confirmation and the human continues in the CLI (Open-in-CLI is on the row — Reply is
  // only offered on an agent-bearing row, which always carries that door).
  const onRedispatch = useCallback(
    // TODO(SLW-TL-11): replace this rejection with the reply-carrying continuation dispatch
    // (`run_id` + reply + original context → a real continuation run), then acknowledge.
    async (_item: InboxItem, _reply: string): Promise<void> => {
      throw new Error("this run has exited — open it in the CLI to continue");
    },
    [],
  );

  const onReprioritize = useCallback((item: InboxItem) => pin(item.key), [pin]);
  const onDismiss = useCallback((item: InboxItem) => dismiss(item.key), [dismiss]);

  const renderReply = useCallback(
    (item: InboxItem, onClose: () => void) => (
      <InboxReplyMount
        item={item}
        onRedispatch={onRedispatch}
        onClose={onClose}
        // A landed reply clears the item (the board reload) — the acknowledgement (§8.14).
        onSent={reload}
      />
    ),
    [onRedispatch, reload],
  );

  return useMemo(
    () => ({ onOpenInCli, onApprove, onReject, onReprioritize, onDismiss, renderReply }),
    [onOpenInCli, onApprove, onReject, onReprioritize, onDismiss, renderReply],
  );
}
