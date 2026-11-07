// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  memberKey,
  REMOTE_LAYOUT_WAIT_MS,
  remoteLayoutPending,
  remoteLayoutWaitEnds,
} from "@/shared/lib/terminal-layout-projection";
import type { MachineSummary } from "@/tauri/commands";

/** A Machine's states on the way to being read: its layout, and so its tabs, are still to come. */
const CONNECTING_STATES: ReadonlySet<string> = new Set([
  "connecting",
  "handshaking",
  "reconnecting",
  "readmitting",
]);

/** The Machines on their way to being read, in every team, sorted. */
function connectingMachinesOf(s: {
  byTeam: Record<string, MachineSummary[] | undefined>;
}): string[] {
  return [
    ...new Set(
      Object.values(s.byTeam).flatMap((list) =>
        (list ?? []).flatMap((m) =>
          m.status && CONNECTING_STATES.has(m.status.state) ? [m.machineId] : [],
        ),
      ),
    ),
  ].sort();
}

/** When each connecting Machine set out: the ones still connecting keep their time, a new one
 *  takes `now`, and one that left those states is dropped. */
function trackSetOut(
  prev: Readonly<Record<string, number>>,
  connecting: readonly string[],
  now: number,
): Readonly<Record<string, number>> {
  const same =
    connecting.length === Object.keys(prev).length &&
    connecting.every((id) => prev[id] !== undefined);
  return same ? prev : Object.fromEntries(connecting.map((id) => [id, prev[id] ?? now]));
}

/**
 * Whether a remote Machine's layout, and so its tabs, is still to come. A Machine on its way to
 * being read (`connecting`) counts for {@link REMOTE_LAYOUT_WAIT_MS} from when it set out, so one
 * stuck connecting is not waited on forever. The answer reads a clock this hook moves at each
 * wait's end, so the list re-reads the moment a wait runs out.
 */
export function useRemoteLayoutsLoading(): {
  loading: boolean;
  /** The same answer for one Machine's layout alone. */
  loadingFor: (machineId: string) => boolean;
} {
  const [clock, setClock] = useState(() => Date.now());
  const connecting = useMachinesStore(useShallow(connectingMachinesOf));
  // When each connecting Machine set out, by id, kept as the Machines' states change.
  const [since, setSince] = useState(() =>
    trackSetOut({}, connectingMachinesOf(useMachinesStore.getState()), Date.now()),
  );
  useEffect(
    () =>
      useMachinesStore.subscribe((st) =>
        setSince((prev) => trackSetOut(prev, connectingMachinesOf(st), Date.now())),
      ),
    [],
  );
  // Each connecting Machine with when it set out: the projection bounds its wait from then.
  const setOut = useMemo(
    () => Object.fromEntries(connecting.map((id) => [id, since[id] ?? clock])),
    [connecting, since, clock],
  );
  // A wait that began after the clock last moved (a Machine that set out, a layout admitted) has
  // only just started: the clock reads at least that late, so the projection does not take it for
  // a clock that ran backwards and end the wait.
  const loading = useViewerStore((s) =>
    remoteLayoutPending(
      s.terminalLayout,
      setOut,
      Math.max(
        clock,
        ...Object.values(setOut),
        ...Object.values(s.terminalLayout.admittedAt ?? {}),
      ),
    ),
  );
  const layoutEnds = useViewerStore(useShallow((s) => remoteLayoutWaitEnds(s.terminalLayout)));
  useEffect(() => {
    const ends = [
      ...layoutEnds,
      ...Object.values(since).map((t) => t + REMOTE_LAYOUT_WAIT_MS),
    ].filter((t) => t > clock);
    if (ends.length === 0) return;
    const timer = setTimeout(
      () => setClock(Date.now()),
      Math.max(0, Math.min(...ends) - Date.now()),
    );
    return () => clearTimeout(timer);
  }, [layoutEnds, since, clock]);
  const admission = useViewerStore((s) => s.terminalLayout.admission);
  const members = useViewerStore((s) => s.terminalLayout.members);
  const admittedAt = useViewerStore((s) => s.terminalLayout.admittedAt);
  const loadingFor = useCallback(
    (machineId: string) => {
      const key = memberKey({ machineId, slot: "remote" });
      const row = admission?.[key];
      const at = admittedAt?.[key];
      const set = setOut[machineId];
      return remoteLayoutPending(
        {
          admission: row ? { [key]: row } : {},
          members: members ?? {},
          admittedAt: at === undefined ? {} : { [key]: at },
        },
        set === undefined ? {} : { [machineId]: set },
        Math.max(clock, set ?? 0, at ?? 0),
      );
    },
    [admission, members, admittedAt, setOut, clock],
  );
  return { loading, loadingFor };
}

/** True once `ms` have passed since the pane mounted. */
export function useWaitOver(ms: number): boolean {
  const [over, setOver] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setOver(true), ms);
    return () => clearTimeout(timer);
  }, [ms]);
  return over;
}
