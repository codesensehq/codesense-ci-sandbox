// SPDX-License-Identifier: MIT
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import type { HubDelta, HubSnapshot } from "@/features/dashboard/lib/live-hub";
import type { DashboardRef, PaneEntry, PaneListing, PaneSourceRow } from "@/tauri/commands";
import type { PaneChangedPayload } from "@/tauri/events";

/** Transport fixture for the Rust source projection, shared by mock mode and native seams. */
export class PaneSourceProjection {
  private views = new Map<
    string,
    { org: string; team: string; selected: DashboardRef[]; value?: HubSnapshot }
  >();
  // Models `live_hub/pane_sources.rs`: a source belongs to a reader (a team), its `sourceRevision`
  // is a counter that renumbers whenever the source is re-created after its reader's view was
  // pruned (one view per owner, so opening another team prunes the previous team's sources), and
  // its `documentRevision` is stable per document. When every source for a document was pruned,
  // Rust remembers its last entry: a re-put of the same entry keeps the revision, a different one
  // renumbers it, and a pane change on disk forgets the memory (an asset change leaves the entry
  // as it was, so it must not be mistaken for "unchanged").
  private sources = new Map<string, number>();
  private documents = new Map<string, number>();
  private lastEntries = new Map<string, { ref: DashboardRef; entry: string }>();
  private counter = 0;
  constructor(
    private list: (org: string, team: string) => Promise<PaneListing> | PaneListing,
    private emit: (event: string, value: unknown) => void,
  ) {}
  private ref(org: string, pane: PaneEntry): DashboardRef {
    return pane.dashboardRef ?? { scope: pane.scope, org, team: pane.team, id: pane.id };
  }
  private sourceKey(view: { org: string; team: string }, id: string): string {
    return `${view.org}:${view.team}|${id}`;
  }
  /** Rust `put`: an existing source keeps its revision; a missing one is numbered anew. */
  private put(
    view: { org: string; team: string },
    ref: DashboardRef,
    id: string,
    entry: PaneEntry | null,
  ): [number, number] {
    const key = this.sourceKey(view, id);
    const held = [...this.sources.keys()].some((source) => source.endsWith(`|${id}`));
    const json = JSON.stringify(entry);
    if (!held && entry && this.lastEntries.get(id)?.entry !== json)
      this.documents.set(id, ++this.counter);
    if (entry) this.lastEntries.set(id, { ref, entry: json });
    if (!this.documents.has(id)) this.documents.set(id, ++this.counter);
    if (!this.sources.has(key)) this.sources.set(key, ++this.counter);
    return [this.sources.get(key)!, this.documents.get(id)!];
  }
  /** Rust `prune`: a source whose reader has no view is dropped. */
  private prune(): void {
    const readers = new Set([...this.views.values()].map((v) => `${v.org}:${v.team}`));
    for (const key of this.sources.keys())
      if (!readers.has(key.slice(0, key.indexOf("|")))) this.sources.delete(key);
  }
  private bump(id: string): void {
    this.documents.set(id, ++this.counter);
    for (const key of this.sources.keys())
      if (key.endsWith(`|${id}`)) this.sources.set(key, ++this.counter);
  }
  private async materialize(key: string): Promise<HubSnapshot> {
    const view = this.views.get(key)!;
    const listing = await this.list(view.org, view.team);
    const rows = new Map<string, PaneSourceRow>();
    const add = (pane: PaneEntry, listed: boolean, root: string) => {
      const reference = this.ref(view.org, pane);
      const id = dashboardRefKey(reference);
      const [source, document] = this.put(view, reference, id, pane);
      rows.set(id, {
        id,
        dashboardRef: reference,
        entry: pane,
        contentRoot: root,
        sourceRevision: source,
        documentRevision: document,
        listed,
      });
    };
    for (const pane of listing.panes)
      add(
        pane,
        true,
        pane.scope === "team" && pane.team
          ? listing.contentRoot.replace(`/${view.team}/`, `/${pane.team}/`)
          : listing.contentRoot,
      );
    for (const ref of view.selected.filter((ref) => ref.org === view.org)) {
      const id = dashboardRefKey(ref);
      if (rows.has(id)) continue;
      const own = ref.team ? await this.list(view.org, ref.team) : listing;
      const pane = own.panes.find((p) => dashboardRefKey(this.ref(view.org, p)) === id);
      if (pane) add(pane, false, own.contentRoot);
      else {
        const [source, document] = this.put(view, ref, id, null);
        rows.set(id, {
          id,
          dashboardRef: ref,
          entry: null,
          contentRoot: own.contentRoot,
          sourceRevision: source,
          documentRevision: document,
          listed: false,
        });
      }
    }
    return {
      topic: "paneSources",
      viewKey: key,
      rev: view.value?.rev ?? 0,
      snapshot: [
        { id: "view", listing: { ...listing, panes: [], sources: undefined } },
        ...rows.values(),
      ] as HubSnapshot["snapshot"],
    };
  }
  async read(args: Record<string, unknown>): Promise<PaneListing> {
    if (!args.sourceView) return this.list(String(args.org), String(args.team));
    const key = `paneSources:mock:${String(args.sourceView)}`;
    const old = this.views.get(key)?.value;
    this.views.set(key, {
      org: String(args.org),
      team: String(args.team),
      selected: (args.selectedRefs ?? []) as DashboardRef[],
      value: old,
    });
    const retry = args.refreshRef as DashboardRef | undefined;
    let value = await this.materialize(key);
    this.prune();
    if (retry) {
      // Like the Rust registry, a refresh keeps the revision of an unchanged entry.
      const id = dashboardRefKey(retry);
      const entryOf = (snapshot?: HubSnapshot) =>
        JSON.stringify(snapshot?.snapshot.find((r) => r.id === id)?.entry ?? null);
      if (entryOf(value) !== entryOf(old)) {
        this.bump(id);
        value = await this.materialize(key);
      }
    }
    this.commit(key, value);
    const context = value.snapshot[0].listing as PaneListing;
    return {
      ...context,
      panes: value.snapshot
        .slice(1)
        .filter((r) => r.listed && r.entry)
        .map((r) => r.entry as PaneEntry),
      sources: this.views.get(key)!.value,
    };
  }
  snapshot(key: string): HubSnapshot | undefined {
    return this.views.get(key)?.value;
  }
  release(key: string): void {
    this.views.delete(key);
    this.prune();
  }
  private commit(key: string, next: HubSnapshot) {
    const view = this.views.get(key)!;
    const before = new Map(view.value?.snapshot.map((r) => [r.id, r]) ?? []);
    const after = new Map(next.snapshot.map((r) => [r.id, r]));
    const upserts = next.snapshot.filter(
      (row) => JSON.stringify(before.get(row.id)) !== JSON.stringify(row),
    );
    const removes = [...before.keys()].filter((id) => !after.has(id));
    const changed = upserts.length > 0 || removes.length > 0;
    next.rev += changed ? 1 : 0;
    view.value = next;
    if (changed)
      this.emit("codesense:delta", {
        topic: next.topic,
        viewKey: key,
        rev: next.rev,
        upserts,
        removes,
      } satisfies HubDelta);
  }
  async changed(change: PaneChangedPayload): Promise<void> {
    for (const [id, { ref }] of this.lastEntries)
      if (
        ref.org === change.org &&
        ref.scope === change.scope &&
        (ref.scope === "private" || ref.team === change.team) &&
        (!change.id || ref.id === change.id)
      )
        this.lastEntries.delete(id);
    const touched = new Set<string>();
    const affected: string[] = [];
    for (const [key, view] of this.views) {
      if (change.org !== view.org) continue;
      const context = view.value?.snapshot.find((r) => r.id === "view")?.listing as
        | PaneListing
        | undefined;
      const matching = (ref: DashboardRef) =>
        ref.scope === change.scope && (ref.scope === "private" || ref.team === change.team);
      const layerVisible =
        change.scope === "private" || !context || context.chain.includes(change.team!);
      if (!layerVisible && !view.selected.some(matching)) continue;
      for (const row of view.value?.snapshot ?? []) {
        const ref = row.dashboardRef as DashboardRef | undefined;
        if (ref && matching(ref) && (!change.id || ref.id === change.id)) touched.add(row.id);
      }
      if (change.id && layerVisible)
        touched.add(
          dashboardRefKey({
            scope: change.scope,
            org: change.org,
            team: change.team,
            id: change.id,
          }),
        );
      affected.push(key);
    }
    for (const id of touched) this.bump(id);
    for (const key of affected) this.commit(key, await this.materialize(key));
  }
}
