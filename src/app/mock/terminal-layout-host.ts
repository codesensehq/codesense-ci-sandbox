// SPDX-License-Identifier: MIT
/**
 * An in-memory terminal host: the host-owned terminal layout model
 * (`codesense-pty-core`'s `layout.rs`) reduced to what the frontend observes —
 * workspaces of tab entries (strip order, names, roles, placement,
 * live/exited, grid, focus), each change announced as one workspace's whole
 * layout under a rising `rev`. It serves `pnpm dev:mock` (which has no Rust)
 * and the unit tests' terminal layout harness, so both drive the viewer store
 * exactly as the real hosts do: through the verbs and their announcements.
 *
 * It runs no processes: an entry is live until {@link MockTerminalHost.exit}
 * marks it exited, and a "busy" one (see {@link MockTerminalHost.setBusy})
 * survives a keep-if-busy close out of sight.
 */

import type {
  RuntimeLayoutReply,
  RuntimeMemberTarget,
  TerminalCloseMode,
  TerminalCloseOutcome,
  TerminalGridState,
  TerminalLayoutChanged,
  TerminalLayoutSnapshot,
  TerminalOpenTabRequest,
  TerminalPlacement,
  TerminalRespawnScope,
  TerminalRespawnedTab,
  TerminalTabEntry,
  TerminalTabSpawn,
  TerminalWorkspaceDescriptor,
  TerminalWorkspaceLayout,
} from "@/tauri/commands";

/** An entry to place directly, as if its session were already running. */
export interface SeededEntry {
  session: string;
  name?: string;
  role?: string;
  placement?: TerminalPlacement;
  exit?: { code: number | null; signal?: string | null };
  tags?: Record<string, string>;
}

function emptyGrid(): TerminalGridState {
  return { paned: [], preset: "columns", collapsed: false };
}

export class MockTerminalHost {
  readonly serverId: string;
  epoch: string;
  private rev = 0;
  /** This host's admission revision: each lifetime is admitted anew. */
  private admission = 1;
  private readonly layouts = new Map<string, TerminalWorkspaceLayout>();
  private readonly busy = new Set<string>();
  private readonly onChange: (change: TerminalLayoutChanged) => void;
  /** Every open request, in order — what a caller asked the host to start. */
  readonly opened: TerminalOpenTabRequest[] = [];

  constructor(onChange: (change: TerminalLayoutChanged) => void, epoch = "mock-epoch-1") {
    this.onChange = onChange;
    this.epoch = epoch;
    this.serverId = "mock-host";
  }

  /** The host's whole layout. */
  snapshot(): TerminalLayoutSnapshot {
    const active: Record<string, string> = {};
    const workspaces: Record<string, TerminalWorkspaceLayout> = {};
    for (const [key, layout] of this.layouts) {
      workspaces[key] = structuredClone(layout);
      if (layout.lastActive) active[key] = layout.lastActive;
    }
    return {
      schema: 1,
      serverId: this.serverId,
      serverName: "mock",
      epoch: this.epoch,
      rev: this.rev,
      workspaces,
      viewer: { active, views: {}, hidden: [] },
    };
  }

  /** The admitted target this host announces its layout under: This Mac's
   *  in-process runtime, in its current lifetime. */
  target(): RuntimeMemberTarget {
    return {
      member: { machineId: "local", slot: "inProcess" },
      admissionRevision: String(this.admission),
      namespace: { serverId: this.serverId, epoch: this.epoch, generation: String(this.admission) },
    };
  }

  /** The host's sealed snapshot reply, as `terminal_layout_snapshot` answers. */
  reply(): RuntimeLayoutReply {
    return { mode: "live", ...this.target(), snapshot: this.snapshot() };
  }

  /** Whether any workspace holds `session`. */
  contains(session: string): boolean {
    return this.locate(session) !== undefined;
  }

  /** Whether `session` is a role-tagged agent (its tab carries a role). */
  isAgent(session: string): boolean {
    const at = this.locate(session);
    return !!at && at.layout.tabs[at.index].role != null;
  }

  openTab(request: TerminalOpenTabRequest): TerminalTabEntry {
    if (this.contains(request.session)) {
      throw new Error(`layout: session ${request.session} is already open`);
    }
    this.opened.push(request);
    return this.place(
      request.workspace,
      request.descriptor,
      {
        session: request.session,
        name: request.name,
        role: request.role,
        placement: request.placement,
        tags: request.tags,
      },
      { index: request.index, activate: request.activate },
    );
  }

  /** Place `entry` as if its session were already running — a restored host. */
  seed(workspace: string, descriptor: TerminalWorkspaceDescriptor, entry: SeededEntry): void {
    this.place(workspace, descriptor, entry, { activate: false });
  }

  closeTab(session: string, mode: TerminalCloseMode): TerminalCloseOutcome {
    const found = this.require(session);
    const entry = found.layout.tabs[found.index];
    if (entry.state.kind === "exited") {
      this.remove(found.key, found.index);
      return "removed";
    }
    if (mode === "keepIfBusy" && this.busy.has(session)) {
      entry.placement = "background";
      this.refocus(found.key, session, found.index);
      this.publish(found.key);
      return "backgrounded";
    }
    this.remove(found.key, found.index);
    return "killed";
  }

  placeTab(
    session: string,
    workspace: string,
    placement: TerminalPlacement,
    index?: number,
    descriptor?: TerminalWorkspaceDescriptor,
  ): void {
    const found = this.require(session);
    if (!this.layouts.has(workspace) && !descriptor) {
      throw new Error(`layout: no workspace ${workspace}`);
    }
    const [entry] = found.layout.tabs.splice(found.index, 1);
    this.refocus(found.key, session, found.index);
    const from = found.key;
    entry.placement = placement;
    const target = this.workspace(workspace, descriptor);
    target.tabs.splice(index ?? target.tabs.length, 0, entry);
    this.publish(from);
    this.publish(workspace);
  }

  setTags(session: string, patch: Record<string, string | null>): void {
    const found = this.require(session);
    const tags = found.layout.tabs[found.index].tags;
    for (const [key, value] of Object.entries(patch)) {
      if (value === null) delete tags[key];
      else tags[key] = value;
    }
    this.publish(found.key);
  }

  renameTab(session: string, name: string): void {
    const found = this.require(session);
    found.layout.tabs[found.index].name = name;
    this.publish(found.key);
  }

  moveTab(session: string, toIndex: number): void {
    const found = this.require(session);
    const [entry] = found.layout.tabs.splice(found.index, 1);
    found.layout.tabs.splice(Math.min(toIndex, found.layout.tabs.length), 0, entry);
    this.publish(found.key);
  }

  setActive(workspace: string, session: string | null): void {
    const layout = this.layouts.get(workspace);
    if (!layout) throw new Error(`layout: no workspace ${workspace}`);
    if (session !== null) {
      if (!layout.tabs.some((t) => t.session === session && t.placement === "tab")) {
        throw new Error(`layout: no entry for session ${session}`);
      }
      layout.lastActive = session;
    }
    this.publish(workspace);
  }

  setGrid(workspace: string, grid: TerminalGridState): void {
    const layout = this.layouts.get(workspace);
    if (!layout) throw new Error(`layout: no workspace ${workspace}`);
    const tabs = new Set(layout.tabs.filter((t) => t.placement === "tab").map((t) => t.session));
    if (grid.paned.some((s) => !tabs.has(s)) || new Set(grid.paned).size !== grid.paned.length) {
      throw new Error(`layout: invalid grid for ${workspace}`);
    }
    layout.grid = structuredClone(grid);
    this.publish(workspace);
  }

  closeWorkspace(workspace: string): void {
    if (!this.layouts.has(workspace)) throw new Error(`layout: no workspace ${workspace}`);
    this.layouts.delete(workspace);
    this.publish(workspace);
  }

  /** The process of `session` exited: its entry stays, read-only, exited. */
  exit(session: string, code: number | null, signal: string | null = null): void {
    const found = this.require(session);
    found.layout.tabs[found.index].state = { kind: "exited", exitCode: code, signal };
    this.publish(found.key);
  }

  /** A new process in `session`'s exited entry, same id and place
   *  (`terminal_respawn_tab`): refused while the entry is live. A scope gets
   *  fresh launch arguments for its agent's line. */
  respawnTab(
    machineId: string,
    session: string,
    _spawn: TerminalTabSpawn,
    scope?: TerminalRespawnScope,
  ): TerminalRespawnedTab {
    if (machineId !== "local") throw new Error(`mock terminal host cannot respawn on ${machineId}`);
    const found = this.require(session);
    const entry = found.layout.tabs[found.index];
    if (entry.state.kind !== "exited") {
      throw new Error(`layout: session ${session} is still running`);
    }
    entry.state = { kind: "live" };
    this.publish(found.key);
    return {
      entry: structuredClone(entry),
      extraArgs: scope ? ["--mcp-config", `/tmp/mcp-scope/${session}/.mcp.json`] : [],
    };
  }

  /** Whether `session`'s foreground is busy (a keep-if-busy close keeps it). */
  setBusy(session: string, busy: boolean): void {
    if (busy) this.busy.add(session);
    else this.busy.delete(session);
  }

  /** A new host lifetime: everything ends, the epoch changes. */
  restart(epoch: string): void {
    this.layouts.clear();
    this.busy.clear();
    this.rev = 0;
    this.admission += 1;
    this.epoch = epoch;
  }

  private place(
    workspace: string,
    descriptor: TerminalWorkspaceDescriptor | undefined,
    entry: SeededEntry,
    opts: { index?: number; activate: boolean },
  ): TerminalTabEntry {
    if (!this.layouts.has(workspace) && !descriptor) {
      throw new Error(`layout: no workspace ${workspace}`);
    }
    const layout = this.workspace(workspace, descriptor);
    const ordinal = layout.nextOrdinal;
    layout.nextOrdinal += 1;
    const placed: TerminalTabEntry = {
      session: entry.session,
      name: entry.name ?? `Terminal ${ordinal}`,
      role: entry.role ?? null,
      placement: entry.placement ?? "tab",
      state: entry.exit
        ? { kind: "exited", exitCode: entry.exit.code, signal: entry.exit.signal ?? null }
        : { kind: "live" },
      tags: entry.tags ?? {},
    };
    layout.tabs.splice(opts.index ?? layout.tabs.length, 0, placed);
    if (placed.placement === "tab" && (opts.activate || !layout.lastActive)) {
      layout.lastActive = placed.session;
    }
    this.publish(workspace);
    return structuredClone(placed);
  }

  private workspace(
    key: string,
    descriptor: TerminalWorkspaceDescriptor | undefined,
  ): TerminalWorkspaceLayout {
    let layout = this.layouts.get(key);
    if (!layout) {
      layout = {
        descriptor: descriptor!,
        tabs: [],
        lastActive: null,
        grid: emptyGrid(),
        nextOrdinal: 1,
      };
      this.layouts.set(key, layout);
    } else if (descriptor) {
      layout.descriptor = descriptor;
    }
    return layout;
  }

  private locate(
    session: string,
  ): { key: string; layout: TerminalWorkspaceLayout; index: number } | undefined {
    for (const [key, layout] of this.layouts) {
      const index = layout.tabs.findIndex((t) => t.session === session);
      if (index >= 0) return { key, layout, index };
    }
    return undefined;
  }

  private require(session: string): {
    key: string;
    layout: TerminalWorkspaceLayout;
    index: number;
  } {
    const found = this.locate(session);
    if (!found) throw new Error(`layout: no entry for session ${session}`);
    return found;
  }

  private remove(key: string, index: number): void {
    const layout = this.layouts.get(key)!;
    const [entry] = layout.tabs.splice(index, 1);
    this.busy.delete(entry.session);
    this.refocus(key, entry.session, index);
    this.settle(key);
    this.publish(key);
  }

  /** Focus on `session` moves to the nearest remaining tab, looking right first. */
  private refocus(key: string, session: string, from: number): void {
    const layout = this.layouts.get(key);
    if (!layout || layout.lastActive !== session) return;
    const tabs = layout.tabs;
    const right = tabs.slice(from).find((t) => t.placement === "tab");
    const left = tabs
      .slice(0, from)
      .reverse()
      .find((t) => t.placement === "tab");
    layout.lastActive = (right ?? left)?.session ?? null;
  }

  /** A workspace's grid panes only its tabs, and an empty workspace is gone. */
  private settle(key: string): void {
    const layout = this.layouts.get(key);
    if (!layout) return;
    const tabs = new Set(layout.tabs.filter((t) => t.placement === "tab").map((t) => t.session));
    layout.grid.paned = layout.grid.paned.filter((s) => tabs.has(s));
    if (layout.tabs.length === 0) this.layouts.delete(key);
  }

  private publish(key: string): void {
    this.settle(key);
    this.rev += 1;
    const layout = this.layouts.get(key);
    this.onChange({
      epoch: this.epoch,
      rev: this.rev,
      workspace: key,
      layout: layout ? structuredClone(layout) : null,
    });
  }
}
