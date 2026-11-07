// SPDX-License-Identifier: MIT
/**
 * SW-DNS-2: per-dashboard settings (`codesense.prefs()` / `setPrefs(patch)`),
 * kept in the user's own app state under one top-level key, one entry per
 * dashboard instance.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";

const { store } = vi.hoisted(() => ({ store: { raw: "{}" } }));

vi.mock("@/tauri/commands", () => ({
  loadAppState: vi.fn(async () => store.raw),
  saveAppState: vi.fn(async (s: string) => {
    store.raw = s;
  }),
}));

import {
  dashboardPrefsKey,
  readDashboardPrefs,
  writeDashboardPrefs,
  DASHBOARD_PREFS_MAX_BYTES,
  DASHBOARD_PREFS_TOTAL_MAX_BYTES,
} from "./dashboard-prefs";

beforeEach(() => {
  store.raw = JSON.stringify({ viewer: { keep: true }, repos: ["r"] });
});

describe("dashboard prefs", () => {
  it("allows a bounded tree cache in existing file-nav UI state without widening ordinary pane prefs", async () => {
    const key = JSON.stringify(["ui:file-nav", "team:acme/a"]);
    const listings = "x".repeat(32 * 1024);
    await writeDashboardPrefs(key, { listings }, "liveHubUi");
    expect(await readDashboardPrefs(key, "liveHubUi")).toEqual({ listings });
    await expect(writeDashboardPrefs(key, { listings })).rejects.toThrow(/^too-large:/);
  });
  it("keys panes by stable dashboard identity and forks source prefs on first write", async () => {
    const source = { scope: "team" as const, org: "acme/repo", team: "dev", id: "board" };
    const copy = {
      scope: "private" as const,
      org: "acme/repo",
      id: "board",
      installedFrom: source,
    };
    const sourceKey = dashboardPrefsKey({ dashboardRef: source });
    const copyKey = dashboardPrefsKey({ dashboardRef: copy });
    expect(sourceKey).toBe("dashboard:team:acme/repo:dev/board");
    expect(copyKey).toBe("dashboard:private:acme/repo/board");
    await writeDashboardPrefs(sourceKey, { density: "compact", sort: "title" });
    expect(await readDashboardPrefs(copyKey, undefined, copy.installedFrom)).toEqual({
      density: "compact",
      sort: "title",
    });
    await writeDashboardPrefs(copyKey, { sort: "date" }, undefined, copy.installedFrom);
    await writeDashboardPrefs(sourceKey, { density: "full" });
    expect(await readDashboardPrefs(copyKey, undefined, copy.installedFrom)).toEqual({
      density: "compact",
      sort: "date",
    });
  });

  it("keeps inherited source prefs in use without creating a private entry", async () => {
    const source = { scope: "team" as const, org: "acme/repo", team: "dev", id: "board" };
    const sourceKey = dashboardPrefsKey({ dashboardRef: source })!;
    const copyKey = dashboardPrefsKey({
      dashboardRef: { scope: "private", org: source.org, id: "copy", installedFrom: source },
    })!;
    await writeDashboardPrefs(sourceKey, { density: "compact" });
    const state = JSON.parse(store.raw);
    const old = Date.now() - 2 * 24 * 60 * 60 * 1000;
    state.dashboardPrefs[sourceKey].at = old;
    store.raw = JSON.stringify(state);
    await readDashboardPrefs(copyKey, undefined, source);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(JSON.parse(store.raw).dashboardPrefs[sourceKey].at).toBeGreaterThan(old);
    expect(JSON.parse(store.raw).dashboardPrefs[copyKey]).toBeUndefined();
  });
  it("a new dashboard reads {}; a patch merges, null removes a key", async () => {
    const k = dashboardPrefsKey({ docPath: "/ctx/a/content/d.html" });
    expect(await readDashboardPrefs(k)).toEqual({});
    expect(await writeDashboardPrefs(k, { details: "full", hideDone: true })).toEqual({
      details: "full",
      hideDone: true,
    });
    expect(await writeDashboardPrefs(k, { hideDone: null })).toEqual({ details: "full" });
    expect(await readDashboardPrefs(k)).toEqual({ details: "full" });
  });

  it("survives a reload (it lives in app state) and leaves every other key alone", async () => {
    const k = dashboardPrefsKey({ docPath: "/ctx/a/content/d.html" });
    await writeDashboardPrefs(k, { a: 1 });
    const saved = JSON.parse(store.raw);
    expect(saved.viewer).toEqual({ keep: true });
    expect(saved.repos).toEqual(["r"]);
    expect(await readDashboardPrefs(k)).toEqual({ a: 1 });
  });

  it("a team pane and a private pane of the same id, and a team and a private dashboard, never share", async () => {
    const teamPane = dashboardPrefsKey({
      pane: { panesDir: "/ctx/acme/a/panes", fileName: "pipeline.html" },
    });
    const privPane = dashboardPrefsKey({
      pane: { panesDir: "/scratch/orgs/acme/panes", fileName: "pipeline.html" },
    });
    const teamDoc = dashboardPrefsKey({ docPath: "/ctx/acme/a/content/board.html" });
    const privDoc = dashboardPrefsKey({ docPath: "/scratch/orgs/acme/content/board.html" });
    expect(new Set([teamPane, privPane, teamDoc, privDoc]).size).toBe(4);
    await writeDashboardPrefs(teamPane, { details: "off" });
    await writeDashboardPrefs(privPane, { details: "full" });
    expect(await readDashboardPrefs(teamPane)).toEqual({ details: "off" });
    expect(await readDashboardPrefs(privPane)).toEqual({ details: "full" });
    expect(await readDashboardPrefs(teamDoc)).toEqual({});
  });

  it("a message doc is keyed by its message", () => {
    expect(
      dashboardPrefsKey({ docPath: "/x.html", message: { org: "acme", team: "a", messageId: 7 } }),
    ).toBe("message:acme/a/7");
  });

  it("rejects a non-object patch and an oversized entry", async () => {
    const k = dashboardPrefsKey({ docPath: "/d.html" });
    await expect(writeDashboardPrefs(k, [1] as never)).rejects.toThrow(/invalid:/);
    await expect(
      writeDashboardPrefs(k, { big: "x".repeat(DASHBOARD_PREFS_MAX_BYTES + 1) }),
    ).rejects.toThrow(/^too-large:/);
    expect(await readDashboardPrefs(k)).toEqual({});
  });

  it("a read refreshes the entry's age at most once a day, so set-once settings aren't evicted first", async () => {
    const k = dashboardPrefsKey({ docPath: "/once.html" })!;
    await writeDashboardPrefs(k, { details: "full" });
    const day = 24 * 60 * 60 * 1000;
    const state = JSON.parse(store.raw);
    state.dashboardPrefs[k].at = Date.now() - 2 * day;
    store.raw = JSON.stringify(state);
    expect(await readDashboardPrefs(k)).toEqual({ details: "full" });
    await new Promise((r) => setTimeout(r, 0));
    const touched = JSON.parse(store.raw).dashboardPrefs[k].at;
    expect(Date.now() - touched).toBeLessThan(60_000);
    // A second read the same day writes nothing.
    const raw = store.raw;
    await readDashboardPrefs(k);
    await new Promise((r) => setTimeout(r, 0));
    expect(store.raw).toBe(raw);
  });

  it("no identity: unavailable", async () => {
    expect(dashboardPrefsKey({})).toBeNull();
    await expect(readDashboardPrefs(null)).rejects.toThrow(/^unavailable:/);
  });

  it("rejects values JSON would mangle: Map, Set, BigInt, Date, functions, NaN", async () => {
    const k = dashboardPrefsKey({ docPath: "/d.html" });
    for (const bad of [
      new Map(),
      new Set([1]),
      BigInt(1),
      new Date(0),
      () => 1,
      NaN,
      { deep: [new Map()] },
    ]) {
      await expect(writeDashboardPrefs(k, { v: bad })).rejects.toThrow(/^invalid:/);
    }
    expect(await readDashboardPrefs(k)).toEqual({});
  });

  it("caps all dashboards together, dropping the least recently written (never the one just written)", async () => {
    const chunk = "x".repeat(15 * 1024);
    const n = Math.ceil(DASHBOARD_PREFS_TOTAL_MAX_BYTES / chunk.length) + 3;
    for (let i = 0; i < n; i++) {
      await writeDashboardPrefs(dashboardPrefsKey({ docPath: `/d${i}.html` }), { c: chunk });
    }
    const saved = JSON.parse(store.raw).dashboardPrefs;
    expect(new TextEncoder().encode(JSON.stringify(saved)).length).toBeLessThanOrEqual(
      DASHBOARD_PREFS_TOTAL_MAX_BYTES,
    );
    expect(saved[`doc:/d${n - 1}.html`]).toBeTruthy();
    expect(saved["doc:/d0.html"]).toBeUndefined();
  });
});
