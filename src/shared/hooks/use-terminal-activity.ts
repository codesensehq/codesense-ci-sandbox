// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  getViewerState,
  subscribeViewerState,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import {
  acceptsPrivateAuthority,
  onPrivateAuthorityRetired,
} from "@/shared/lib/private-terminal-authority";
import { agentInputSeq, useAgentStateStore } from "@/shared/stores/agent-state-store";
import {
  TERMINAL_RUNTIME_EVENT,
  TERMINAL_LAYOUT_RESYNC_EVENT,
  terminalActivitySnapshot,
  type TerminalRuntimeEvent,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-activity-source";
import { createActivityRecovery } from "./terminal-activity-recovery";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";

/** Subscribe before reading any host. Each host recovers independently. */
export function useTerminalActivity() {
  useEffect(() => {
    let cancelled = false;
    const recovery = createActivityRecovery({
      layout: () => getViewerState().terminalLayout,
      acceptsAuthority: acceptsPrivateAuthority,
      fetch: terminalActivitySnapshot,
      sequence: agentInputSeq,
      unavailable: (ids, state) => useAgentStateStore.getState().activityUnavailable(ids, state),
      apply: (snapshot, sentAt, receivedAt) => {
        const recorded = useAgentStateStore.getState().recordActivity(snapshot, sentAt, receivedAt);
        // Stored legacy dots must not reappear when authoritative state is idle. A
        // snapshot that has observed nothing is not authoritative: the fallbacks stand.
        if (recorded && (snapshot.observed || snapshot.exit)) {
          getViewerState().setSessionAiStatus(snapshot.session, undefined);
        }
      },
    });
    const cleanup: Array<() => void> = [];
    const group = createListenerGroup();
    const listeners = [
      listen<TerminalRuntimeEvent>(TERMINAL_RUNTIME_EVENT, ({ payload }) => {
        if (!cancelled) recovery.event(payload);
      }),
      listen(TERMINAL_LAYOUT_RESYNC_EVENT, () => {
        if (!cancelled) recovery.resync();
      }),
    ];
    listeners.forEach((pending) => group.track(pending));
    void Promise.all(listeners).then(
      () => {
        if (cancelled) return;
        cleanup.push(subscribeViewerState(() => recovery.reconcile()));
        cleanup.push(onPrivateAuthorityRetired(() => recovery.reconcile()));
        recovery.reconcile();
      },
      () => {
        // Failed subscription cannot be presented as current telemetry.
        recovery.dispose();
        group.dispose();
      },
    );
    return () => {
      cancelled = true;
      recovery.dispose();
      group.dispose();
      cleanup.forEach((stop) => stop());
    };
  }, []);
}
