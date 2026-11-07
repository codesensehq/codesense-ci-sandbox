// SPDX-License-Identifier: MIT
import { useRef } from "react";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import type { HubSnapshot } from "./live-hub";
import type { PaneListing, PaneSourceRow } from "@/tauri/commands";

/** Materialize the native view; source state remains owned by the hub. */
export function applyPaneSources(snapshot: HubSnapshot, before: Record<string, PaneSourceRow>) {
  const context = snapshot.snapshot.find((row) => row.id === "view")?.listing as PaneListing;
  const rows = Object.fromEntries(
    snapshot.snapshot
      .filter((row) => row.id !== "view")
      .map((row) => {
        const source = row as unknown as PaneSourceRow;
        const prior = before[source.id];
        return [
          source.id,
          prior?.sourceRevision === source.sourceRevision &&
          prior.documentRevision === source.documentRevision &&
          prior.listed === source.listed
            ? prior
            : source,
        ];
      }),
  );
  const panes = Object.values(rows)
    .filter((row) => row.listed && row.entry)
    .map((row) => row.entry!);
  panes.sort((a, b) => {
    const rank = (pane: typeof a) =>
      pane.dashboardRef?.scope === "built-in"
        ? Number.MAX_SAFE_INTEGER
        : pane.scope === "private"
          ? context.chain.length
          : context.chain.indexOf(pane.team!);
    return rank(a) - rank(b) || a.id.localeCompare(b.id);
  });
  return { listing: { ...context, panes }, sourceRows: rows };
}

/** Hold a frame's applied source while hidden. Many changes become one refresh on reveal. */
export function useSourceRevision(id: string | null, visible: boolean): PaneSourceRow | undefined {
  const applied = useRef<PaneSourceRow | undefined>(undefined);
  if (applied.current?.id !== id) applied.current = undefined;
  const current = usePaneStore((s) => (visible && id ? s.sourceRows[id] : applied.current));
  if (visible) applied.current = current;
  return applied.current;
}
