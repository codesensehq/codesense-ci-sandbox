// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  readTeamKeyed,
  updateTeamKeyed,
  removeTeamKeyed,
  pinPreviewInTabs,
  replacePreviewSlot,
  neighborPathAfterClose,
} from "./viewer-store";
import type { TabState } from "./viewer-store";

/**
 * RF-4 — pure team-keyed record + tab-array helpers.
 *
 * These extract the `state.field[teamKey] ?? default` reads (~90 sites), the
 * `{ ...state.field, [teamKey]: next }` writes (~96 sites), and the tab-array
 * mutators that the normal viewer tabs and the sync redline tabs kept parallel
 * copies of. The load-bearing property is AC-5: an update whose value is
 * unchanged returns the SAME map reference, so we never churn zustand slice
 * identity (no gratuitous subscriber fan-out / re-renders).
 */

const tk = "acme:default";

describe("RF-4 readTeamKeyed", () => {
  it("returns the entry when present", () => {
    expect(readTeamKeyed({ [tk]: [1, 2] }, tk, [] as number[])).toEqual([1, 2]);
  });

  it("returns the caller-supplied fallback when absent", () => {
    expect(readTeamKeyed<number[]>({}, tk, [])).toEqual([]);
    expect(readTeamKeyed<string>({}, tk, "shared")).toBe("shared");
  });

  it("returns the SAME reference as the stored value (no copy)", () => {
    const stored = [{ path: "a" }];
    expect(readTeamKeyed({ [tk]: stored }, tk, [])).toBe(stored);
  });
});

describe("RF-4 updateTeamKeyed", () => {
  it("writes a new entry, preserving other keys", () => {
    const map = { other: 1 };
    const next = updateTeamKeyed(map, tk, 5);
    expect(next).toEqual({ other: 1, [tk]: 5 });
    expect(next).not.toBe(map);
  });

  it("overwrites an existing entry with a changed value", () => {
    const next = updateTeamKeyed({ [tk]: 1 }, tk, 2);
    expect(next[tk]).toBe(2);
  });

  it("AC-5: returns the SAME map reference when the value is unchanged (primitive)", () => {
    const map = { [tk]: "shared" };
    expect(updateTeamKeyed(map, tk, "shared")).toBe(map);
  });

  it("AC-5: returns the SAME map reference when the value ref is unchanged (array)", () => {
    const arr = [{ path: "a" }];
    const map = { [tk]: arr };
    expect(updateTeamKeyed(map, tk, arr)).toBe(map);
  });

  it("AC-5: DOES churn when a new-but-equal array ref is written (matches raw spread)", () => {
    const map = { [tk]: [{ path: "a" }] };
    const next = updateTeamKeyed(map, tk, [{ path: "a" }]);
    expect(next).not.toBe(map); // ref differs → real change, mirrors `{ ...map, [tk]: next }`
  });
});

describe("RF-4 removeTeamKeyed", () => {
  it("drops the key, preserving others", () => {
    expect(removeTeamKeyed({ [tk]: 1, other: 2 }, tk)).toEqual({ other: 2 });
  });

  it("returns the SAME map reference when the key is already absent", () => {
    const map = { other: 2 };
    expect(removeTeamKeyed(map, tk)).toBe(map);
  });
});

describe("RF-4 pinPreviewInTabs", () => {
  it("pins an open preview tab in place", () => {
    const tabs: TabState[] = [
      { path: "a", name: "a" },
      { path: "b", name: "b", preview: true },
    ];
    const next = pinPreviewInTabs(tabs, "b");
    expect(next[1].preview).toBe(false);
    expect(next).not.toBe(tabs);
  });

  it("returns the SAME array when the path is not open (no-op)", () => {
    const tabs: TabState[] = [{ path: "a", name: "a" }];
    expect(pinPreviewInTabs(tabs, "z")).toBe(tabs);
  });

  it("returns the SAME array when the path is already pinned (no-op)", () => {
    const tabs: TabState[] = [{ path: "a", name: "a" }];
    expect(pinPreviewInTabs(tabs, "a")).toBe(tabs);
  });
});

describe("RF-4 replacePreviewSlot", () => {
  it("replaces the existing preview tab in place", () => {
    const tabs: TabState[] = [
      { path: "a", name: "a" },
      { path: "b", name: "b", preview: true },
    ];
    const newPreview: TabState = { path: "c", name: "c", preview: true };
    const next = replacePreviewSlot(tabs, newPreview);
    expect(next).toEqual([
      { path: "a", name: "a" },
      { path: "c", name: "c", preview: true },
    ]);
  });

  it("appends when no preview slot is open", () => {
    const tabs: TabState[] = [{ path: "a", name: "a" }];
    const newPreview: TabState = { path: "c", name: "c", preview: true };
    expect(replacePreviewSlot(tabs, newPreview)).toEqual([...tabs, newPreview]);
  });
});

describe("RF-4 neighborPathAfterClose", () => {
  it("picks the same-index neighbor", () => {
    const remaining: TabState[] = [
      { path: "a", name: "a" },
      { path: "c", name: "c" },
    ];
    expect(neighborPathAfterClose(remaining, 1)).toBe("c");
  });

  it("clamps to the last remaining tab", () => {
    const remaining: TabState[] = [{ path: "a", name: "a" }];
    expect(neighborPathAfterClose(remaining, 3)).toBe("a");
  });

  it("returns '' when nothing remains", () => {
    expect(neighborPathAfterClose([], 0)).toBe("");
  });
});
