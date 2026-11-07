// SPDX-License-Identifier: MIT
import { currentContextNavigationRevision } from "@/shared/lib/context-navigation";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { LandingCandidate, LandingPageResolution } from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { Org, Team } from "@/shared/types/domain";

import {
  asLandingPageResolution,
  contentRelativePath,
  invalidateLandingPages,
  isTeamEmpty,
  landingPageCandidates,
  openTeamLandingPage,
  teamContentRoot,
} from "./team-landing-page";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  resolveTeamLandingPage: vi.fn(),
}));

const DOCS_ROOT = "/home/u/codesense-docs";
const DOCS_ORG = { slug: "codesense/docs", org_root: DOCS_ROOT };
const DOCS_TEAM: Team = { name: "getting-started", source_repos: [], landing_page: "README.html" };
const DOCS_KEY = buildTeamKey(DOCS_ORG.slug, DOCS_TEAM.name);
const DOCS_CONTENT = `${DOCS_ROOT}/getting-started/content`;
const DOCS_README = `${DOCS_CONTENT}/README.html`;
const DOCS_TOUR = `${DOCS_CONTENT}/tour.html`;

const ACME_ORG = { slug: "acme", org_root: "/orgs/acme" };
const ACME_TEAM: Team = { name: "backend", source_repos: [] };
const ACME_KEY = buildTeamKey(ACME_ORG.slug, ACME_TEAM.name);
const ACME_CONTENT = "/orgs/acme/backend/content";

/**
 * A fake `resolve_team_landing_page` over `{contentRoot: files[]}` with the real
 * contract: a missing or empty folder is "unavailable" (the backend never
 * throws for it), the first candidate present wins, fallbacks in any case.
 */
function fakeDisk(tree: Record<string, string[]>) {
  return vi.fn(
    async (root: string, candidates: LandingCandidate[]): Promise<LandingPageResolution> => {
      const files = tree[root] ?? [];
      if (files.length === 0) return { kind: "unavailable" };
      for (const c of candidates) {
        const hit =
          files.find((f) => f === c.path) ??
          (c.anyCase ? files.find((f) => f.toLowerCase() === c.path.toLowerCase()) : undefined);
        if (hit) return { kind: "found", path: `${root}/${hit}` };
      }
      return { kind: "none" };
    },
  );
}

function org(slug: string, org_root: string, teams: Team[]): Org {
  return {
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams,
    org_root,
  };
}

function enter(orgSlug: string, teamName: string) {
  useOrgStore.setState({ activeOrgSlug: orgSlug, activeTeamName: teamName });
}

beforeEach(() => {
  invalidateLandingPages();
  useOrgStore.setState({
    orgs: [
      org(DOCS_ORG.slug, DOCS_ORG.org_root, [DOCS_TEAM]),
      org(ACME_ORG.slug, ACME_ORG.org_root, [ACME_TEAM]),
    ],
  });
  enter(DOCS_ORG.slug, DOCS_TEAM.name);
  useViewerStore.setState({
    stateLoaded: true,
    persistedStateRestored: true,
    openTabs: {},
    activeTab: {},
    navHistory: {},
    navIndex: {},
    openedViewerPaths: {},
    layoutMode: null,
  });
});

function tabsOf(teamKey: string): string[] {
  return (useViewerStore.getState().openTabs[teamKey] ?? []).map((t) => t.path);
}

const docs = (resolve: ReturnType<typeof fakeDisk>, justClosed?: string[]) =>
  openTeamLandingPage({ org: DOCS_ORG, team: DOCS_TEAM, resolve, justClosed });

describe("landingPageCandidates", () => {
  it("puts a declared page first (exact name), then the fallbacks (any case)", () => {
    expect(landingPageCandidates(DOCS_TEAM)).toEqual([
      { path: "README.html", anyCase: false },
      { path: "README.md", anyCase: true },
      { path: "index.html", anyCase: true },
    ]);
  });

  it("uses only the fallbacks when the team declares nothing", () => {
    expect(landingPageCandidates(ACME_TEAM).map((c) => c.path)).toEqual([
      "README.md",
      "README.html",
      "index.html",
    ]);
  });

  it("ignores a declared path that would leave the team's content", () => {
    for (const landing_page of [
      "../org.yml",
      "/etc/passwd",
      "a/../../x",
      "a\\b.md",
      "",
      "./x",
      ".env",
      ".claude/notes.md",
      "docs/.secret/x.md",
    ]) {
      expect(landingPageCandidates({ landing_page }).map((c) => c.path)).toEqual([
        "README.md",
        "README.html",
        "index.html",
      ]);
    }
  });

  it("accepts a declared page in a sub-folder, trimmed", () => {
    expect(landingPageCandidates({ landing_page: " guide/start.md " })[0]).toEqual({
      path: "guide/start.md",
      anyCase: false,
    });
  });
});

describe("asLandingPageResolution", () => {
  it("keeps well-formed answers and turns anything else into unavailable", () => {
    expect(asLandingPageResolution({ kind: "found", path: "/c/README.md" })).toEqual({
      kind: "found",
      path: "/c/README.md",
    });
    expect(asLandingPageResolution({ kind: "none" })).toEqual({ kind: "none" });
    for (const raw of [
      undefined,
      null,
      42,
      "x",
      {},
      { kind: "found" },
      { kind: "found", path: "" },
      { kind: "other" },
    ]) {
      expect(asLandingPageResolution(raw)).toEqual({ kind: "unavailable" });
    }
  });
  it("logs a malformed answer but stays quiet when there is no answer at all", () => {
    const warn = vi.spyOn(logger, "warn").mockImplementation(() => {});
    warn.mockClear(); // the test setup may already spy on the logger
    try {
      asLandingPageResolution(undefined);
      asLandingPageResolution(null);
      asLandingPageResolution({ kind: "unavailable" });
      expect(warn).not.toHaveBeenCalled();
      asLandingPageResolution({ kind: "found" });
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn.mock.calls[0]?.[0]).toBe("viewer");
    } finally {
      warn.mockRestore();
    }
  });
});

describe("contentRelativePath", () => {
  it("strips the content root, however its slashes are spelled", () => {
    expect(contentRelativePath(`${ACME_CONTENT}/guide/a.md`, ACME_CONTENT)).toBe("guide/a.md");
    expect(contentRelativePath("/orgs/acme//backend/content/a.md", ACME_CONTENT)).toBe("a.md");
    expect(contentRelativePath(`${ACME_CONTENT}/`, ACME_CONTENT)).toBeNull();
    expect(contentRelativePath("/orgs/acme/web/content/a.md", ACME_CONTENT)).toBeNull();
    // A same-named team elsewhere is not this team's file.
    expect(contentRelativePath("/orgs/other/backend/content/a.md", ACME_CONTENT)).toBeNull();
  });
});

describe("isTeamEmpty / teamContentRoot", () => {
  it("is empty only with no tabs and no active tab", () => {
    expect(isTeamEmpty({ openTabs: {}, activeTab: {} }, ACME_KEY)).toBe(true);
    expect(isTeamEmpty({ openTabs: { [ACME_KEY]: [] }, activeTab: {} }, ACME_KEY)).toBe(true);
    expect(
      isTeamEmpty(
        { openTabs: { [ACME_KEY]: [{ name: "a", path: "/a" }] }, activeTab: {} },
        ACME_KEY,
      ),
    ).toBe(false);
    expect(isTeamEmpty({ openTabs: {}, activeTab: { [ACME_KEY]: "/a" } }, ACME_KEY)).toBe(false);
  });

  it("is {org_root}/{team}/content", () => {
    expect(teamContentRoot("/orgs/acme/", "backend")).toBe(ACME_CONTENT);
  });
});

describe("openTeamLandingPage", () => {
  it("entering the CodeSense Docs team with nothing open opens its README.html", async () => {
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html", "tour.html", "index.html"] });
    await expect(docs(resolve)).resolves.toBe("opened");
    expect(resolve).toHaveBeenCalledWith(DOCS_CONTENT, landingPageCandidates(DOCS_TEAM));
    expect(tabsOf(DOCS_KEY)).toEqual([DOCS_README]);
    expect(useViewerStore.getState().activeTab[DOCS_KEY]).toBe(DOCS_README);
  });

  it("saved tabs win: they are left alone and the disk is not probed", async () => {
    useViewerStore.setState({
      openTabs: { [DOCS_KEY]: [{ name: "tour.html", path: DOCS_TOUR }] },
      activeTab: { [DOCS_KEY]: DOCS_TOUR },
    });
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    await expect(docs(resolve)).resolves.toBe("occupied");
    expect(resolve).not.toHaveBeenCalled();
    expect(tabsOf(DOCS_KEY)).toEqual([DOCS_TOUR]);
  });

  it("closing the last tab (not the landing page) brings the landing page back", async () => {
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html", "tour.html"] });
    useViewerStore.getState().openFile(DOCS_KEY, "", DOCS_TOUR);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_TOUR);
    await expect(docs(resolve, [DOCS_TOUR])).resolves.toBe("opened");
    expect(tabsOf(DOCS_KEY)).toEqual([DOCS_README]);
  });

  it("closing the landing page itself leaves the team empty (dismissed)", async () => {
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    await docs(resolve);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_README);
    await expect(docs(resolve, [DOCS_README])).resolves.toBe("dismissed");
    expect(tabsOf(DOCS_KEY)).toEqual([]);
    // …until the team is entered again.
    await expect(docs(resolve)).resolves.toBe("opened");
    expect(tabsOf(DOCS_KEY)).toEqual([DOCS_README]);
  });

  it("dismissal compares content-relative paths, not exact strings", async () => {
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    await docs(resolve);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_README);
    // The tab was opened from a differently spelled root (a doubled slash).
    const sameFile = `${DOCS_ROOT}//getting-started/content/README.html`;
    await expect(docs(resolve, [sameFile])).resolves.toBe("dismissed");
    expect(tabsOf(DOCS_KEY)).toEqual([]);
  });

  it("a team with no landing page and no README opens nothing", async () => {
    enter(ACME_ORG.slug, ACME_TEAM.name);
    const resolve = fakeDisk({ [ACME_CONTENT]: ["notes.md", "plan.html"] });
    await expect(openTeamLandingPage({ org: ACME_ORG, team: ACME_TEAM, resolve })).resolves.toBe(
      "none",
    );
    expect(tabsOf(ACME_KEY)).toEqual([]);
    expect(useViewerStore.getState().activeTab[ACME_KEY]).toBeUndefined();
  });

  it("a team without a declared page opens its README by convention, in any case", async () => {
    enter(ACME_ORG.slug, ACME_TEAM.name);
    const resolve = fakeDisk({ [ACME_CONTENT]: ["readme.md", "index.html"] });
    await expect(openTeamLandingPage({ org: ACME_ORG, team: ACME_TEAM, resolve })).resolves.toBe(
      "opened",
    );
    expect(tabsOf(ACME_KEY)).toEqual([`${ACME_CONTENT}/readme.md`]);
  });

  it("content not there yet (missing/empty folder): nothing opens, the next entry retries", async () => {
    enter(ACME_ORG.slug, ACME_TEAM.name);
    const tree: Record<string, string[]> = {};
    const resolve = fakeDisk(tree);
    await expect(openTeamLandingPage({ org: ACME_ORG, team: ACME_TEAM, resolve })).resolves.toBe(
      "skipped",
    );
    tree[ACME_CONTENT] = ["README.md"];
    await expect(openTeamLandingPage({ org: ACME_ORG, team: ACME_TEAM, resolve })).resolves.toBe(
      "opened",
    );
    expect(resolve).toHaveBeenCalledTimes(2);
  });

  it("a missing or malformed answer (no command in the IPC mock) is unavailable, not a throw", async () => {
    enter(ACME_ORG.slug, ACME_TEAM.name);
    const answers: unknown[] = [undefined, null, {}, { kind: "found" }, "README.md"];
    const resolve = vi.fn(async () => answers.shift() as LandingPageResolution);
    for (let i = 0; i < 5; i += 1) {
      await expect(openTeamLandingPage({ org: ACME_ORG, team: ACME_TEAM, resolve })).resolves.toBe(
        "skipped",
      );
    }
    // Never memoised: every entry asks again, and a real answer then opens.
    resolve.mockResolvedValueOnce({ kind: "found", path: `${ACME_CONTENT}/README.md` });
    await expect(openTeamLandingPage({ org: ACME_ORG, team: ACME_TEAM, resolve })).resolves.toBe(
      "opened",
    );
    expect(resolve).toHaveBeenCalledTimes(6);
    expect(tabsOf(ACME_KEY)).toEqual([`${ACME_CONTENT}/README.md`]);
  });

  it("waits for the real restore: a watchdog-forced stateLoaded is not enough", async () => {
    useViewerStore.setState({ stateLoaded: true, persistedStateRestored: false });
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    await expect(docs(resolve)).resolves.toBe("skipped");
    expect(resolve).not.toHaveBeenCalled();
  });

  it("never changes the layout: a console-layout team gets nothing opened", async () => {
    useViewerStore.setState({ layoutMode: "console" });
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    await expect(docs(resolve)).resolves.toBe("skipped");
    expect(tabsOf(DOCS_KEY)).toEqual([]);
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });

  it("a file the user opens while the page is looked up wins", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const disk = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    const resolve = vi.fn(async (root: string, c: LandingCandidate[]) => {
      await gate;
      return disk(root, c);
    });
    const pending = openTeamLandingPage({ org: DOCS_ORG, team: DOCS_TEAM, resolve });
    useViewerStore.getState().openFile(DOCS_KEY, "", DOCS_TOUR);
    release();
    await expect(pending).resolves.toBe("occupied");
    expect(tabsOf(DOCS_KEY)).toEqual([DOCS_TOUR]);
  });

  it("a quick switch away while the page is looked up opens nothing into the old team", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const disk = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    const resolve = vi.fn(async (root: string, c: LandingCandidate[]) => {
      await gate;
      return disk(root, c);
    });
    const pending = openTeamLandingPage({ org: DOCS_ORG, team: DOCS_TEAM, resolve });
    enter(ACME_ORG.slug, ACME_TEAM.name);
    release();
    await expect(pending).resolves.toBe("skipped");
    expect(tabsOf(DOCS_KEY)).toEqual([]);
  });

  it("memoises the resolved page per team until its content changes", async () => {
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    await docs(resolve);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_README);
    await docs(resolve);
    expect(resolve).toHaveBeenCalledTimes(1);

    // A change elsewhere keeps the memo; one in the team's content drops it.
    invalidateLandingPages(`${ACME_CONTENT}/x.md`);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_README);
    await docs(resolve);
    expect(resolve).toHaveBeenCalledTimes(1);
    invalidateLandingPages(`${DOCS_CONTENT}/README.html`);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_README);
    await docs(resolve);
    expect(resolve).toHaveBeenCalledTimes(2);
  });

  it("entering the team again resolves afresh (it may have changed while away)", async () => {
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    await docs(resolve);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_README);
    await openTeamLandingPage({ org: DOCS_ORG, team: DOCS_TEAM, resolve, fresh: true });
    expect(resolve).toHaveBeenCalledTimes(2);
  });

  it("closing the landing page during a running lookup still counts as dismissing it", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const disk = fakeDisk({ [DOCS_CONTENT]: ["README.html"] });
    const resolve = vi.fn(async (root: string, c: LandingCandidate[]) => {
      await gate;
      return disk(root, c);
    });
    const entry = openTeamLandingPage({ org: DOCS_ORG, team: DOCS_TEAM, resolve, fresh: true });
    // Meanwhile the user opens the README and closes it again.
    useViewerStore.getState().openFile(DOCS_KEY, "", DOCS_README);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_README);
    await expect(docs(resolve, [DOCS_README])).resolves.toBe("skipped");
    release();
    await expect(entry).resolves.toBe("dismissed");
    expect(tabsOf(DOCS_KEY)).toEqual([]);
  });

  it("a changed declared page (teams.yml) is resolved afresh", async () => {
    const resolve = fakeDisk({ [DOCS_CONTENT]: ["README.html", "tour.html"] });
    await docs(resolve);
    useViewerStore.getState().closeTab(DOCS_KEY, "", DOCS_README);
    const retargeted = { ...DOCS_TEAM, landing_page: "tour.html" };
    await openTeamLandingPage({ org: DOCS_ORG, team: retargeted, resolve });
    expect(resolve).toHaveBeenCalledTimes(2);
    expect(tabsOf(DOCS_KEY)).toEqual([DOCS_TOUR]);
  });
});

it("an automatic landing page preserves pending navigation", async () => {
  const revision = currentContextNavigationRevision();
  expect(await docs(fakeDisk({ [DOCS_CONTENT]: ["README.html"] }))).toBe("opened");
  expect(currentContextNavigationRevision()).toBe(revision);
});
