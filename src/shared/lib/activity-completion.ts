// SPDX-License-Identifier: MIT
import type { ActivityCompletion } from "@/tauri/commands";

export function sameActivityIdentity(
  a: ActivityCompletion["identity"],
  b: ActivityCompletion["identity"],
): boolean {
  return (
    a.hostBootId === b.hostBootId &&
    a.sessionIncarnation === b.sessionIncarnation &&
    a.invocationId === b.invocationId
  );
}

/** Receipt order is irrelevant when the owner proves which turn completed. */
export function followsActivityCompletion(
  turn: ActivityCompletion | undefined,
  completion: ActivityCompletion | undefined,
): boolean {
  return (
    !!turn &&
    !!completion &&
    sameActivityIdentity(turn.identity, completion.identity) &&
    turn.turnGeneration > completion.turnGeneration &&
    turn.revision > completion.revision
  );
}

/** Only comparable owner proofs can reject a stale completion. Equal proofs
 * remain admissible so independent listeners can consume the same event. */
export function admitsActivityCompletion(
  incoming: ActivityCompletion | undefined,
  retained: ActivityCompletion | undefined,
): boolean {
  return (
    !incoming ||
    !retained ||
    !sameActivityIdentity(incoming.identity, retained.identity) ||
    incoming.turnGeneration > retained.turnGeneration ||
    (incoming.turnGeneration === retained.turnGeneration && incoming.revision >= retained.revision)
  );
}

export function equalActivityCompletion(
  a: ActivityCompletion | undefined,
  b: ActivityCompletion | undefined,
): boolean {
  return (
    !!a &&
    !!b &&
    sameActivityIdentity(a.identity, b.identity) &&
    a.turnGeneration === b.turnGeneration &&
    a.revision === b.revision
  );
}

/** Older producers omit the boundary; malformed optional metadata has the same fallback. */
export function parseActivityCompletion(value: unknown): ActivityCompletion | undefined {
  if (!value || typeof value !== "object") return;
  const { identity, turnGeneration, revision } = value as Partial<ActivityCompletion>;
  if (!identity || typeof identity !== "object") return;
  if (
    ![identity.hostBootId, identity.sessionIncarnation, identity.invocationId].every(
      (part) => typeof part === "string" && part.trim().length > 0,
    ) ||
    !Number.isSafeInteger(turnGeneration) ||
    (turnGeneration as number) < 0 ||
    !Number.isSafeInteger(revision) ||
    (revision as number) < 0
  )
    return;
  return {
    identity: {
      hostBootId: identity.hostBootId,
      sessionIncarnation: identity.sessionIncarnation,
      invocationId: identity.invocationId,
    },
    turnGeneration: turnGeneration as number,
    revision: revision as number,
  };
}
