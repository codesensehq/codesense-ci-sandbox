// SPDX-License-Identifier: MIT
export type ReviewOpenPhase =
  | "opening"
  | "reconciling"
  | "skipping-reconcile"
  | "launching"
  | "joining"
  | "reviewing";

type Listener = (phase: ReviewOpenPhase) => void;
const listeners = new Map<string, Set<Listener>>();
const latest = new Map<string, ReviewOpenPhase>();

export function publishReviewOpenPhase(
  operationId: string | undefined,
  phase: ReviewOpenPhase,
): void {
  if (!operationId) return;
  latest.set(operationId, phase);
  for (const listener of listeners.get(operationId) ?? []) listener(phase);
}

export function subscribeReviewOpenPhase(operationId: string, listener: Listener): () => void {
  const current = listeners.get(operationId) ?? new Set<Listener>();
  current.add(listener);
  listeners.set(operationId, current);
  const phase = latest.get(operationId);
  if (phase) listener(phase);
  return () => {
    current.delete(listener);
    if (current.size === 0) {
      listeners.delete(operationId);
      latest.delete(operationId);
    }
  };
}
