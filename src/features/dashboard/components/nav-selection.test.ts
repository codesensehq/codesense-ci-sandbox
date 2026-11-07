// SPDX-License-Identifier: MIT
/**
 * SW-DNS-1: the nav selection a dashboard reads — scope (the doc's own team, paths
 * confined to its content root) and identity (story / epic from the index).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

import type { ContentLocation } from "@/tauri/commands";
import type { ContextQuery, ContextRow } from "@/shared/lib/context-query/types";

import {
  clearNavSelectionCaches,
  navInputsFor,
  resolveDocTeam,
  resolveNavSelection,
  type NavSelectionDeps,
} from "./nav-selection";

const ROOT_A = "/ctx/acme/a/content";
const ROOT_B = "/ctx/acme/b/content";

function loc(path: string): ContentLocation | null {
  if (path.startsWith(ROOT_A))
    return { org: "acme", team: "a", layer: "team", contentRoot: ROOT_A };
  if (path.startsWith(ROOT_B))
    return { org: "acme", team: "b", layer: "team", contentRoot: ROOT_B };
  if (path.startsWith("/ctx/acme/private"))
    return { org: "acme", team: null, layer: "private", contentRoot: "/ctx/acme/private" };
  return null;
}

function row(partial: Partial<ContextRow> & { path: string }): ContextRow {
  const relPath = partial.path.slice(ROOT_A.length + 1);
  return {
    id: partial.id ?? relPath.split("/").pop()!.replace(/\.md$/, ""),
    relPath,
    fileName: relPath.split("/").pop()!,
    org: "acme",
    team: "a",
    layer: "team",
    kind: partial.kind ?? "doc",
    mtime: null,
    title: partial.title ?? null,
    fields: partial.fields ?? {},
    relationships: partial.relationships,
    ...partial,
  };
}

const STORY = `${ROOT_A}/epics/billing/stories/BIL-2-invoices.md`;
const EPIC_README = `${ROOT_A}/epics/billing/README.md`;
const SUB_README = `${ROOT_A}/epics/billing/payments/README.md`;
const ROWS: ContextRow[] = [
  row({
    path: STORY,
    id: "BIL-2",
    kind: "story",
    title: "Invoices",
    relationships: {
      dependsOn: [],
      dependents: [],
      supportingDocs: [],
      backlinks: [],
      parentEpic: "BIL",
      folderEpic: "billing",
    },
  }),
  row({
    path: EPIC_README,
    id: "README",
    kind: "epic",
    title: "Billing",
    fields: { epic_id: "BIL" },
  }),
  row({
    path: SUB_README,
    id: "README",
    kind: "doc",
    title: "Payments",
    fields: { type: "sub-epic", sub_epic: "BIL-PAY", epic_id: "BIL" },
  }),
];

function makeDeps(): NavSelectionDeps & { query: ReturnType<typeof vi.fn> } {
  const query = vi.fn(async (q: ContextQuery) => {
    const w = q.where ?? {};
    let rows = ROWS.filter((r) => !w.kinds || w.kinds.includes(r.kind));
    if (typeof w.pathPrefix === "string")
      rows = rows.filter((r) => r.path.startsWith(w.pathPrefix!));
    if (q.filters?.some((f) => f.key === "type"))
      rows = rows.filter((r) => String(r.fields.type ?? "").includes("sub"));
    return { rows, total: rows.length } as never;
  });
  return { docContext: vi.fn(async (p: string) => loc(p)), query };
}

beforeEach(() => clearNavSelectionCaches());

describe("resolveDocTeam", () => {
  const active = { org: "acme", team: "b" };
  it("a pane belongs to the team on screen", async () => {
    const deps = makeDeps();
    expect(
      await resolveDocTeam({ docPath: "/ctx/acme/a/panes/p.html", pane: true }, active, deps),
    ).toEqual(active);
  });
  it("a message doc belongs to its message's team", async () => {
    const deps = makeDeps();
    expect(
      await resolveDocTeam({ docPath: "x", message: { org: "acme", team: "a" } }, active, deps),
    ).toEqual({ org: "acme", team: "a" });
  });
  it("a centre doc belongs to the team its file lives in, even when another team is active", async () => {
    const deps = makeDeps();
    expect(await resolveDocTeam({ docPath: `${ROOT_A}/dash.html` }, active, deps)).toEqual({
      org: "acme",
      team: "a",
    });
  });
  it("a private centre doc follows the team on screen", async () => {
    const deps = makeDeps();
    expect(await resolveDocTeam({ docPath: "/ctx/acme/private/d.html" }, active, deps)).toEqual(
      active,
    );
  });
  it("no team on screen and none of its own: null", async () => {
    const deps = makeDeps();
    expect(await resolveDocTeam({ docPath: "/nowhere/d.html" }, null, deps)).toBeNull();
  });
});

describe("navInputsFor", () => {
  it("reads only the given team's slices", () => {
    const state = {
      activeTab: { "acme:a": STORY, "acme:b": `${ROOT_B}/x.md` },
      drillPaths: { "acme:a": `${ROOT_A}/epics/billing` },
    };
    expect(navInputsFor(state, { org: "acme", team: "a" })).toEqual({
      activePath: STORY,
      drillPath: `${ROOT_A}/epics/billing`,
    });
    expect(navInputsFor(state, { org: "acme", team: "c" })).toEqual({
      activePath: null,
      drillPath: null,
    });
  });
});

describe("resolveNavSelection", () => {
  const team = { org: "acme", team: "a" };

  it("names the active story, its epic, and the drilled folder's epic", async () => {
    const deps = makeDeps();
    const sel = await resolveNavSelection(
      team,
      { activePath: STORY, drillPath: `${ROOT_A}/epics/billing/stories` },
      "/page.html",
      deps,
    );
    expect(sel).toEqual({
      org: "acme",
      team: "a",
      file: {
        path: STORY,
        relPath: "epics/billing/stories/BIL-2-invoices.md",
        kind: "story",
        id: "BIL-2",
        title: "Invoices",
        epic: "BIL",
      },
      drill: {
        path: `${ROOT_A}/epics/billing/stories`,
        relPath: "epics/billing/stories",
        epic: {
          id: "BIL",
          title: "Billing",
          path: EPIC_README,
          relPath: "epics/billing/README.md",
        },
      },
    });
  });

  it("an epic doc names itself as the epic; a sub-epic folder resolves to the sub-epic", async () => {
    const deps = makeDeps();
    const sel = await resolveNavSelection(
      team,
      { activePath: EPIC_README, drillPath: `${ROOT_A}/epics/billing/payments` },
      "/page.html",
      deps,
    );
    expect(sel?.file).toMatchObject({ kind: "epic", id: "BIL", epic: "BIL" });
    expect(sel?.drill?.epic).toMatchObject({ id: "BIL-PAY", title: "Payments", path: SUB_README });
    // A sub-epic README that also names its parent epic is the sub-epic.
    expect(sel?.drill?.epic?.id).toBe("BIL-PAY");
  });

  it("drilled to the content root is no drill; an unindexed file keeps its path with no identity", async () => {
    const deps = makeDeps();
    const sel = await resolveNavSelection(
      team,
      { activePath: `${ROOT_A}/notes/todo.txt`, drillPath: ROOT_A },
      "/page.html",
      deps,
    );
    expect(sel?.drill).toBeNull();
    expect(sel?.file).toEqual({
      path: `${ROOT_A}/notes/todo.txt`,
      relPath: "notes/todo.txt",
      kind: null,
      id: null,
      title: null,
      epic: null,
    });
  });

  it("a path in another team's content or outside every root never leaks", async () => {
    const deps = makeDeps();
    const sel = await resolveNavSelection(
      team,
      { activePath: `${ROOT_B}/secret.md`, drillPath: "/Users/me/src/repo" },
      "/page.html",
      deps,
    );
    expect(sel).toEqual({ org: "acme", team: "a", file: null, drill: null });
    expect(deps.query).not.toHaveBeenCalled();
  });

  it("caches the epic list per content root until cleared", async () => {
    const deps = makeDeps();
    const inputs = { activePath: null, drillPath: `${ROOT_A}/epics/billing` };
    await resolveNavSelection(team, inputs, "/p", deps);
    const calls = deps.query.mock.calls.length;
    await resolveNavSelection(
      team,
      { ...inputs, drillPath: `${ROOT_A}/epics/billing/stories` },
      "/p",
      deps,
    );
    expect(deps.query.mock.calls.length).toBe(calls);
    clearNavSelectionCaches();
    await resolveNavSelection(team, inputs, "/p", deps);
    expect(deps.query.mock.calls.length).toBeGreaterThan(calls);
  });
});
