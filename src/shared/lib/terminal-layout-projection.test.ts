// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import type {
  RuntimeAdmissionRow,
  RuntimeLayoutReply,
  RuntimeMemberAddress,
  RuntimeMemberTarget,
  TerminalLayoutSnapshot,
  TerminalTabEntry,
  TerminalWorkspaceLayout,
} from "@/tauri/commands";
import {
  applyLayoutInput,
  EMPTY_TERMINAL_LAYOUT,
  memberOfSession,
  remoteLayoutPending,
  remoteLayoutWaitEnds,
  REMOTE_LAYOUT_WAIT_MS,
  sessionTarget,
  type TerminalLayoutProjection,
  USER_HIDDEN_TAG,
} from "./terminal-layout-projection";

function entry(session: string, fields: Partial<TerminalTabEntry> = {}): TerminalTabEntry {
  return {
    session,
    name: session,
    role: null,
    placement: "tab",
    state: { kind: "live" },
    tags: {},
    ...fields,
  };
}

function layout(
  tabs: TerminalTabEntry[],
  fields: Partial<TerminalWorkspaceLayout> = {},
): TerminalWorkspaceLayout {
  return {
    descriptor: { displayName: "w", kind: "generic" },
    tabs,
    lastActive: null,
    grid: { paned: [], preset: "columns", collapsed: false },
    nextOrdinal: tabs.length + 1,
    ...fields,
  };
}

function snapshot(
  epoch: string,
  rev: number,
  workspaces: Record<string, TerminalWorkspaceLayout>,
  viewer: Partial<TerminalLayoutSnapshot["viewer"]> = {},
): TerminalLayoutSnapshot {
  return {
    schema: 1,
    serverId: `server-${epoch}`,
    serverName: epoch,
    epoch,
    rev,
    workspaces,
    viewer: { active: {}, views: {}, hidden: [], ...viewer },
  };
}

const K = "acme:core|w";

const IN_PROCESS: RuntimeMemberAddress = { machineId: "local", slot: "inProcess" };
const SIDECAR: RuntimeMemberAddress = { machineId: "local", slot: "sidecar" };
const REMOTE: RuntimeMemberAddress = { machineId: "box", slot: "remote" };

/** A member's admitted target in the lifetime `epoch`; This Mac's members
 *  share one real ServerId. */
function target(epoch: string, member = IN_PROCESS, revision = "1"): RuntimeMemberTarget {
  return {
    member,
    admissionRevision: revision,
    namespace: { serverId: "shared", epoch, generation: revision },
  };
}

function live(
  snap: TerminalLayoutSnapshot,
  member = IN_PROCESS,
  revision = "1",
): RuntimeLayoutReply {
  return { mode: "live", ...target(snap.epoch, member, revision), snapshot: snap };
}

/** Hydrate from one snapshot per local member: in-process, then sidecar. */
function hydrate(...snapshots: TerminalLayoutSnapshot[]): TerminalLayoutProjection {
  const members = [IN_PROCESS, SIDECAR];
  const replies = snapshots.map((snap, i) => live(snap, members[i]));
  return applyLayoutInput(EMPTY_TERMINAL_LAYOUT, { kind: "snapshot", replies }).next;
}

/** A live admission row in `epoch`, or a removal without one. */
function row(member: RuntimeMemberAddress, revision: string, epoch?: string): RuntimeAdmissionRow {
  return epoch
    ? {
        member,
        revision,
        state: "live",
        namespace: target(epoch, member, revision).namespace,
        historical: null,
      }
    : { member, revision, state: "removed", namespace: null, historical: null };
}

describe("applyLayoutInput — the projection's one way in", () => {
  it("a snapshot hydrates: tab entries in strip order, the viewer's focus, the grid", () => {
    const p = hydrate(
      snapshot(
        "e1",
        4,
        {
          [K]: layout([entry("a"), entry("bg", { placement: "background" }), entry("b")], {
            grid: { paned: ["b"], preset: "grid", collapsed: false },
            lastActive: "a",
          }),
        },
        { active: { [K]: "b" } },
      ),
    );
    expect(p.hydrated).toBe(true);
    expect(p.byKey[K].tabs.map((t) => t.id)).toEqual(["a", "b"]);
    expect(p.byKey[K].active).toBe("b");
    expect(p.byKey[K].grid).toEqual({ paned: ["b"], preset: "grid", collapsed: false });
  });

  it("Hidden tabs are the Background entries the user hid — an ended one stays listed; never a headless launch's", () => {
    const hidden = { [USER_HIDDEN_TAG]: "tab" };
    const p = hydrate(
      snapshot("e1", 1, {
        [K]: layout([
          entry("a"),
          entry("hid", { placement: "background", tags: hidden }),
          entry("headless", { placement: "background" }),
          entry("gone", {
            placement: "background",
            tags: hidden,
            state: { kind: "exited", exitCode: 0, signal: null },
          }),
        ]),
      }),
    );
    expect(p.byKey[K].tabs.map((t) => t.id)).toEqual(["a"]);
    expect(p.byKey[K].hiddenTabs.map((t) => t.id)).toEqual(["hid", "gone"]);
    expect(p.byKey[K].hiddenTabs[1].exit).toEqual({ code: 0, signal: null });
  });

  it("a workspace no viewer focused shows its first tab", () => {
    const p = hydrate(snapshot("e1", 1, { [K]: layout([entry("a"), entry("b")]) }));
    expect(p.byKey[K].active).toBe("a");
  });

  it("an exited entry projects its real exit status", () => {
    const p = hydrate(
      snapshot("e1", 1, {
        [K]: layout([entry("a", { state: { kind: "exited", exitCode: 3, signal: null } })]),
      }),
    );
    expect(p.byKey[K].tabs[0].exit).toEqual({ code: 3, signal: null });
  });

  it("exited entries this viewer relaunched past are not shown", () => {
    const p = hydrate(
      snapshot("e1", 1, { [K]: layout([entry("gone"), entry("a")]) }, { hidden: ["gone"] }),
    );
    expect(p.byKey[K].tabs.map((t) => t.id)).toEqual(["a"]);
  });

  it("a change newer than the host's rev applies; an old or duplicate one is dropped", () => {
    const p = hydrate(snapshot("e1", 5, { [K]: layout([entry("a")]) }));
    const stale = applyLayoutInput(p, {
      kind: "change",
      target: target("e1"),
      change: { epoch: "e1", rev: 5, workspace: K, layout: layout([]) },
    });
    expect(stale).toEqual({ next: p, outcome: "stale" });
    const applied = applyLayoutInput(p, {
      kind: "change",
      target: target("e1"),
      change: { epoch: "e1", rev: 6, workspace: K, layout: layout([entry("a"), entry("b")]) },
    });
    expect(applied.outcome).toBe("applied");
    expect(applied.next.byKey[K].tabs.map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("a gap in the revs, or a host lifetime it never saw, asks for a re-fetch", () => {
    const p = hydrate(snapshot("e1", 5, { [K]: layout([entry("a")]) }));
    const gap = applyLayoutInput(p, {
      kind: "change",
      target: target("e1"),
      change: { epoch: "e1", rev: 8, workspace: K, layout: layout([entry("x")]) },
    });
    expect(gap.outcome).toBe("resync");
    expect(gap.next.byKey[K].tabs.map((t) => t.id)).toEqual(["x"]);
    const unknown = applyLayoutInput(p, {
      kind: "change",
      target: target("e2"),
      change: { epoch: "e2", rev: 1, workspace: K, layout: layout([entry("y")]) },
    });
    expect(unknown).toEqual({ next: p, outcome: "resync" });
  });

  it("a workspace whose layout is gone leaves the projection", () => {
    const p = hydrate(snapshot("e1", 1, { [K]: layout([entry("a")]) }));
    const { next } = applyLayoutInput(p, {
      kind: "change",
      target: target("e1"),
      change: { epoch: "e1", rev: 2, workspace: K, layout: null },
    });
    expect(next.byKey[K]).toBeUndefined();
  });

  it("a workspace held by two hosts merges their tabs", () => {
    const p = hydrate(
      snapshot("local", 1, { [K]: layout([entry("in-process")]) }),
      snapshot("sidecar", 9, { [K]: layout([entry("hosted")], { lastActive: "hosted" }) }),
    );
    expect(p.byKey[K].tabs.map((t) => t.id)).toEqual(["in-process", "hosted"]);
    expect(p.byKey[K].active).toBe("hosted");
    expect(memberOfSession(p, "hosted")?.key).toBe("local/sidecar");
  });

  it("an unchanged tab keeps its identity across changes (no needless re-render)", () => {
    const p = hydrate(snapshot("e1", 1, { [K]: layout([entry("a"), entry("b")]) }));
    const { next } = applyLayoutInput(p, {
      kind: "change",
      target: target("e1"),
      change: {
        epoch: "e1",
        rev: 2,
        workspace: K,
        layout: layout([entry("a"), entry("b", { name: "renamed" })]),
      },
    });
    expect(next.byKey[K].tabs[0]).toBe(p.byKey[K].tabs[0]);
    expect(next.byKey[K].tabs[1]).not.toBe(p.byKey[K].tabs[1]);
    const other = "acme:core|other";
    const { next: after } = applyLayoutInput(next, {
      kind: "change",
      target: target("e1"),
      change: { epoch: "e1", rev: 3, workspace: other, layout: layout([entry("c")]) },
    });
    expect(after.byKey[K]).toBe(next.byKey[K]);
  });

  it("a re-fetched snapshot older than the changes already applied does not roll them back", () => {
    const p = hydrate(snapshot("e1", 5, { [K]: layout([entry("a")]) }));
    const { next } = applyLayoutInput(p, {
      kind: "change",
      target: target("e1"),
      change: { epoch: "e1", rev: 6, workspace: K, layout: layout([entry("a"), entry("b")]) },
    });
    const refetched = applyLayoutInput(next, {
      kind: "snapshot",
      replies: [live(snapshot("e1", 5, { [K]: layout([entry("a")]) }))],
    });
    expect(refetched.next.byKey[K].tabs.map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("a snapshot replaces every member: a lifetime that ended takes its tabs with it", () => {
    const p = hydrate(
      snapshot("local", 1, {}),
      snapshot("old-sidecar", 3, { [K]: layout([entry("gone")]) }),
    );
    const { next } = applyLayoutInput(p, {
      kind: "snapshot",
      replies: [live(snapshot("local", 1, {})), live(snapshot("new-sidecar", 0, {}), SIDECAR)],
    });
    expect(next.byKey[K]).toBeUndefined();
  });

  it("a snapshot withholding This Mac's member keeps its tabs and asks again", () => {
    const p = hydrate(
      snapshot("local", 1, {}),
      snapshot("sidecar", 3, { [K]: layout([entry("still-running")]) }),
    );
    // The sidecar's reply was withheld: its admission changed mid-read.
    const { next, outcome } = applyLayoutInput(p, {
      kind: "snapshot",
      replies: [live(snapshot("local", 1, {}))],
    });
    expect(outcome).toBe("resync");
    expect(next.byKey[K].tabs.map((t) => t.id)).toEqual(["still-running"]);
  });

  it("an admitted-live local member a snapshot leaves without a layout is asked for again", () => {
    const admitted = applyLayoutInput(hydrate(snapshot("local", 1, {})), {
      kind: "admission",
      row: row(SIDECAR, "2", "sidecar"),
    }).next;
    const { outcome } = applyLayoutInput(admitted, {
      kind: "snapshot",
      replies: [live(snapshot("local", 1, {}))],
    });
    expect(outcome).toBe("resync");
  });

  it("a failed read of a live local member keeps it live and shown, and asks again", () => {
    const p = hydrate(
      snapshot("local", 1, {}),
      snapshot("sidecar", 3, { [K]: layout([entry("still-running")]) }),
    );
    // A busy server: the backend could not read the sidecar's layout and sent
    // an unavailable reply at the admission it already holds.
    const { next, outcome } = applyLayoutInput(p, {
      kind: "snapshot",
      replies: [
        live(snapshot("local", 1, {})),
        { mode: "unavailable", member: SIDECAR, admissionRevision: "1" },
      ],
    });
    expect(outcome).toBe("resync");
    expect(next.admission["local/sidecar"].state).toBe("live");
    expect(next.byKey[K].tabs.map((t) => t.id)).toEqual(["still-running"]);
  });

  it("an unavailable reply at a newer admission, or for a remote Machine, is recorded as such", () => {
    const p = hydrate(
      snapshot("local", 1, {}),
      snapshot("sidecar", 3, { [K]: layout([entry("still-running")]) }),
    );
    // The sidecar really went unavailable (a newer admission): recorded, tabs kept.
    const newer = applyLayoutInput(p, {
      kind: "snapshot",
      replies: [
        live(snapshot("local", 1, {})),
        { mode: "unavailable", member: SIDECAR, admissionRevision: "2" },
      ],
    });
    expect(newer.next.admission["local/sidecar"].state).toBe("unavailable");
    expect(newer.next.byKey[K].tabs.map((t) => t.id)).toEqual(["still-running"]);
    // A remote Machine's unavailable reply is not asked for again.
    const remote = applyLayoutInput(p, {
      kind: "snapshot",
      replies: [
        live(snapshot("local", 1, {})),
        live(snapshot("sidecar", 3, { [K]: layout([entry("still-running")]) }), SIDECAR),
        { mode: "unavailable", member: REMOTE, admissionRevision: "2" },
      ],
    });
    expect(remote.outcome).toBe("applied");
  });

  it("a fresh viewer whose server read failed asks again instead of settling on nothing", () => {
    const { next, outcome } = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [{ mode: "unavailable", member: SIDECAR, admissionRevision: "1" }],
    });
    expect(outcome).toBe("resync");
    expect(next.admission["local/sidecar"].state).toBe("unavailable");
  });

  it("a complete snapshot is applied, and a withheld remote member is not asked for", () => {
    const p = hydrate(snapshot("local", 1, {}));
    expect(
      applyLayoutInput(p, { kind: "snapshot", replies: [live(snapshot("local", 2, {}))] }).outcome,
    ).toBe("applied");
    const withRemote = applyLayoutInput(p, {
      kind: "snapshot",
      replies: [live(snapshot("local", 1, {})), live(snapshot("box", 1, {}), REMOTE)],
    }).next;
    expect(
      applyLayoutInput(withRemote, {
        kind: "snapshot",
        replies: [live(snapshot("local", 1, {}))],
      }).outcome,
    ).toBe("applied");
  });

  it("an exited entry this viewer closed while another viewer shows it is hidden here", () => {
    const p = hydrate(
      snapshot("e1", 1, {
        [K]: layout([
          entry("x", { state: { kind: "exited", exitCode: 0, signal: null } }),
          entry("a"),
        ]),
      }),
    );
    const { next, outcome } = applyLayoutInput(p, { kind: "dismissed", session: "x" });
    expect(outcome).toBe("applied");
    expect(next.byKey[K].tabs.map((t) => t.id)).toEqual(["a"]);
    expect(applyLayoutInput(next, { kind: "dismissed", session: "x" }).outcome).toBe("stale");
  });
});

describe("applyLayoutInput — admitted members", () => {
  it("a remote Machine's layout without a private authority is held but never composed", () => {
    const { next } = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [
        live(snapshot("mac", 1, { [K]: layout([entry("local-tab")]) })),
        live(snapshot("box", 1, { [K]: layout([entry("remote-tab")]) }), REMOTE),
      ],
    });
    expect(next.byKey[K].tabs.map((t) => t.id)).toEqual(["local-tab"]);
    expect(next.members["box/remote"].workspaces[K].tabs[0].session).toBe("remote-tab");
    expect(sessionTarget(next, "remote-tab")).toEqual({
      ...target("box", REMOTE),
      session: "remote-tab",
    });
  });

  it("a remote change without a private authority is applied to its member, never rendered", () => {
    const p = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [live(snapshot("box", 1, {}), REMOTE)],
    }).next;
    const { next, outcome } = applyLayoutInput(p, {
      kind: "change",
      target: target("box", REMOTE),
      change: { epoch: "box", rev: 2, workspace: K, layout: layout([entry("r")]) },
    });
    expect(outcome).toBe("applied");
    expect(next.members["box/remote"].workspaces[K]).toBeDefined();
    expect(next.byKey[K]).toBeUndefined();
  });

  it("historical and unavailable replies record the admission but compose nothing", () => {
    const frozen = snapshot("old", 1, { [K]: layout([entry("frozen")]) });
    const { next } = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [
        {
          mode: "historical",
          member: SIDECAR,
          admissionRevision: "4",
          frozen: {
            lastNamespace: null,
            serverId: "shared",
            epoch: "old",
            snapshot: frozen,
            reason: "unreachable",
          },
        },
        { mode: "unavailable", member: REMOTE, admissionRevision: "2" },
      ],
    });
    expect(next.byKey).toEqual({});
    expect(next.members).toEqual({});
    expect(next.admission["local/sidecar"].state).toBe("historical");
    expect(next.admission["box/remote"].state).toBe("unavailable");
    expect(sessionTarget(next, "frozen")).toBeUndefined();
  });

  it("admission revisions compare exactly past 2^53, and an old or duplicate row is dropped", () => {
    const high = "9007199254740993";
    const p = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [live(snapshot("mac", 1, { [K]: layout([entry("a")]) }), IN_PROCESS, high)],
    }).next;
    // As Numbers these two are equal: only an exact comparison sees the row is older.
    const older = applyLayoutInput(p, {
      kind: "admission",
      row: row(IN_PROCESS, "9007199254740992"),
    });
    expect(older).toEqual({ next: p, outcome: "stale" });
    expect(applyLayoutInput(p, { kind: "admission", row: row(IN_PROCESS, high) }).outcome).toBe(
      "stale",
    );
  });

  it("a re-admission in the same lifetime keeps the layout under the new revision", () => {
    const p = hydrate(snapshot("mac", 1, { [K]: layout([entry("a")]) }));
    const readmitted: RuntimeAdmissionRow = {
      member: IN_PROCESS,
      revision: "2",
      state: "live",
      namespace: target("mac").namespace,
      historical: null,
    };
    const { next, outcome } = applyLayoutInput(p, { kind: "admission", row: readmitted });
    expect(outcome).toBe("applied");
    expect(next.byKey[K]).toBe(p.byKey[K]);
    expect(sessionTarget(next, "a")?.admissionRevision).toBe("2");
  });

  it("a new lifetime keeps the tabs shown and asks for a re-fetch that replaces them", () => {
    const p = hydrate(snapshot("mac", 1, { [K]: layout([entry("a")]) }));
    const { next, outcome } = applyLayoutInput(p, {
      kind: "admission",
      row: row(IN_PROCESS, "2", "mac-2"),
    });
    expect(outcome).toBe("resync");
    expect(next.byKey[K]).toBe(p.byKey[K]);
    const { next: fetched } = applyLayoutInput(next, {
      kind: "snapshot",
      replies: [live(snapshot("mac-2", 0, { [K]: layout([entry("b")]) }), IN_PROCESS, "2")],
    });
    expect(fetched.byKey[K].tabs.map((t) => t.id)).toEqual(["b"]);
  });

  it("an unreachable member keeps its tabs shown, but they resolve to no target", () => {
    const p = hydrate(snapshot("mac", 1, {}), snapshot("side", 1, { [K]: layout([entry("s")]) }));
    const unavailable: RuntimeAdmissionRow = {
      member: SIDECAR,
      revision: "2",
      state: "unavailable",
      namespace: null,
      historical: null,
    };
    const { next } = applyLayoutInput(p, { kind: "admission", row: unavailable });
    expect(next.byKey[K]).toBe(p.byKey[K]);
    expect(sessionTarget(next, "s")).toBeUndefined();
    // Its changes sealed before it went unreachable are dropped, not re-fetched.
    const late = applyLayoutInput(next, {
      kind: "change",
      target: target("side", SIDECAR),
      change: { epoch: "side", rev: 2, workspace: K, layout: layout([entry("x")]) },
    });
    expect(late).toEqual({ next, outcome: "stale" });
    // A re-fetch answering unavailable keeps them too.
    const refetched = applyLayoutInput(next, {
      kind: "snapshot",
      replies: [
        live(snapshot("mac", 1, {})),
        { mode: "unavailable", member: SIDECAR, admissionRevision: "2" },
      ],
    });
    expect(refetched.next.byKey[K].tabs.map((t) => t.id)).toEqual(["s"]);
  });

  it("a removed member stays removed: a reply sealed before the removal cannot bring it back", () => {
    const p = hydrate(snapshot("mac", 1, {}), snapshot("side", 1, { [K]: layout([entry("s")]) }));
    const removed = applyLayoutInput(p, { kind: "admission", row: row(SIDECAR, "5") });
    expect(removed.outcome).toBe("applied");
    expect(removed.next.byKey[K]).toBeUndefined();
    const late = applyLayoutInput(removed.next, {
      kind: "snapshot",
      replies: [
        live(snapshot("mac", 1, {})),
        live(snapshot("side", 2, { [K]: layout([entry("s")]) }), SIDECAR, "4"),
      ],
    });
    expect(late.next.byKey[K]).toBeUndefined();
    expect(late.next.members["local/sidecar"]).toBeUndefined();
    expect(late.next.admission["local/sidecar"].state).toBe("removed");
  });

  it("a change sealed under an older admission is dropped; a newer or unknown one re-fetches", () => {
    const p = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [live(snapshot("mac", 1, { [K]: layout([entry("a")]) }), IN_PROCESS, "3")],
    }).next;
    const change = { epoch: "mac", rev: 2, workspace: K, layout: layout([entry("x")]) };
    const at = (member: RuntimeMemberAddress, revision: string) =>
      applyLayoutInput(p, { kind: "change", target: target("mac", member, revision), change });
    expect(at(IN_PROCESS, "2")).toEqual({ next: p, outcome: "stale" });
    expect(at(IN_PROCESS, "4")).toEqual({ next: p, outcome: "resync" });
    expect(at(SIDECAR, "3")).toEqual({ next: p, outcome: "resync" });
  });

  it("two local members sharing a ServerId compose together, each tab targeting its own", () => {
    const p = hydrate(
      snapshot("in-process", 1, { [K]: layout([entry("a")]) }),
      snapshot("sidecar", 1, { [K]: layout([entry("b")]) }),
    );
    expect(p.byKey[K].tabs.map((t) => t.id)).toEqual(["a", "b"]);
    expect(sessionTarget(p, "a")).toEqual({ ...target("in-process"), session: "a" });
    expect(sessionTarget(p, "b")).toEqual({ ...target("sidecar", SIDECAR), session: "b" });
  });

  it("sessionTarget never picks a member for an ambiguous or unknown wire id", () => {
    const p = hydrate(
      snapshot("in-process", 1, { [K]: layout([entry("same")]) }),
      snapshot("sidecar", 1, { [K]: layout([entry("same")]) }),
    );
    expect(sessionTarget(p, "same")).toBeUndefined();
    expect(sessionTarget(p, "missing")).toBeUndefined();
  });
});

/** A remote Machine's private shell tab, as its host tags it. */
function remoteEntry(session: string, fields: Partial<TerminalTabEntry> = {}): TerminalTabEntry {
  return entry(session, {
    tags: { remoteHandle: `handle-${session}`, remoteRequestId: `request-${session}` },
    ...fields,
  });
}

/** The team `K` belongs to, as a remote host's descriptor names it. */
const TEAM = { displayName: "w", kind: "generic", org: "acme", team: "core" };

function privateLive(snap: TerminalLayoutSnapshot, revision = "1"): RuntimeLayoutReply {
  return {
    mode: "live",
    ...target(snap.epoch, REMOTE, revision),
    snapshot: snap,
    privateAuthorityEpoch: "7",
  };
}

/** This Mac with tab `a`, and the Machine with remote tab `r`, both in `K`. */
function sideBySide(): TerminalLayoutProjection {
  return applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
    kind: "snapshot",
    replies: [
      live(snapshot("mac", 1, { [K]: layout([entry("a")], { lastActive: "a" }) })),
      privateLive(snapshot("box", 1, { [K]: layout([remoteEntry("r")], { descriptor: TEAM }) })),
    ],
  }).next;
}

function dropped(revision: string, frozenTabs: TerminalTabEntry[]): RuntimeAdmissionRow {
  return {
    member: REMOTE,
    revision,
    privateAuthorityEpoch: "7",
    state: "historical",
    namespace: null,
    historical: {
      lastNamespace: target("box", REMOTE).namespace,
      serverId: "shared",
      epoch: "box",
      snapshot: snapshot("box", 1, { [K]: layout(frozenTabs, { descriptor: TEAM }) }),
      reason: "unreachable",
    },
  };
}

describe("applyLayoutInput — remote tabs beside This Mac's", () => {
  it("a privately admitted Machine's tabs show after This Mac's, marked with their origin", () => {
    const p = sideBySide();
    expect(p.byKey[K].tabs.map((t) => t.id)).toEqual(["a", "r"]);
    expect(p.byKey[K].tabs[0].origin).toBeUndefined();
    expect(p.byKey[K].tabs[1].origin).toEqual({
      machineId: "box",
      member: "box/remote",
      handle: "handle-r",
      requestId: "request-r",
    });
    expect(memberOfSession(p, "r")?.key).toBe("box/remote");
    expect(sessionTarget(p, "r")).toEqual({ ...target("box", REMOTE), session: "r" });
  });

  it("only the Machine's private shell tabs of the same team show", () => {
    const p = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [
        privateLive(
          snapshot("box", 1, {
            [K]: layout([remoteEntry("r"), entry("untagged")], { descriptor: TEAM }),
            "acme:other|w": layout([remoteEntry("o")], { descriptor: TEAM }),
          }),
        ),
      ],
    }).next;
    expect(p.byKey[K].tabs.map((t) => t.id)).toEqual(["r"]);
    expect(p.byKey["acme:other|w"]).toBeUndefined();
  });

  it("two teams' same workspace id each show only their own team's remote session", () => {
    const other = "acme:infra|w";
    const p = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [
        privateLive(
          snapshot("box", 1, {
            [K]: layout([remoteEntry("core-r")], { descriptor: TEAM }),
            [other]: layout([remoteEntry("infra-r")], {
              descriptor: { ...TEAM, team: "infra" },
            }),
          }),
        ),
      ],
    }).next;
    expect(p.byKey[K].tabs.map((t) => t.id)).toEqual(["core-r"]);
    expect(p.byKey[other].tabs.map((t) => t.id)).toEqual(["infra-r"]);
  });

  it("a dropped Machine keeps its tabs in place, offline, with no actionable target", () => {
    const p = sideBySide();
    const { next } = applyLayoutInput(p, { kind: "admission", row: dropped("2", []) }, 1000);
    expect(next.byKey[K].tabs.map((t) => t.id)).toEqual(["a", "r"]);
    expect(next.byKey[K].tabs[1].origin?.offlineSince).toBe(1000);
    expect(sessionTarget(next, "r")).toBeUndefined();
    // A second drop keeps the first time.
    const again = applyLayoutInput(
      next,
      {
        kind: "admission",
        row: {
          member: REMOTE,
          revision: "3",
          privateAuthorityEpoch: "7",
          state: "unavailable",
          namespace: null,
          historical: null,
        },
      },
      5000,
    ).next;
    expect(again.byKey[K].tabs[1].origin?.offlineSince).toBe(1000);
  });

  it("a Machine back in the same lifetime turns live in place and re-reads its layout", () => {
    const offline = applyLayoutInput(sideBySide(), {
      kind: "admission",
      row: dropped("2", []),
    }).next;
    const back = applyLayoutInput(offline, {
      kind: "admission",
      row: {
        member: REMOTE,
        revision: "3",
        state: "live",
        namespace: target("box", REMOTE).namespace,
        historical: null,
        privateAuthorityEpoch: "7",
      },
    });
    expect(back.outcome).toBe("resync");
    expect(back.next.byKey[K].tabs.map((t) => t.id)).toEqual(["a", "r"]);
    expect(back.next.byKey[K].tabs[1].origin?.offlineSince).toBeUndefined();
  });

  it("a reconnect to a new lifetime replaces the offline tabs with the live ones, no duplicates", () => {
    const offline = applyLayoutInput(sideBySide(), {
      kind: "admission",
      row: dropped("2", []),
    }).next;
    const fetched = applyLayoutInput(offline, {
      kind: "snapshot",
      replies: [
        live(snapshot("mac", 1, { [K]: layout([entry("a")]) })),
        privateLive(
          snapshot("box-2", 1, { [K]: layout([remoteEntry("r")], { descriptor: TEAM }) }),
          "3",
        ),
      ],
    }).next;
    expect(fetched.byKey[K].tabs.map((t) => t.id)).toEqual(["a", "r"]);
    expect(fetched.byKey[K].tabs[1].origin?.offlineSince).toBeUndefined();
    expect(sessionTarget(fetched, "r")?.namespace.epoch).toBe("box-2");
  });

  it("a Machine first seen offline shows the layout the app last read from it", () => {
    const { next } = applyLayoutInput(
      EMPTY_TERMINAL_LAYOUT,
      {
        kind: "snapshot",
        replies: [
          {
            mode: "historical",
            member: REMOTE,
            admissionRevision: "2",
            privateAuthorityEpoch: "7",
            frozen: dropped("2", [remoteEntry("r")]).historical!,
          },
        ],
      },
      42,
    );
    expect(next.byKey[K].tabs.map((t) => t.id)).toEqual(["r"]);
    expect(next.byKey[K].tabs[0].origin?.offlineSince).toBe(42);
  });

  it("only removing the Machine closes its tabs", () => {
    const { next } = applyLayoutInput(sideBySide(), {
      kind: "admission",
      row: {
        member: REMOTE,
        revision: "2",
        privateAuthorityEpoch: "7",
        state: "removed",
        namespace: null,
        historical: null,
      },
    });
    expect(next.byKey[K].tabs.map((t) => t.id)).toEqual(["a"]);
  });

  it("this viewer's focus on a remote tab wins until a local focus takes it back", () => {
    const p = sideBySide();
    expect(p.byKey[K].active).toBe("a");
    const focused = applyLayoutInput(p, { kind: "focused", key: K, session: "r" }).next;
    expect(focused.byKey[K].active).toBe("r");
    // The local host focusing a new tab takes the focus back.
    const opened = applyLayoutInput(focused, {
      kind: "change",
      target: target("mac"),
      change: {
        epoch: "mac",
        rev: 2,
        workspace: K,
        layout: layout([entry("a"), entry("b")], { lastActive: "b" }),
      },
    }).next;
    expect(opened.byKey[K].active).toBe("b");
    const refocused = applyLayoutInput(opened, { kind: "focused", key: K, session: "r" }).next;
    expect(
      applyLayoutInput(refocused, { kind: "focused", key: K, session: null }).next.byKey[K].active,
    ).toBe("b");
  });

  it("restored remote focus waits for its Machine without overriding a later local choice", () => {
    const restored = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "focused",
      key: K,
      session: "s",
    }).next;
    const local = live(snapshot("mac", 1, { [K]: layout([entry("a")]) }));
    const localOnly = applyLayoutInput(restored, { kind: "snapshot", replies: [local] }).next;
    expect(localOnly.byKey[K].active).toBe("a");
    expect(localOnly.focus?.[K]).toBe("s");
    const remote = privateLive(
      snapshot("box", 1, {
        [K]: layout([remoteEntry("r"), remoteEntry("s")], { descriptor: TEAM }),
      }),
    );
    const connected = applyLayoutInput(localOnly, {
      kind: "snapshot",
      replies: [local, remote],
    }).next;
    expect(connected.byKey[K].active).toBe("s");
    const chosenLocal = applyLayoutInput(localOnly, {
      kind: "focused",
      key: K,
      session: null,
    }).next;
    expect(
      applyLayoutInput(chosenLocal, { kind: "snapshot", replies: [local, remote] }).next.byKey[K]
        .active,
    ).toBe("a");
  });

  it("one grid: This Mac's paned tabs and shape plus the viewer's remote cells; a Machine's own grid is ignored", () => {
    const p = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [
        live(
          snapshot("mac", 1, {
            [K]: layout([entry("a"), entry("b")], {
              grid: { paned: ["a"], preset: "grid", collapsed: false },
            }),
          }),
        ),
        privateLive(
          snapshot("box", 1, {
            [K]: layout([remoteEntry("r"), remoteEntry("s")], {
              descriptor: TEAM,
              grid: { paned: ["s"], preset: "rows", collapsed: true },
            }),
          }),
        ),
      ],
    }).next;
    expect(p.byKey[K].grid).toEqual({ paned: ["a"], preset: "grid", collapsed: false });
    const arranged = applyLayoutInput(p, {
      kind: "viewer-grid",
      key: K,
      grid: { paned: [{ session: "r", member: "box/remote" }], preset: "rows", collapsed: true },
    }).next;
    // The shape stays This Mac's.
    expect(arranged.byKey[K].grid).toEqual({ paned: ["a", "r"], preset: "grid", collapsed: false });
  });

  it("a workspace with only remote tabs takes its grid shape from the viewer", () => {
    const p = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [
        privateLive(snapshot("box", 1, { [K]: layout([remoteEntry("r")], { descriptor: TEAM }) })),
      ],
    }).next;
    const arranged = applyLayoutInput(p, {
      kind: "viewer-grid",
      key: K,
      grid: { paned: [{ session: "r", member: "box/remote" }], preset: "grid", collapsed: false },
    }).next;
    expect(arranged.byKey[K].grid).toEqual({ paned: ["r"], preset: "grid", collapsed: false });
  });

  it("a remote cell stays paned while its Machine is offline and goes once its tab is gone", () => {
    const arranged = applyLayoutInput(sideBySide(), {
      kind: "viewer-grid",
      key: K,
      grid: { paned: [{ session: "r", member: "box/remote" }], preset: "grid", collapsed: false },
    }).next;
    const offline = applyLayoutInput(arranged, { kind: "admission", row: dropped("2", []) }).next;
    expect(offline.byKey[K].grid.paned).toEqual(["r"]);
    // Back live without the tab: it ended there.
    const gone = applyLayoutInput(offline, {
      kind: "snapshot",
      replies: [
        live(snapshot("mac", 1, { [K]: layout([entry("a")]) })),
        privateLive(snapshot("box-2", 1, { [K]: layout([], { descriptor: TEAM }) }), "3"),
      ],
    }).next;
    expect(gone.grids?.[K]?.paned).toEqual([]);
    expect(gone.byKey[K].grid.paned).toEqual([]);
  });

  it("saved remote cells are kept before their Machine is read, and shown once it is", () => {
    const restored = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "viewer-grids",
      grids: {
        [K]: {
          paned: [{ session: "r", member: "box/remote" }],
          preset: "grid",
          collapsed: false,
        },
      },
    }).next;
    const localOnly = applyLayoutInput(restored, {
      kind: "snapshot",
      replies: [live(snapshot("mac", 1, { [K]: layout([entry("a")]) }))],
    }).next;
    expect(localOnly.grids?.[K]?.paned).toEqual([{ session: "r", member: "box/remote" }]);
    const connected = applyLayoutInput(localOnly, {
      kind: "snapshot",
      replies: [
        live(snapshot("mac", 1, { [K]: layout([entry("a")]) })),
        privateLive(snapshot("box", 1, { [K]: layout([remoteEntry("r")], { descriptor: TEAM }) })),
      ],
    }).next;
    expect(connected.byKey[K].grid.paned).toEqual(["r"]);
  });

  it("a remote tab can be dismissed like a local one", () => {
    const { next, outcome } = applyLayoutInput(sideBySide(), { kind: "dismissed", session: "r" });
    expect(outcome).toBe("applied");
    expect(next.byKey[K].tabs.map((t) => t.id)).toEqual(["a"]);
  });
});

describe("remoteLayoutPending", () => {
  const T0 = 1_000_000;
  const unavailable = (revision: string): RuntimeAdmissionRow => ({
    member: REMOTE,
    revision,
    state: "unavailable",
    namespace: null,
    historical: null,
  });
  const admit = (p: TerminalLayoutProjection, r: RuntimeAdmissionRow, now: number) =>
    applyLayoutInput(p, { kind: "admission", row: r }, now).next;

  it("waits on a Machine admitted live until its layout is held, and only for a while", () => {
    const p = admit(EMPTY_TERMINAL_LAYOUT, row(REMOTE, "1", "e1"), T0);
    expect(remoteLayoutPending(p, [], T0 + REMOTE_LAYOUT_WAIT_MS - 1)).toBe(true);
    expect(remoteLayoutPending(p, [], T0 + REMOTE_LAYOUT_WAIT_MS)).toBe(false);
    expect(remoteLayoutWaitEnds(p)).toEqual([T0 + REMOTE_LAYOUT_WAIT_MS]);
    const held = applyLayoutInput(p, {
      kind: "snapshot",
      replies: [live(snapshot("e1", 1, {}), REMOTE)],
    }).next;
    expect(remoteLayoutPending(held, [], T0)).toBe(false);
    // Its wait is over: the time it was admitted is no longer kept.
    expect(held.admittedAt).toEqual({});
    expect(remoteLayoutWaitEnds(held)).toEqual([]);
  });

  it("stops waiting when the clock goes back past the wait's start", () => {
    const p = admit(EMPTY_TERMINAL_LAYOUT, unavailable("1"), T0);
    expect(remoteLayoutPending(p, [], T0 - 1)).toBe(false);
    expect(remoteLayoutPending(p, { box: T0 }, T0 - 1)).toBe(false);
  });

  it("waits on a Machine being connected from when it set out, and only for a while", () => {
    expect(remoteLayoutPending(EMPTY_TERMINAL_LAYOUT, { box: T0 }, T0)).toBe(true);
    expect(
      remoteLayoutPending(EMPTY_TERMINAL_LAYOUT, { box: T0 }, T0 + REMOTE_LAYOUT_WAIT_MS - 1),
    ).toBe(true);
    expect(
      remoteLayoutPending(EMPTY_TERMINAL_LAYOUT, { box: T0 }, T0 + REMOTE_LAYOUT_WAIT_MS),
    ).toBe(false);
  });

  it("waits on a Machine admitted unavailable only for a while, so nothing waits on it forever", () => {
    const p = admit(EMPTY_TERMINAL_LAYOUT, unavailable("1"), T0);
    expect(remoteLayoutPending(p, [], T0)).toBe(true);
    expect(remoteLayoutPending(p, [], T0 + REMOTE_LAYOUT_WAIT_MS - 1)).toBe(true);
    expect(remoteLayoutPending(p, [], T0 + REMOTE_LAYOUT_WAIT_MS)).toBe(false);
    // A new admission starts a new wait; a re-read of the same one does not.
    const again = admit(p, unavailable("2"), T0 + REMOTE_LAYOUT_WAIT_MS);
    expect(remoteLayoutPending(again, [], T0 + REMOTE_LAYOUT_WAIT_MS + 1)).toBe(true);
    const reread = applyLayoutInput(
      again,
      {
        kind: "snapshot",
        replies: [{ mode: "unavailable", member: REMOTE, admissionRevision: "2" }],
      },
      T0 + 3 * REMOTE_LAYOUT_WAIT_MS,
    ).next;
    expect(remoteLayoutPending(reread, [], T0 + 3 * REMOTE_LAYOUT_WAIT_MS)).toBe(false);
  });

  it("does not wait on a removed or historical Machine, nor on This Mac's members", () => {
    expect(remoteLayoutPending(admit(EMPTY_TERMINAL_LAYOUT, row(REMOTE, "1"), T0), [], T0)).toBe(
      false,
    );
    const local = admit(EMPTY_TERMINAL_LAYOUT, { ...unavailable("1"), member: SIDECAR }, T0);
    expect(remoteLayoutPending(local, [], T0)).toBe(false);
    expect(remoteLayoutPending(null, [], T0)).toBe(false);
  });

  it("waits on a Machine being connected until its layout is held", () => {
    expect(remoteLayoutPending(EMPTY_TERMINAL_LAYOUT, ["box"], T0)).toBe(true);
    const held = applyLayoutInput(EMPTY_TERMINAL_LAYOUT, {
      kind: "snapshot",
      replies: [live(snapshot("e1", 1, {}), REMOTE)],
    }).next;
    expect(remoteLayoutPending(held, ["box"], T0)).toBe(false);
  });
});
