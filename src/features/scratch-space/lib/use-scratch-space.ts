// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useState } from "react";

import { currentNotesScope } from "@/shared/lib/notes-scope";
import {
  agentsList,
  listNotes,
  listScratchSpace,
  routineList,
  type AgentRow,
  type NoteMeta,
  type RoutineRow,
  type ScratchSpaceListing,
} from "@/tauri/commands";

/** Everything the Scratch space tab lists for one org. Each list that failed to read
 *  carries its reason in `errors` (keyed by section) and reads empty; the rest show. */
export interface ScratchSpaceContents {
  listing: ScratchSpaceListing;
  notes: NoteMeta[];
  routines: RoutineRow[];
  agents: AgentRow[];
  errors: Partial<Record<"notes" | "routines" | "agents", string>>;
}

export type ScratchSpaceState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; contents: ScratchSpaceContents };

/** Pinned notes first, then the most recently opened. */
function byRecency(notes: NoteMeta[]): NoteMeta[] {
  return [...notes].sort(
    (a, b) => Number(b.pinned) - Number(a.pinned) || b.lastOpenedAt - a.lastOpenedAt,
  );
}

function reason(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

async function readContents(orgSlug: string | null): Promise<ScratchSpaceContents> {
  // With no org open only the all-orgs folder applies: notes, routines and agents
  // are kept per org. The listing itself is the one read the tab can't do without.
  if (orgSlug === null) {
    return {
      listing: await listScratchSpace(null),
      notes: [],
      routines: [],
      agents: [],
      errors: {},
    };
  }
  const [listing, notes, routines, agents] = await Promise.all([
    listScratchSpace(orgSlug),
    listNotes(currentNotesScope()).then(
      (v) => ({ ok: true as const, v }),
      (e: unknown) => ({ ok: false as const, e: reason(e) }),
    ),
    routineList().then(
      (v) => ({ ok: true as const, v }),
      (e: unknown) => ({ ok: false as const, e: reason(e) }),
    ),
    agentsList(false).then(
      (v) => ({ ok: true as const, v }),
      (e: unknown) => ({ ok: false as const, e: reason(e) }),
    ),
  ]);
  const errors: ScratchSpaceContents["errors"] = {};
  if (!notes.ok) errors.notes = notes.e;
  if (!routines.ok) errors.routines = routines.e;
  if (!agents.ok) errors.agents = agents.e;
  return {
    listing,
    notes: notes.ok ? byRecency(notes.v) : [],
    // Both lists already narrow scratch items to the open org.
    routines: routines.ok ? routines.v.filter((r) => r.scope === "personal") : [],
    agents: agents.ok ? agents.v.filter((a) => a.scope === "personal") : [],
    errors,
  };
}

/** Load the open org's scratch items; re-read when the org changes or on `reload`. While
 *  a read for the current org is in flight the state is `loading` (never the previous
 *  org's items under the new org's name). */
export function useScratchSpace(orgSlug: string | null): {
  state: ScratchSpaceState;
  reload: () => void;
} {
  const [generation, setGeneration] = useState(0);
  const reload = useCallback(() => setGeneration((g) => g + 1), []);
  const key = `${orgSlug ?? ""}#${generation}`;
  const [settled, setSettled] = useState<{ key: string; state: ScratchSpaceState } | null>(null);

  useEffect(() => {
    let live = true;
    readContents(orgSlug).then(
      (contents) => {
        if (live) setSettled({ key, state: { kind: "ready", contents } });
      },
      (e: unknown) => {
        if (live) {
          setSettled({ key, state: { kind: "error", message: reason(e) } });
        }
      },
    );
    return () => {
      live = false;
    };
  }, [orgSlug, key]);

  const state: ScratchSpaceState =
    settled && settled.key === key ? settled.state : { kind: "loading" };
  return { state, reload };
}
