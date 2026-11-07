// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

import type {
  RuntimeAdmissionRow,
  RuntimeLayoutReply,
  RuntimeMemberTarget,
  TerminalLayoutChanged,
  TerminalLayoutSnapshot,
} from "@/tauri/commands";

import {
  listenForPrivateAuthorityRetirement,
  observePrivateAuthority,
  retirePrivateAuthority,
} from "./private-terminal-authority";
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));

// The real hydrate against scripted members: their listeners and snapshots.
const host = vi.hoisted(() => ({
  order: [] as string[],
  changed: null as
    | ((
        target: RuntimeMemberTarget,
        change: TerminalLayoutChanged,
        privateAuthorityEpoch?: string,
      ) => void)
    | null,
  admitted: null as ((row: RuntimeAdmissionRow) => void) | null,
  resync: null as (() => void) | null,
  snapshots: [] as Array<RuntimeLayoutReply[] | Promise<RuntimeLayoutReply[]>>,
  bootIds: [] as string[],
  opened: 0,
  listening: 0,
}));

vi.mock("@/features/viewer/lib/cli-workspace/transport/terminal-layout-source", () => {
  const listen =
    <H>(name: string, set: (handler: H | null) => void) =>
    (handler: H) => {
      host.order.push(`listen:${name}`);
      host.listening += 1;
      set(handler);
      return Promise.resolve(() => {
        host.listening -= 1;
        set(null);
      });
    };
  return {
    onTerminalLayoutChanged: listen("changed", (h: typeof host.changed) => (host.changed = h)),
    onTerminalAdmissionChanged: listen(
      "admission",
      (h: typeof host.admitted) => (host.admitted = h),
    ),
    onTerminalLayoutResync: listen("resync", (h: typeof host.resync) => (host.resync = h)),
    terminalLayoutMemberSnapshots: (bootId: string) => {
      host.order.push("snapshot");
      host.bootIds.push(bootId);
      const next = host.snapshots.shift();
      return next ? Promise.resolve(next) : Promise.reject("the host could not be asked");
    },
    terminalOpenTab: () => {
      host.opened += 1;
      return Promise.reject("restore must never open a terminal");
    },
  };
});

import { useViewerStore, selectWorkspaceTabs } from "@/shared/stores/viewer-store";
import { EMPTY_TERMINAL_LAYOUT } from "./terminal-layout-projection";
import { clientBootId, hydrateTerminalLayout, whenTerminalLayoutHydrated } from "./terminal-layout";

const TEAM = "acme:core";
const K = `${TEAM}|w1`;

function snap(epoch: string, rev: number, sessions: string[]): TerminalLayoutSnapshot {
  return {
    schema: 1,
    serverId: "s",
    serverName: "s",
    epoch,
    rev,
    workspaces: sessions.length
      ? {
          [K]: {
            descriptor: { displayName: "w1", kind: "generic" },
            tabs: sessions.map((session) => ({
              session,
              name: session,
              role: null,
              placement: "tab" as const,
              state: { kind: "live" as const },
              tags: {},
            })),
            lastActive: null,
            grid: { paned: [], preset: "columns" as const, collapsed: false },
            nextOrdinal: sessions.length + 1,
          },
        }
      : {},
    viewer: { active: {}, views: {}, hidden: [] },
  };
}

const LOCAL = { machineId: "local", slot: "inProcess" } as const;

function target(epoch: string, revision = "1", member: RuntimeMemberTarget["member"] = LOCAL) {
  return {
    member,
    admissionRevision: revision,
    namespace: { serverId: "s", epoch, generation: revision },
  };
}

/** One live member's sealed reply. */
function live(
  epoch: string,
  rev: number,
  sessions: string[],
  member: RuntimeMemberTarget["member"] = LOCAL,
): RuntimeLayoutReply {
  return { mode: "live", ...target(epoch, "1", member), snapshot: snap(epoch, rev, sessions) };
}

const change = (epoch: string, rev: number, sessions: string[]): TerminalLayoutChanged => ({
  epoch,
  rev,
  workspace: K,
  layout: snap(epoch, rev, sessions).workspaces[K] ?? null,
});

const shown = () => selectWorkspaceTabs(useViewerStore.getState(), TEAM, "w1").map((t) => t.id);

beforeEach(() => {
  host.order.length = 0;
  host.snapshots.length = 0;
  host.bootIds.length = 0;
  host.opened = 0;
  host.listening = 0;
  sessionStorage.clear();
  useViewerStore.setState({
    terminalLayout: EMPTY_TERMINAL_LAYOUT,
    terminalHostsUnreachable: false,
    workspaces: { [TEAM]: [{ id: "w1", kind: "generic", cwd: "/w1", status: "ready" }] },
    softClosedWorkspaces: {},
  });
});

describe("hydrateTerminalLayout — the whole launch restore", () => {
  it("subscribes FIRST, then fetches every member's snapshot — and never opens a terminal", async () => {
    host.snapshots.push([live("e1", 3, ["a", "b"])]);
    const stop = await hydrateTerminalLayout();
    expect(host.order).toEqual(["listen:changed", "listen:admission", "listen:resync", "snapshot"]);
    expect(shown()).toEqual(["a", "b"]);
    expect(useViewerStore.getState().terminalLayout.hydrated).toBe(true);
    expect(host.opened).toBe(0);
    stop();
  });

  it("a workspace with no live terminals restores empty — nothing auto-starts", async () => {
    host.snapshots.push([live("e1", 0, [])]);
    await hydrateTerminalLayout();
    expect(shown()).toEqual([]);
    expect(host.opened).toBe(0);
  });

  it("a change that lands before the snapshot waits for it (and a stale one is dropped)", async () => {
    let release!: (s: RuntimeLayoutReply[]) => void;
    host.snapshots.push(new Promise<RuntimeLayoutReply[]>((r) => (release = r)));
    const hydrating = hydrateTerminalLayout();
    await vi.waitFor(() => expect(host.order).toContain("snapshot"));
    host.changed!(target("e1"), change("e1", 4, ["a", "c"]));
    host.changed!(target("e1"), change("e1", 3, ["stale"]));
    expect(shown()).toEqual([]);
    release([live("e1", 3, ["a"])]);
    await hydrating;
    expect(shown()).toEqual(["a", "c"]);
  });

  it("a host's resync notice, or a lifetime it never saw, re-fetches the snapshot", async () => {
    host.snapshots.push([live("e1", 1, ["a"])]);
    await hydrateTerminalLayout();
    host.snapshots.push([live("e2", 0, ["b"])]);
    host.changed!(target("e2"), change("e2", 1, ["b"]));
    await vi.waitFor(() => expect(shown()).toEqual(["b"]));
    host.snapshots.push([live("e2", 5, ["b", "c"])]);
    host.resync!();
    await vi.waitFor(() => expect(shown()).toEqual(["b", "c"]));
    expect(host.opened).toBe(0);
  });

  it("until a snapshot lands the layout reads 'not known yet', and it is asked for again", async () => {
    vi.useFakeTimers();
    try {
      await hydrateTerminalLayout();
      expect(useViewerStore.getState().terminalLayout.hydrated).toBe(false);
      // The terminal area says it is reconnecting instead of showing nothing.
      expect(useViewerStore.getState().terminalHostsUnreachable).toBe(true);
      host.snapshots.push([live("e1", 1, ["a"])]);
      await vi.advanceTimersByTimeAsync(3000);
      expect(shown()).toEqual(["a"]);
      expect(useViewerStore.getState().terminalHostsUnreachable).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("whenTerminalLayoutHydrated waits for the first snapshot", async () => {
    let done = false;
    const waiting = whenTerminalLayoutHydrated().then(() => (done = true));
    await Promise.resolve();
    expect(done).toBe(false);
    host.snapshots.push([live("e1", 0, [])]);
    await hydrateTerminalLayout();
    await waiting;
    expect(done).toBe(true);
  });
});

describe("hydrateTerminalLayout — admitted members", () => {
  it("a remote Machine's tabs are ingested but never shown in This Mac's workspace", async () => {
    host.snapshots.push([
      live("e1", 1, ["a"]),
      live("box", 1, ["remote"], { machineId: "box", slot: "remote" }),
    ]);
    await hydrateTerminalLayout();
    expect(shown()).toEqual(["a"]);
    expect(useViewerStore.getState().terminalLayout.members["box/remote"]).toBeDefined();
  });

  it("an admission that lands before the snapshot waits for it, then removes the member", async () => {
    let release!: (s: RuntimeLayoutReply[]) => void;
    host.snapshots.push(new Promise<RuntimeLayoutReply[]>((r) => (release = r)));
    const hydrating = hydrateTerminalLayout();
    await vi.waitFor(() => expect(host.order).toContain("snapshot"));
    host.admitted!({
      member: LOCAL,
      revision: "2",
      state: "removed",
      namespace: null,
      historical: null,
    });
    release([live("e1", 1, ["a"])]);
    await hydrating;
    expect(shown()).toEqual([]);
    expect(host.opened).toBe(0);
  });

  it("a new lifetime's admission re-fetches the snapshot", async () => {
    host.snapshots.push([live("e1", 1, ["a"])]);
    await hydrateTerminalLayout();
    host.snapshots.push([{ ...live("e2", 0, ["b"]), ...target("e2", "2") }]);
    host.admitted!({
      member: LOCAL,
      revision: "2",
      state: "live",
      namespace: target("e2", "2").namespace,
      historical: null,
    });
    await vi.waitFor(() => expect(shown()).toEqual(["b"]));
  });

  it("an incomplete snapshot keeps This Mac's tabs and is asked for again", async () => {
    vi.useFakeTimers();
    try {
      host.snapshots.push([live("e1", 1, ["a"])]);
      await hydrateTerminalLayout();
      // The server reconnected; its reply is withheld because its admission
      // changed while the snapshot was read.
      host.snapshots.push([]);
      host.admitted!({
        member: LOCAL,
        revision: "2",
        state: "live",
        namespace: target("e2", "2").namespace,
        historical: null,
      });
      await vi.advanceTimersByTimeAsync(0);
      expect(shown()).toEqual(["a"]);
      host.snapshots.push([{ ...live("e2", 0, ["a", "b"]), ...target("e2", "2") }]);
      await vi.advanceTimersByTimeAsync(3000);
      expect(shown()).toEqual(["a", "b"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a member that stays withheld is asked for again, backing off from 3 s to 30 s", async () => {
    vi.useFakeTimers();
    try {
      host.snapshots.push([live("e1", 1, ["a"])]);
      await hydrateTerminalLayout();
      for (let i = 0; i < 20; i++) host.snapshots.push([]);
      host.resync!();
      await vi.advanceTimersByTimeAsync(0);
      const asked = () => host.order.filter((step) => step === "snapshot").length;
      expect(asked()).toBe(2);
      // Waits of 3, 6, 12, 24, then 30 s each.
      for (const [wait, total] of [
        [2999, 2],
        [1, 3],
        [6000, 4],
        [12_000, 5],
        [24_000, 6],
        [30_000, 7],
        [30_000, 8],
      ]) {
        await vi.advanceTimersByTimeAsync(wait);
        expect(asked()).toBe(total);
      }
      expect(shown()).toEqual(["a"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a fresh boot whose server read failed asks again instead of showing nothing", async () => {
    vi.useFakeTimers();
    try {
      host.snapshots.push([{ mode: "unavailable", member: LOCAL, admissionRevision: "1" }]);
      await hydrateTerminalLayout();
      expect(shown()).toEqual([]);
      host.snapshots.push([live("e1", 1, ["a"])]);
      await vi.advanceTimersByTimeAsync(3000);
      expect(shown()).toEqual(["a"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("teardown removes every listener, and nothing lands after it", async () => {
    host.snapshots.push([live("e1", 1, ["a"])]);
    const stop = await hydrateTerminalLayout();
    expect(host.listening).toBe(3);
    const changed = host.changed!;
    stop();
    expect(host.listening).toBe(0);
    changed(target("e1"), change("e1", 2, ["a", "late"]));
    expect(shown()).toEqual(["a"]);
  });
});

describe("clientBootId", () => {
  it("is one id per app launch: a webview reload keeps it", () => {
    const first = clientBootId();
    expect(clientBootId()).toBe(first);
    sessionStorage.clear(); // a relaunch starts a fresh webview session
    expect(clientBootId()).not.toBe(first);
  });
});

it("C hydration clears retired private snapshots and queued pushes while local layout continues", async () => {
  await listenForPrivateAuthorityRetirement();
  observePrivateAuthority({ epoch: "900", available: true });
  const remote = { machineId: "private-box", slot: "remote" } as const;
  let release!: (replies: RuntimeLayoutReply[]) => void;
  host.snapshots.push(
    new Promise((resolve) => {
      release = resolve;
    }),
  );
  const hydrating = hydrateTerminalLayout();
  await vi.waitFor(() => expect(host.order).toContain("snapshot"));
  host.admitted!({
    member: remote,
    revision: "1",
    state: "live",
    namespace: target("private", "1", remote).namespace,
    historical: null,
    privateAuthorityEpoch: "900",
  });
  host.changed!(
    target("private", "1", remote),
    change("private", 2, ["private-queued-sentinel"]),
    "900",
  );
  retirePrivateAuthority({ epoch: "901", available: false });
  release([
    {
      ...live("private", 1, ["private-snapshot-sentinel"], remote),
      privateAuthorityEpoch: "900",
    } as RuntimeLayoutReply,
    live("local", 1, ["local-tab"]),
  ]);
  const stop = await hydrating;
  expect(shown()).toEqual(["local-tab"]);
  expect(JSON.stringify(useViewerStore.getState().terminalLayout)).not.toContain(
    "private-snapshot-sentinel",
  );
  expect(useViewerStore.getState().terminalLayout.members["private-box/remote"]).toBeUndefined();
  observePrivateAuthority({ epoch: "902", available: true });
  host.snapshots.push([
    live("local", 1, ["local-tab"]),
    {
      ...live("fresh", 1, ["fresh-private-sentinel"], remote),
      privateAuthorityEpoch: "902",
    } as RuntimeLayoutReply,
  ]);
  host.resync!();
  await vi.waitFor(() =>
    expect(useViewerStore.getState().terminalLayout.members["private-box/remote"]).toBeDefined(),
  );
  retirePrivateAuthority({ epoch: "903", available: false });
  expect(useViewerStore.getState().terminalLayout.members["private-box/remote"]).toBeUndefined();
  expect(JSON.stringify(useViewerStore.getState().terminalLayout)).not.toContain(
    "fresh-private-sentinel",
  );
  host.changed!(target("fresh", "1", remote), change("fresh", 2, ["untagged-private-sentinel"]));
  host.changed!(target("local"), change("local", 2, ["local-continued"]));
  expect(shown()).toEqual(["local-continued"]);
  stop();
});
