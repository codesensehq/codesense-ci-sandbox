// SPDX-License-Identifier: MIT
import {
  admitsActivityCompletion,
  followsActivityCompletion,
  parseActivityCompletion,
} from "@/shared/lib/activity-completion";
import { hasTurnAfterCompletion } from "@/shared/lib/agent-state";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useViewerStore, type SessionProgress } from "@/shared/stores/viewer-store";

/** One app-root owner for current progress detail; managed task history is separate. */
export function createSessionProgressLifetime() {
  const continued = (id: string) => {
    const agent = useAgentStateStore.getState();
    return hasTurnAfterCompletion(agent.bySession[id], agent.completionSeqBySession[id]);
  };
  const reconcile = () => {
    const viewer = useViewerStore.getState();
    for (const [id, progress] of Object.entries(viewer.progressBySession)) {
      if (progress.settled && continued(id)) viewer.setSessionProgress(id, undefined);
    }
  };
  reconcile();
  const dispose = useAgentStateStore.subscribe((next, prior) => {
    if (
      next.bySession !== prior.bySession ||
      next.completionSeqBySession !== prior.completionSeqBySession
    )
      reconcile();
  });
  return {
    dispose,
    accept(id: string, progress: SessionProgress) {
      const viewer = useViewerStore.getState();
      const completed = useAgentStateStore.getState().completionSeqBySession[id] !== undefined;
      if ((completed || viewer.progressBySession[id]?.settled) && !continued(id)) return;
      viewer.setSessionProgress(id, progress);
    },
    settle(id: string, status: string, summary: string | undefined, boundary: unknown) {
      const agent = useAgentStateStore.getState();
      const completion = parseActivityCompletion(boundary);
      if (!admitsActivityCompletion(completion, agent.completionActivityBySession[id])) return;
      // Use this event's proof, even if the managed listener hasn't seen it yet.
      if (followsActivityCompletion(agent.bySession[id]?.hostTurn, completion)) return;
      const viewer = useViewerStore.getState();
      const phase = status === "needs-attention" ? "blocked" : "done";
      if (summary !== undefined) {
        const prior = viewer.progressBySession[id];
        if (prior?.settled && prior.phase === phase && prior.note === summary) return;
        viewer.setSessionProgress(id, { phase, note: summary, settled: true });
      } else if (viewer.progressBySession[id] && !viewer.progressBySession[id].settled) {
        viewer.setSessionProgress(id, { phase, settled: true });
      }
    },
  };
}
