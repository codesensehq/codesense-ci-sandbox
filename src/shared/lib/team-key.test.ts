// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";
import {
  buildTeamKey,
  parseTeamKey,
  teamLabelAmong,
  workspaceInTeam,
  useActiveTeamKey,
  useHasActiveTeam,
  teamKeyHasTeam,
  NO_TEAM_SENTINEL,
  buildRootWorkspaceId,
  parseRootWorkspaceId,
  isRootWorkspaceId,
  firstRootIdForTeam,
  adjacentWorkspaceIdAfterClose,
  activeWorkspaceIdForTeam,
  activeWorkspaceForTeam,
  ROOT_NO_REPOS_SENTINEL,
  computeRepoGlyphBase,
  computeTeamGlyphMap,
  sortWorkspacesByCanonicalOrder,
} from "./team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Workspace } from "@/shared/stores/viewer-store";

describe("buildTeamKey", () => {
  it("builds key with team name", () => {
    expect(buildTeamKey("acme", "frontend")).toBe("acme:frontend");
  });

  it("builds key with null team using _none sentinel", () => {
    expect(buildTeamKey("acme", null)).toBe("acme:_none");
  });

  it("preserves slug/team casing verbatim", () => {
    expect(buildTeamKey("AcmeCo", "Mobile-Team")).toBe("AcmeCo:Mobile-Team");
  });

  it("accepts unusual chars in slug or team name (does not validate)", () => {
    expect(buildTeamKey("a.b-c_d", "team.with-chars_1")).toBe("a.b-c_d:team.with-chars_1");
  });
});

describe("parseTeamKey", () => {
  it("round-trips a (orgSlug, teamName) pair", () => {
    const key = buildTeamKey("acme", "frontend");
    expect(parseTeamKey(key)).toEqual({ orgSlug: "acme", teamName: "frontend" });
  });

  it("round-trips a null team", () => {
    const key = buildTeamKey("acme", null);
    expect(parseTeamKey(key)).toEqual({ orgSlug: "acme", teamName: null });
  });

  it("treats `_none` as null team", () => {
    expect(parseTeamKey("acme:_none")).toEqual({ orgSlug: "acme", teamName: null });
  });

  it("handles teamName containing colons by joining the rest", () => {
    // Defensive: while team names with colons aren't expected, the parser
    // splits at the FIRST colon so the orgSlug is unambiguous.
    expect(parseTeamKey("acme:team:with:colons")).toEqual({
      orgSlug: "acme",
      teamName: "team:with:colons",
    });
  });
});

describe("useActiveTeamKey", () => {
  beforeEach(() => {
    useOrgStore.setState({
      activeOrgSlug: null,
      activeTeamName: null,
    });
  });

  it("returns sentinel when no org is active", () => {
    const { result } = renderHook(() => useActiveTeamKey());
    expect(result.current).toBe(NO_TEAM_SENTINEL);
  });

  it("returns key with team when both are set", () => {
    useOrgStore.setState({
      activeOrgSlug: "acme",
      activeTeamName: "frontend",
    });
    const { result } = renderHook(() => useActiveTeamKey());
    expect(result.current).toBe("acme:frontend");
  });

  it("returns key with _none when team is null but org is set", () => {
    useOrgStore.setState({
      activeOrgSlug: "acme",
      activeTeamName: null,
    });
    const { result } = renderHook(() => useActiveTeamKey());
    expect(result.current).toBe("acme:_none");
  });
});

describe("teamKeyHasTeam (SW-AW-6 — both no-team forms have no team)", () => {
  it("is true for a real team key", () => {
    expect(teamKeyHasTeam("acme:frontend")).toBe(true);
  });
  it("is false for the no-org sentinel", () => {
    expect(teamKeyHasTeam(NO_TEAM_SENTINEL)).toBe(false);
  });
  it("is false for the org-set/team-null form `${org}:_none`", () => {
    expect(teamKeyHasTeam("acme:_none")).toBe(false);
  });
});

describe("useHasActiveTeam (SW-AW-6 — reactive real-team gate)", () => {
  beforeEach(() => {
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
  });
  it("is false with no org", () => {
    const { result } = renderHook(() => useHasActiveTeam());
    expect(result.current).toBe(false);
  });
  it("is false with an org but a null team", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: null });
    const { result } = renderHook(() => useHasActiveTeam());
    expect(result.current).toBe(false);
  });
  it("is true only when org AND team are set", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "frontend" });
    const { result } = renderHook(() => useHasActiveTeam());
    expect(result.current).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// T.2 helpers: root workspace IDs + firstRootIdForTeam
// ---------------------------------------------------------------------------

describe("buildRootWorkspaceId", () => {
  it("builds id from structured SourceRepoSlug", () => {
    expect(buildRootWorkspaceId({ owner: "acme", repo: "web" })).toBe("root:acme/web");
  });

  it("builds id from pre-joined `owner/repo` string", () => {
    expect(buildRootWorkspaceId("acme/web")).toBe("root:acme/web");
  });

  it("preserves casing verbatim (no normalisation)", () => {
    expect(buildRootWorkspaceId({ owner: "AcmeCo", repo: "WebApp" })).toBe("root:AcmeCo/WebApp");
  });
});

describe("parseRootWorkspaceId", () => {
  it("round-trips a built root id", () => {
    const id = buildRootWorkspaceId({ owner: "acme", repo: "web" });
    expect(parseRootWorkspaceId(id)).toEqual({ slug: "acme/web" });
  });

  it("returns null for non-root ids (no `root:` prefix)", () => {
    expect(parseRootWorkspaceId("R-12")).toBeNull();
    expect(parseRootWorkspaceId("story-foo-bar")).toBeNull();
    expect(parseRootWorkspaceId("")).toBeNull();
  });

  it("returns null for the legacy plain `root` literal", () => {
    // The legacy ROOT_WORKSPACE_ID constant was "root" without a slug.
    // It's not a valid T.2 root id and parsing should refuse it.
    expect(parseRootWorkspaceId("root")).toBeNull();
  });

  it("returns null for the no-repos sentinel", () => {
    expect(parseRootWorkspaceId(ROOT_NO_REPOS_SENTINEL)).toBeNull();
  });

  it("recovers slugs containing dots, dashes, underscores", () => {
    expect(parseRootWorkspaceId("root:9north-co/smart-web")).toEqual({
      slug: "9north-co/smart-web",
    });
    expect(parseRootWorkspaceId("root:user_one/repo.with.dots")).toEqual({
      slug: "user_one/repo.with.dots",
    });
  });
});

describe("isRootWorkspaceId", () => {
  it("is true for valid root ids", () => {
    expect(isRootWorkspaceId("root:acme/web")).toBe(true);
  });

  it("is false for the legacy plain `root` literal", () => {
    expect(isRootWorkspaceId("root")).toBe(false);
  });

  it("is false for story ids and the no-repos sentinel", () => {
    expect(isRootWorkspaceId("R-12")).toBe(false);
    expect(isRootWorkspaceId(ROOT_NO_REPOS_SENTINEL)).toBe(false);
  });
});

describe("firstRootIdForTeam", () => {
  // SW-GS-2a 4-kind model: every kind is terminal-bearing (the zombie
  // terminal-less epic is gone), so `firstRootIdForTeam` returns the FIRST
  // workspace of any kind, else the no-repos sentinel for an empty/absent team.
  const epic = (id: string): Workspace => ({
    id,
    kind: "epic",
    cwd: "",
    epicDirPath: id,
    status: "ready",
  });
  const integration = (id: string): Workspace => ({
    id,
    kind: "epic",
    cwd: "",
    epicDirPath: "",
    status: "ready",
  });
  const story = (id: string): Workspace => ({
    id,
    kind: "story",
    cwd: "/tmp/wt",
    storyFilePath: `/repo/${id}.md`,
    storyId: id,
    status: "ready",
  });

  it("returns the first non-epic workspace id in array order", () => {
    const workspaces = {
      "acme:default": [integration("integration:web"), story("R-1"), story("R-2")],
    };
    expect(firstRootIdForTeam("acme:default", workspaces)).toBe("integration:web");
  });

  it("returns the first workspace of any kind (epic now tiles, no longer skipped)", () => {
    const workspaces = {
      "acme:default": [epic("epic-a"), epic("epic-b"), story("R-1")],
    };
    expect(firstRootIdForTeam("acme:default", workspaces)).toBe("epic-a");
  });

  it("returns the first story when stories are the only terminal workspaces", () => {
    const workspaces = { "acme:default": [story("R-1"), story("R-2")] };
    expect(firstRootIdForTeam("acme:default", workspaces)).toBe("R-1");
  });

  it("returns the first epic when epics are the only open workspaces (epic now tiles)", () => {
    const workspaces = { "acme:default": [epic("epic-a"), epic("epic-b")] };
    expect(firstRootIdForTeam("acme:default", workspaces)).toBe("epic-a");
  });

  it("returns the sentinel when the team is absent entirely", () => {
    expect(firstRootIdForTeam("missing:team", {})).toBe(ROOT_NO_REPOS_SENTINEL);
  });

  it("returns the sentinel when the team's workspace list is empty", () => {
    expect(firstRootIdForTeam("acme:default", { "acme:default": [] })).toBe(ROOT_NO_REPOS_SENTINEL);
  });
});

describe("activeWorkspaceIdForTeam", () => {
  const root = (id: string): Workspace => ({ id, kind: "root", cwd: "", status: "ready" });
  const workspaces = { "acme:default": [root("root:acme/web"), root("root:acme/api")] };

  it("returns the explicitly-focused workspace id when one is set", () => {
    const active = { "acme:default": "root:acme/api" };
    expect(activeWorkspaceIdForTeam("acme:default", active, workspaces)).toBe("root:acme/api");
  });

  it("falls back to the first root when no workspace is focused", () => {
    expect(activeWorkspaceIdForTeam("acme:default", {}, workspaces)).toBe("root:acme/web");
  });

  it("falls back to the sentinel when the team has no roots and none is focused", () => {
    expect(activeWorkspaceIdForTeam("acme:default", {}, {})).toBe(ROOT_NO_REPOS_SENTINEL);
  });
});

describe("activeWorkspaceForTeam", () => {
  const web = (): Workspace => ({ id: "root:acme/web", kind: "root", cwd: "", status: "ready" });
  const api = (): Workspace => ({ id: "root:acme/api", kind: "root", cwd: "", status: "ready" });

  it("returns the explicitly-focused workspace object by reference", () => {
    const [w, a] = [web(), api()];
    const workspaces = { "acme:default": [w, a] };
    const active = { "acme:default": "root:acme/api" };
    expect(activeWorkspaceForTeam("acme:default", active, workspaces)).toBe(a);
  });

  it("falls back to the first root object when no workspace is focused", () => {
    const [w, a] = [web(), api()];
    const workspaces = { "acme:default": [w, a] };
    expect(activeWorkspaceForTeam("acme:default", {}, workspaces)).toBe(w);
  });

  it("resolves an epic-only team to the epic (epic now tiles — SW-GS-2a)", () => {
    const epicWs: Workspace = {
      id: "epic-a",
      kind: "epic",
      cwd: "",
      epicDirPath: "epic-a",
      status: "ready",
    };
    expect(activeWorkspaceForTeam("acme:default", {}, { "acme:default": [epicWs] })).toBe(epicWs);
  });

  it("returns null when the team is absent entirely", () => {
    expect(activeWorkspaceForTeam("missing:team", {}, {})).toBeNull();
  });

  it("returns null when the team's workspace list is empty", () => {
    expect(activeWorkspaceForTeam("acme:default", {}, { "acme:default": [] })).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// T.3 helpers: source-repo glyph + canonical workspace sort
// ---------------------------------------------------------------------------

describe("computeRepoGlyphBase", () => {
  it("uses first char + first following consonant", () => {
    expect(computeRepoGlyphBase("web")).toBe("WB");
    expect(computeRepoGlyphBase("api")).toBe("AP");
    expect(computeRepoGlyphBase("auth")).toBe("AT");
    expect(computeRepoGlyphBase("wiki")).toBe("WK");
  });

  it("falls back to second char when no consonant follows the first char", () => {
    // "ai" -> no consonant after 'a' -> use second char 'i' -> "AI"
    expect(computeRepoGlyphBase("ai")).toBe("AI");
    // "io" -> no consonant after 'i' -> "IO"
    expect(computeRepoGlyphBase("io")).toBe("IO");
  });

  it("uppercases the result regardless of input casing", () => {
    expect(computeRepoGlyphBase("Web")).toBe("WB");
    expect(computeRepoGlyphBase("API")).toBe("AP");
  });

  it("handles 1-char repo names with `?` filler", () => {
    expect(computeRepoGlyphBase("x")).toBe("X?");
  });

  it("returns `??` for empty input (defence-in-depth)", () => {
    expect(computeRepoGlyphBase("")).toBe("??");
  });

  it("treats digits and special chars as non-consonants (falls back to char 1)", () => {
    // "a1b" -> no consonant at idx 1, but idx 2 'b' is consonant -> "AB"
    expect(computeRepoGlyphBase("a1b")).toBe("AB");
    // "a1" -> no consonant anywhere -> fall back to second char "1" -> "A1"
    expect(computeRepoGlyphBase("a1")).toBe("A1");
  });
});

describe("computeTeamGlyphMap", () => {
  it("maps each slug to its base glyph when no collisions", () => {
    const map = computeTeamGlyphMap(["acme/web", "acme/api", "acme/auth"]);
    expect(map.get("acme/web")).toBe("WB");
    expect(map.get("acme/api")).toBe("AP");
    expect(map.get("acme/auth")).toBe("AT");
  });

  it("appends numeric suffix starting at 2 for collisions in source_repos order", () => {
    // Both compute "AP" (a + first consonant p).
    const map = computeTeamGlyphMap(["acme/api", "acme/apex"]);
    expect(map.get("acme/api")).toBe("AP");
    expect(map.get("acme/apex")).toBe("AP2");
  });

  it("continues numbering past 2 for three+ colliders", () => {
    const map = computeTeamGlyphMap(["x/api", "x/apex", "x/apple"]);
    expect(map.get("x/api")).toBe("AP");
    expect(map.get("x/apex")).toBe("AP2");
    expect(map.get("x/apple")).toBe("AP3");
  });

  it("collision suffix is deterministic by input order, not alphabetical", () => {
    const a = computeTeamGlyphMap(["x/apex", "x/api"]);
    expect(a.get("x/apex")).toBe("AP");
    expect(a.get("x/api")).toBe("AP2");
    const b = computeTeamGlyphMap(["x/api", "x/apex"]);
    expect(b.get("x/api")).toBe("AP");
    expect(b.get("x/apex")).toBe("AP2");
  });

  it("returns empty map for empty input", () => {
    expect(computeTeamGlyphMap([])).toEqual(new Map());
  });

  it("treats a slug without a `/` as repo-only (post-split repoPart = '')", () => {
    // Defence-in-depth: malformed slug -> repoPart "" -> base "??"
    const map = computeTeamGlyphMap(["malformed"]);
    expect(map.get("malformed")).toBe("??");
  });

  it("guarantees glyph uniqueness across same-base 3+ colliders", () => {
    // Defensive coverage: three slugs that all reduce to base "AP" must
    // get distinct glyphs "AP", "AP2", "AP3". Documents the invariant that
    // every output glyph is unique within the map.
    const map = computeTeamGlyphMap(["x/api", "x/apex", "y/ap"]);
    expect(map.get("x/api")).toBe("AP");
    expect(map.get("x/apex")).toBe("AP2");
    expect(map.get("y/ap")).toBe("AP3");
    const values = Array.from(map.values());
    expect(new Set(values).size).toBe(values.length);
  });
});

describe("sortWorkspacesByCanonicalOrder", () => {
  const root = (id: string, slug: string): Workspace => ({
    id,
    kind: "root",
    cwd: "",
    status: "ready",
    sourceRepoSlug: slug,
  });
  const story = (id: string, slug?: string): Workspace => ({
    id,
    kind: "story",
    cwd: `/wt/${id}`,
    storyFilePath: `/repo/${id}.md`,
    storyId: id,
    status: "ready",
    sourceRepoSlug: slug,
  });

  it("groups by sourceRepoSlug in slugsInTeamOrder order, with roots before stories per bucket", () => {
    const input = [
      story("api-1", "acme/api"),
      root("root:acme/web", "acme/web"),
      story("web-1", "acme/web"),
      root("root:acme/api", "acme/api"),
      story("web-2", "acme/web"),
    ];
    const out = sortWorkspacesByCanonicalOrder(input, ["acme/web", "acme/api"]);
    expect(out.map((w) => w.id)).toEqual([
      "root:acme/web",
      "web-1",
      "web-2",
      "root:acme/api",
      "api-1",
    ]);
  });

  it("is stable within (bucket, kind) — preserves in-block insertion order", () => {
    const input = [
      story("web-1", "acme/web"),
      story("web-2", "acme/web"),
      story("web-3", "acme/web"),
    ];
    const out = sortWorkspacesByCanonicalOrder(input, ["acme/web"]);
    expect(out.map((w) => w.id)).toEqual(["web-1", "web-2", "web-3"]);
  });

  it("pushes workspaces with unknown sourceRepoSlug to the end", () => {
    const input = [
      story("orphan", "removed/repo"),
      root("root:acme/web", "acme/web"),
      story("web-1", "acme/web"),
    ];
    const out = sortWorkspacesByCanonicalOrder(input, ["acme/web"]);
    expect(out.map((w) => w.id)).toEqual(["root:acme/web", "web-1", "orphan"]);
  });

  it("treats missing sourceRepoSlug as unknown (pushed to end)", () => {
    const input = [story("no-slug"), root("root:acme/web", "acme/web")];
    const out = sortWorkspacesByCanonicalOrder(input, ["acme/web"]);
    expect(out.map((w) => w.id)).toEqual(["root:acme/web", "no-slug"]);
  });

  it("returns a new array (does not mutate input)", () => {
    const input = [root("root:acme/web", "acme/web")];
    const out = sortWorkspacesByCanonicalOrder(input, ["acme/web"]);
    expect(out).not.toBe(input);
  });

  it("returns empty for empty input", () => {
    expect(sortWorkspacesByCanonicalOrder([], ["acme/web"])).toEqual([]);
  });
});

describe("adjacentWorkspaceIdAfterClose (SW-CW-4 — right-biased close neighbor)", () => {
  // `isEligible` = "is a live surviving workspace" — the store passes membership
  // in the post-close workspace set. `all` = every neighbor eligible.
  const all = () => true;

  it("lands on the RIGHT neighbor when closing a middle tab (spatially stable)", () => {
    // [ROOT, A, B, C], close B → C (the tab that slides into B's slot)
    expect(adjacentWorkspaceIdAfterClose(["ROOT", "A", "B", "C"], "B", all, "ROOT")).toBe("C");
  });

  it("falls to the LEFT neighbor when closing the rightmost tab (no right → clamp)", () => {
    expect(adjacentWorkspaceIdAfterClose(["ROOT", "A", "B", "C"], "C", all, "ROOT")).toBe("B");
  });

  it("picks the right neighbor when closing the leftmost tab", () => {
    // Closing index 0 → the new leftmost, which is index 0's right neighbor.
    expect(adjacentWorkspaceIdAfterClose(["ROOT", "A", "B"], "ROOT", all, "FALLBACK")).toBe("A");
  });

  it("closing a MIDDLE epic's last tab crosses right into the NEXT epic", () => {
    // Strip [eE, eF, eG]; close eF (middle, its epic's only tab) → eG (next epic).
    expect(
      adjacentWorkspaceIdAfterClose(["eE-child", "eF-child", "eG-child"], "eF-child", all, "ROOT"),
    ).toBe("eG-child");
  });

  it("closing the RIGHTMOST tab of the strip falls left to the previous epic", () => {
    // Full flat strip: epicE has one child, epicF has one child. Close the (rightmost)
    // F child → the E child to its left (no right neighbor), NOT the first-root fallback.
    expect(
      adjacentWorkspaceIdAfterClose(["epicE-child", "epicF-child"], "epicF-child", all, "ROOT"),
    ).toBe("epicE-child");
  });

  it("falls back to fallbackId when the closed tab was the only on-screen tab", () => {
    expect(adjacentWorkspaceIdAfterClose(["only"], "only", all, "ROOT")).toBe("ROOT");
  });

  it("skips a parked/minimized neighbor (not a survivor), never resurrecting it", () => {
    // [ROOT, A(parked), B(rightmost)], close B → A is skipped (not live) → clamp left to ROOT.
    const survives = (id: string) => id !== "A";
    expect(adjacentWorkspaceIdAfterClose(["ROOT", "A", "B"], "B", survives, "FB")).toBe("ROOT");
  });

  it("skips a STALE neighbor id absent from the live set (guards a dangling active id)", () => {
    // Order still lists an already-gone workspace G (published before it left); closing B
    // (rightmost) must not select G — G ineligible → clamp left to A. Regression guard.
    const survives = (id: string) => id !== "G";
    expect(adjacentWorkspaceIdAfterClose(["ROOT", "A", "G", "B"], "B", survives, "FB")).toBe("A");
  });

  it("falls back to fallbackId when every neighbor is ineligible", () => {
    const survives = (id: string) => id === "B";
    expect(adjacentWorkspaceIdAfterClose(["A", "B", "C"], "B", survives, "FB")).toBe("FB");
  });

  it("falls back to fallbackId when the full order is not published (undefined)", () => {
    expect(adjacentWorkspaceIdAfterClose(undefined, "B", all, "ROOT")).toBe("ROOT");
  });

  it("falls back to fallbackId when the closed id is absent from the order", () => {
    expect(adjacentWorkspaceIdAfterClose(["A", "B"], "ghost", all, "ROOT")).toBe("ROOT");
  });
});

describe("teamLabelAmong", () => {
  it("names a team by its name, unless another org has a team of that name among them", () => {
    expect(teamLabelAmong("acme:core", ["acme:core", "acme:ops"])).toBe("core");
    expect(teamLabelAmong("acme:core", ["acme:core", "globex:core"])).toBe("acme › core");
    expect(teamLabelAmong("globex:core", ["acme:core", "globex:core"])).toBe("globex › core");
  });

  it("reads a key with no team as its org, and a team alone among none as its name", () => {
    expect(teamLabelAmong("acme:_none", ["globex:_none"])).toBe("acme");
    expect(teamLabelAmong("acme:core", [])).toBe("core");
  });
});

describe("workspaceInTeam", () => {
  const s = {
    workspaces: { "acme:core": [{ id: "w1" }] },
    softClosedWorkspaces: { "acme:core": { tray: { workspace: { id: "tray" } } } },
  };

  it("finds a workspace open or minimized in its own team only", () => {
    expect(workspaceInTeam(s, "acme:core", "w1")).toBe(true);
    expect(workspaceInTeam(s, "acme:core", "tray")).toBe(true);
    expect(workspaceInTeam(s, "acme:core", "gone")).toBe(false);
    expect(workspaceInTeam(s, "acme:ops", "w1")).toBe(false);
  });
});
