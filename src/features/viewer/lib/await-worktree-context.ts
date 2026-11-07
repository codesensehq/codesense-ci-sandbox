// SPDX-License-Identifier: MIT
/** Story readiness is registration and repository setup. Native named-agent
 * startup prepares only the agent's declared context before process spawn. */
import { awaitWorktreeEstablished } from "@/shared/lib/worktree/pending-establish";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

export const CONTEXT_LAUNCH_WAIT_MS = 30_000;
export type WorktreeContextOutcome =
  | "not-needed"
  | "landed"
  | "failed"
  | "stopped"
  | "closed"
  | "timeout";

function findStoryWorkspace(cwd: string): Workspace | undefined {
  return Object.values(useViewerStore.getState().workspaces)
    .flat()
    .find((w) => w.cwd === cwd && w.kind === "story");
}

export async function awaitWorktreeContext(
  cwd: string,
  capMs = CONTEXT_LAUNCH_WAIT_MS,
): Promise<WorktreeContextOutcome> {
  const initial = findStoryWorkspace(cwd);
  await awaitWorktreeEstablished(cwd, capMs);
  const snapshot = (): WorktreeContextOutcome | undefined => {
    const ws = findStoryWorkspace(cwd);
    if (!ws) return initial ? "closed" : "not-needed";
    if (ws.status === "error" || ws.backgroundSetup?.status === "failed") return "failed";
    if (ws.status === "creating" || ws.backgroundSetup) return undefined;
    return "landed";
  };
  const first = snapshot();
  if (first) return first;
  return new Promise((resolve) => {
    let finished = false;
    const finish = (outcome: WorktreeContextOutcome) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      unsubscribe();
      resolve(outcome);
    };
    const timer = setTimeout(() => finish("timeout"), capMs);
    const check = () => {
      const outcome = snapshot();
      if (outcome) finish(outcome);
    };
    const unsubscribe = useViewerStore.subscribe(check);
    check();
  });
}

export function contextMissingMessage(outcome: WorktreeContextOutcome): string {
  return `Agent launch blocked: workspace registration or setup is ${outcome}. Complete setup and retry.`;
}

export async function awaitContextForLaunch(
  cwd: string,
  wait: (cwd: string) => Promise<WorktreeContextOutcome> = awaitWorktreeContext,
): Promise<void> {
  const outcome = await wait(cwd);
  if (outcome === "landed" || outcome === "not-needed") return;
  throw new Error(contextMissingMessage(outcome));
}
