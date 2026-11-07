// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import type {
  ContextQuery,
  ContextQueryResult,
  ContextRow,
} from "@/shared/lib/context-query/types";

// The one engine lives in Rust: the island only builds a query and renders the
// answer. Mock the command seam + the index-change event.
const queryContext = vi.fn<(q: ContextQuery, page?: string) => Promise<ContextQueryResult>>();
vi.mock("@/tauri/commands", () => ({
  queryContext: (q: ContextQuery, page?: string) => queryContext(q, page),
}));
let emitIndexChanged: (() => void) | null = null;
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((_event: string, handler: (e: { payload: null }) => void) => {
    emitIndexChanged = () => handler({ payload: null });
    return Promise.resolve(() => {
      emitIndexChanged = null;
    });
  }),
}));

import { CsQueryIsland } from "./cs-query-view";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

function row(fileName: string, fields: Record<string, unknown>): ContextRow {
  return {
    id: String(fields.story_id ?? fileName),
    path: `/ctx/epics/${fileName}`,
    relPath: `epics/${fileName}`,
    fileName,
    org: "acme",
    team: "default",
    layer: "team",
    kind: "story",
    mtime: 1,
    title: null,
    fields,
  };
}

function result(rows: ContextRow[], extra: Partial<ContextQueryResult> = {}): ContextQueryResult {
  return {
    schemaVersion: 1,
    total: rows.length,
    rows,
    textScanTruncated: false,
    totalApproximate: false,
    indexing: false,
    generation: 1,
    ...extra,
  };
}

const ALPHA = row("alpha.md", {
  story_id: "AL-1",
  type: "epic",
  status: "ready-for-dev",
  team: "platform",
});
const CHARLIE = row("charlie.md", { story_id: "CH-3", type: "epic", status: "ready-for-dev" });
const BRAVO = row("bravo.md", { story_id: "BR-2", type: "epic", status: "ready-for-dev" });

const EPIC_READY = "tag: type=epic\nstatus: ready-for-dev\nview: table";
const BASE = "/ctx/epics/README.md";

beforeEach(() => {
  cleanup();
  queryContext.mockReset();
  emitIndexChanged = null;
});

describe("CsQueryIsland — the engine's answer, rendered (AC-1/AC-2)", () => {
  it("sends one ContextQuery (own org by default) and renders exactly the rows returned", async () => {
    queryContext.mockResolvedValue(result([ALPHA, CHARLIE]));
    render(<CsQueryIsland code={EPIC_READY} tree={null} basePath={BASE} />);

    await waitFor(() => expect(screen.getByText("alpha.md")).toBeTruthy());
    expect(queryContext).toHaveBeenCalledTimes(1);
    // The block runs as its doc's page: private items only from the doc's org.
    expect(queryContext.mock.calls[0][1]).toBe(BASE);
    expect(queryContext.mock.calls[0][0]).toEqual({
      where: { kinds: ["story", "bug", "spike"], sameOrgAs: BASE },
      filters: [
        { key: "type", op: "eq", value: "epic" },
        { key: "status", op: "in", value: ["ready-for-dev"] },
      ],
      // Status sorts follow the team's taxonomy order (the default taxonomy here).
      sort: [
        { key: "status", dir: "asc", order: expect.arrayContaining(["todo", "done"]) },
        { key: "story_id", missing: "first" },
      ],
      limit: 200,
    });
    const rows = screen.getAllByRole("row").slice(1);
    expect(rows.map((r) => within(r).getAllByRole("cell")[0].textContent)).toEqual([
      "alpha.md",
      "charlie.md",
    ]);
  });

  it("an explicit `org: all` is footprint-wide (no own-org default)", async () => {
    queryContext.mockResolvedValue(result([]));
    render(<CsQueryIsland code={"org: all\nstatus: todo"} tree={null} basePath={BASE} />);
    await waitFor(() => expect(queryContext).toHaveBeenCalled());
    expect(queryContext.mock.calls[0][0].where).toEqual({
      kinds: ["story", "bug", "spike"],
      orgs: "all",
    });
  });

  it("AC-2: epic + status + @me + priority sort + group by status, as a board", async () => {
    queryContext.mockResolvedValue(
      result([ALPHA, CHARLIE], {
        total: 3,
        groups: [
          { value: "todo", count: 1 },
          { value: "ready-for-dev", count: 2 },
        ],
      }),
    );
    render(
      <CsQueryIsland
        code={
          "epic: SW-OC\nstatus: todo, in-progress\ntag: assigned_to=@me\nsort: priority\nview: board"
        }
        tree={null}
        basePath={BASE}
      />,
    );
    await waitFor(() => expect(screen.getByText("alpha.md")).toBeTruthy());
    const q = queryContext.mock.calls[0][0];
    expect(q.relationships).toEqual({ parentEpic: "SW-OC" });
    expect(q.filters).toContainEqual({ key: "assigned_to", op: "eq", value: "@me" });
    expect(q.sort).toEqual([{ key: "priority", dir: "asc" }]);
    expect(q.groupBy).toBe("status");
    // Lane counts come from the engine's groups (every match, not just shown rows).
    expect(screen.getByText(/ready-for-dev/).textContent).toContain("(2)");
  });
});

describe("CsQueryIsland — live (AC-3)", () => {
  it("re-queries on every index change and re-renders the new answer", async () => {
    queryContext.mockResolvedValueOnce(result([ALPHA, CHARLIE]));
    render(<CsQueryIsland code={EPIC_READY} tree={null} basePath={BASE} />);
    await waitFor(() => expect(screen.getAllByRole("row").slice(1)).toHaveLength(2));
    expect(screen.getByText("live")).toBeTruthy();

    queryContext.mockResolvedValueOnce(result([ALPHA, BRAVO, CHARLIE]));
    await act(async () => emitIndexChanged?.());
    await waitFor(() => expect(screen.getByText("bravo.md")).toBeTruthy());
    expect(screen.getAllByRole("row").slice(1)).toHaveLength(3);

    // A failed refresh keeps the rows and cues them stale.
    queryContext.mockRejectedValueOnce(new Error("index unavailable"));
    await act(async () => emitIndexChanged?.());
    await waitFor(() => expect(screen.getByText("stale")).toBeTruthy());
    expect(screen.getByText("bravo.md")).toBeTruthy();
  });
});

describe("CsQueryIsland — view hints (AC-4)", () => {
  it("view: list renders a flat list", async () => {
    queryContext.mockResolvedValue(result([ALPHA, CHARLIE]));
    render(<CsQueryIsland code={"tag: type=epic\nview: list"} tree={null} />);
    await waitFor(() => expect(screen.getAllByRole("listitem").length).toBe(2));
  });

  it("table columns resolve a frontmatter key by normalized name (Team → team)", async () => {
    queryContext.mockResolvedValue(result([ALPHA]));
    render(<CsQueryIsland code={"tag: type=epic\ncolumns: name, Team"} tree={null} />);
    await waitFor(() => expect(screen.getByText("platform")).toBeTruthy());
  });

  it("caps with the view's limit and says so", async () => {
    queryContext.mockResolvedValue(result([ALPHA], { total: 5 }));
    render(<CsQueryIsland code={"limit: 1"} tree={null} />);
    await waitFor(() => expect(screen.getByText(/of 5, capped at 1/)).toBeTruthy());
    expect(queryContext.mock.calls[0][0].limit).toBe(1);
  });
});

describe("CsQueryIsland — resilience + empty (AC-6, AC-7)", () => {
  it("shows Loading… until the first answer", () => {
    queryContext.mockReturnValue(new Promise(() => {}));
    render(<CsQueryIsland code={EPIC_READY} tree={null} />);
    expect(screen.getByText(/Loading/)).toBeTruthy();
    expect(screen.queryByText(/No matching docs/)).toBeNull();
  });

  it("empty result → quiet 'No matching docs' placeholder, not an error", async () => {
    queryContext.mockResolvedValue(result([]));
    render(<CsQueryIsland code={"status: nonexistent-status"} tree={null} />);
    await waitFor(() => expect(screen.getByText(/No matching docs/)).toBeTruthy());
  });

  it("a rejected first query renders an error card, not a white-screen", async () => {
    queryContext.mockRejectedValue(new Error("invalid query: unknown variant"));
    render(<CsQueryIsland code={EPIC_READY} tree={null} />);
    await waitFor(() => expect(screen.getByText(/couldn't run its query/i)).toBeTruthy());
    expect(screen.getByText(/unknown variant/)).toBeTruthy();
  });

  it("row click navigates via the viewer's onNavigate (read-only nav path)", async () => {
    queryContext.mockResolvedValue(result([ALPHA]));
    const onNavigate = vi.fn();
    render(<CsQueryIsland code={EPIC_READY} tree={null} onNavigate={onNavigate} />);
    await waitFor(() => expect(screen.getByText("alpha.md")).toBeTruthy());
    fireEvent.click(screen.getByText("alpha.md"));
    expect(onNavigate).toHaveBeenCalledWith("/ctx/epics/alpha.md");
  });

  it("a block with no containing doc runs as an unknown page (no private items)", async () => {
    queryContext.mockResolvedValue(result([]));
    render(<CsQueryIsland code={"org: all\nlayer: private"} tree={null} />);
    await waitFor(() => expect(queryContext).toHaveBeenCalled());
    expect(queryContext.mock.calls[0][1]).toBe("");
  });

  it("a scope that names no sub-epic is an error card, not a silent path prefix", () => {
    const tree = {
      dirPath: "",
      dirName: "e",
      absoluteDirPath: "/ctx/e",
      stories: [],
      docs: [],
      subEpics: [],
      orderError: null,
      mainDoc: null,
    } as unknown as DerivedPipelineTree;
    render(<CsQueryIsland code={"scope: no-such-epic"} tree={tree} />);
    expect(screen.getByText(/scope names no folder/)).toBeTruthy();
    expect(queryContext).not.toHaveBeenCalled();
  });
});
