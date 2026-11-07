// SPDX-License-Identifier: MIT
import { logger } from "@/shared/lib/logger";
import type { RelaunchResult } from "@/shared/lib/agent-session-launcher";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";

export type RestartAction = "Restart" | "Relaunch";

/**
 * Run a restart or relaunch of `sessionId` — one at a time per session, from
 * whichever surface asks (the pane's ended bar, the tray's Hidden-tab menu):
 * a second while one is in flight does nothing. Its state (in flight, or failed
 * and why) is kept per session, so it outlives the pane a respawn remounts.
 * Returns whether it started; `onFail` hears a failure's reason.
 */
export function runRestart(
  sessionId: string,
  action: RestartAction,
  go: () => Promise<RelaunchResult>,
  onFail?: (reason: string) => void,
): boolean {
  const store = useAgentStateStore.getState();
  if (store.relaunchBySession[sessionId]?.busy) return false;
  store.setRelaunch(sessionId, { busy: true, failure: null });
  const fail = (reason: string) => {
    logger.warn("pty", `${action.toLowerCase()} failed: session_id=${sessionId}: ${reason}`);
    useAgentStateStore
      .getState()
      .setRelaunch(sessionId, { busy: false, failure: { action, reason } });
    onFail?.(reason);
  };
  go()
    .then((result) => {
      if (result.ok) useAgentStateStore.getState().setRelaunch(sessionId, null);
      else fail(result.reason);
    })
    .catch((e: unknown) => fail(e instanceof Error ? e.message : String(e)));
  return true;
}
