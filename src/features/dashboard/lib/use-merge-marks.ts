// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useOwnsScope } from "@/shared/lib/warm-scopes";
import { toast } from "sonner";
import {
  clearContentMergeMark,
  listContentMergeMarks,
  setContentMergeMark,
} from "@/tauri/commands";

/**
 * SW-CP-27: the merge-mark read + toggle seam for the content tree. Loads the
 * team's marked content paths ONCE per (orgRoot, team) into a set that drives the
 * in-tree merge badge and the Mark/Unmark menu label, and toggles a mark
 * optimistically (swap-on-success / revert-on-error).
 *
 * Deliberately does NOT force a projection wave after a toggle: re-materializing
 * the marked file is the engine's job (CP-24 next-wave / CP-26's `config/` watch),
 * so a toggle issues exactly one seam call and never blocks the UI thread.
 */
const EMPTY: ReadonlySet<string> = new Set();

interface PendingMergeOperation {
  identity: string;
  path: string;
  desired: boolean;
  version: number;
  settled: boolean;
}

export interface MergeMarks {
  paths: ReadonlySet<string>;
  /** The first read for the current (orgRoot, team) has settled; `paths` is not a loading placeholder. */
  ready: boolean;
  /** Is the content-relative `logicalPath` currently merge-marked? */
  isMarked: (logicalPath: string) => boolean;
  /** Toggle the mark optimistically; revert + toast on error. */
  toggle: (logicalPath: string) => Promise<void>;
}

function withToggled(
  set: ReadonlySet<string>,
  path: string,
  present: boolean,
): ReadonlySet<string> {
  const next = new Set(set);
  if (present) next.add(path);
  else next.delete(path);
  return next;
}

function withPendingMarks(
  base: ReadonlySet<string>,
  identity: string,
  operations: Iterable<PendingMergeOperation>,
): ReadonlySet<string> {
  let next = base;
  for (const pending of operations) {
    if (pending.identity === identity) {
      next = withToggled(next, pending.path, pending.desired);
    }
  }
  return next;
}

export function useMergeMarks(orgRoot: string, team: string): MergeMarks {
  const owns = useOwnsScope();
  const identity = `${orgRoot}\0${team}`;
  const [loaded, setLoaded] = useState<{ identity: string; marks: ReadonlySet<string> }>(() => ({
    identity,
    marks: EMPTY,
  }));
  const marked = loaded.identity === identity ? loaded.marks : EMPTY;
  const [readyIdentity, setReadyIdentity] = useState("");
  const optimisticRef = useRef(loaded);
  const activeIdentityRef = useRef(identity);
  const loadGenerationRef = useRef(0);
  const activeLoadsRef = useRef(new Map<string, number>());
  const confirmedMarksRef = useRef(new Map<string, ReadonlySet<string>>());
  const confirmedDuringLoadRef = useRef(new Map<string, Map<string, boolean>>());
  const operationQueuesRef = useRef(new Map<string, Promise<void>>());
  const pendingOperationsRef = useRef(new Map<string, PendingMergeOperation>());

  // Load once per (orgRoot, team). Fail-open: a merge badge must never break the
  // nav render, so no active team / a failed read yields no marks.
  useEffect(() => {
    activeIdentityRef.current = identity;
    let cancelled = false;
    // A hidden team's nav loads nothing; it loads when it is shown again.
    if (!orgRoot || !team || !owns) return;
    const loadGeneration = ++loadGenerationRef.current;
    const activeLoads = activeLoadsRef.current;
    const pendingOperations = pendingOperationsRef.current;
    const confirmedDuringLoad = confirmedDuringLoadRef.current;
    activeLoads.set(identity, loadGeneration);
    const publish = (paths: string[]) => {
      const confirmed = new Set(paths);
      for (const [path, desired] of confirmedDuringLoad.get(identity) ?? []) {
        if (desired) confirmed.add(path);
        else confirmed.delete(path);
      }
      for (const pending of pendingOperations.values()) {
        if (pending.identity === identity && pending.settled) {
          if (pending.desired) confirmed.add(pending.path);
          else confirmed.delete(pending.path);
        }
      }
      const next = withPendingMarks(confirmed, identity, pendingOperations.values());
      const snapshot = { identity, marks: next };
      if (activeLoads.get(identity) === loadGeneration) {
        activeLoads.delete(identity);
      }
      confirmedMarksRef.current.set(identity, confirmed);
      confirmedDuringLoad.delete(identity);
      for (const [key, pending] of pendingOperations) {
        if (pending.identity === identity && pending.settled) {
          pendingOperations.delete(key);
        }
      }
      optimisticRef.current = snapshot;
      setLoaded(snapshot);
      setReadyIdentity(identity);
    };
    listContentMergeMarks(orgRoot, team)
      .then((paths) => {
        if (!cancelled) publish(paths);
      })
      .catch(() => {
        if (!cancelled) publish([]);
      });
    return () => {
      cancelled = true;
      if (activeLoads.get(identity) === loadGeneration) {
        activeLoads.delete(identity);
      }
      confirmedDuringLoad.delete(identity);
      for (const [key, pending] of pendingOperations) {
        if (pending.identity === identity && pending.settled) {
          pendingOperations.delete(key);
        }
      }
    };
  }, [identity, orgRoot, team, owns]);

  const isMarked = useCallback((logicalPath: string) => marked.has(logicalPath), [marked]);

  const toggle = useCallback(
    async (logicalPath: string) => {
      if (!orgRoot || !team) return;
      const operationKey = `${identity}\0${logicalPath}`;
      const currentMarks =
        optimisticRef.current.identity === identity ? optimisticRef.current.marks : marked;
      const wasMarked = currentMarks.has(logicalPath);
      const operationVersion = (pendingOperationsRef.current.get(operationKey)?.version ?? 0) + 1;
      pendingOperationsRef.current.set(operationKey, {
        identity,
        path: logicalPath,
        desired: !wasMarked,
        version: operationVersion,
        settled: false,
      });
      const optimistic = { identity, marks: withToggled(currentMarks, logicalPath, !wasMarked) };
      optimisticRef.current = optimistic;
      // Optimistic swap first — the badge/label flip immediately.
      setLoaded(optimistic);
      const previousOperation = operationQueuesRef.current.get(operationKey) ?? Promise.resolve();
      const operation = previousOperation
        .catch(() => undefined)
        .then(() =>
          wasMarked
            ? clearContentMergeMark(orgRoot, team, logicalPath)
            : setContentMergeMark(orgRoot, team, logicalPath, {}),
        );
      operationQueuesRef.current.set(operationKey, operation);
      try {
        await operation;
        const confirmed = confirmedMarksRef.current.get(identity) ?? EMPTY;
        confirmedMarksRef.current.set(identity, withToggled(confirmed, logicalPath, !wasMarked));
        if (activeLoadsRef.current.has(identity)) {
          const duringLoad =
            confirmedDuringLoadRef.current.get(identity) ?? new Map<string, boolean>();
          duringLoad.set(logicalPath, !wasMarked);
          confirmedDuringLoadRef.current.set(identity, duringLoad);
        }
        const pending = pendingOperationsRef.current.get(operationKey);
        if (pending?.version === operationVersion) {
          if (activeLoadsRef.current.has(identity)) pending.settled = true;
          else pendingOperationsRef.current.delete(operationKey);
        }
      } catch (e) {
        // Revert to the pre-toggle state and surface the failure.
        const isCurrentOperation =
          pendingOperationsRef.current.get(operationKey)?.version === operationVersion;
        if (isCurrentOperation) {
          pendingOperationsRef.current.delete(operationKey);
          setLoaded((current) => {
            if (current.identity !== identity) return current;
            const reverted = {
              identity,
              marks: withPendingMarks(
                confirmedMarksRef.current.get(identity) ?? EMPTY,
                identity,
                pendingOperationsRef.current.values(),
              ),
            };
            optimisticRef.current = reverted;
            return reverted;
          });
        }
        if (isCurrentOperation && activeIdentityRef.current === identity) {
          toast.error(
            `Couldn't ${wasMarked ? "unmark" : "mark"} merge file: ${
              e instanceof Error ? e.message : String(e)
            }`,
          );
        }
      } finally {
        if (operationQueuesRef.current.get(operationKey) === operation) {
          operationQueuesRef.current.delete(operationKey);
        }
      }
    },
    [identity, orgRoot, team, marked],
  );

  // Stable object identity across renders — it changes only when `isMarked` /
  // `toggle` do (i.e. when the marks actually change), so consumers that key a
  // memo on this hook's return (the nav's `rowCallbacks`) don't rebuild every
  // render.
  const ready = !orgRoot || !team || readyIdentity === identity;
  return useMemo(
    () => ({ isMarked, toggle, paths: marked, ready }),
    [isMarked, toggle, marked, ready],
  );
}
