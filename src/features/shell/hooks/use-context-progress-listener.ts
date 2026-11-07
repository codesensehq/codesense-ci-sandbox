// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import {
  listenAgentPrepareProgress,
  prepareKey,
  type AgentPrepareState,
} from "@/features/agents/lib/agent-prepare-progress";
import { createKeyedDebounce } from "@/shared/lib/keyed-debounce";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { useContextOperationsStore } from "../stores/context-operations-store";

/** Count ticks inside a phase coalesce to one store write per window; a phase
 *  change and a settle land at once. Never per-file noise. */
export const CONTEXT_PROGRESS_THROTTLE_MS = 150;

/** Route one folded state: an explicit operation to the chip's store, a
 *  launch's Step 2 to its workspace's setup line. */
export function routePrepareState(state: AgentPrepareState): void {
  if (state.operationId) useContextOperationsStore.getState().apply(state);
  else useViewerStore.getState().applyAgentPrepareProgress(state);
}

/**
 * SW-XS-10: mount once at the app root. The one consumer of
 * `agent-prepare-progress`: launches' Step 2 drive each workspace's setup line
 * (caption + tray), explicit operations (an `operationId`) drive the pill's
 * operation chip. Replaces the wave-driven surface-sync listener — nothing
 * syncs in the background, so nothing else lights a workspace.
 */
export function useContextProgressListener(): void {
  useEffect(() => {
    const group = createListenerGroup();
    const lastPhase = new Map<string, string>();
    const throttle = createKeyedDebounce<AgentPrepareState>(
      CONTEXT_PROGRESS_THROTTLE_MS,
      (_key, state) => routePrepareState(state),
      { leading: true },
    );
    group.track(
      listenAgentPrepareProgress((state) => {
        const key = prepareKey(state.workspace, state.agent, state.operationId);
        const phaseChanged = lastPhase.get(key) !== state.phase;
        if (state.settled) {
          lastPhase.delete(key);
          throttle.cancel(key);
          routePrepareState(state);
          return;
        }
        lastPhase.set(key, state.phase);
        if (phaseChanged) {
          throttle.cancel(key);
          routePrepareState(state);
        } else {
          throttle.schedule(key, state);
        }
      }),
    );
    return () => {
      throttle.cancelAll();
      group.dispose();
    };
  }, []);
}
