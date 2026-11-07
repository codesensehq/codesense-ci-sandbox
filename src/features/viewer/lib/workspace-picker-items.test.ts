// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  buildWorkspacePickerItems,
  encodePickerId,
  decodePickerId,
} from "./workspace-picker-items";
import type { Workspace } from "@/shared/stores/viewer-store";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

function node(partial: Partial<DerivedPipelineTree>): DerivedPipelineTree {
  return {
    dirPath: "",
    dirName: "",
    absoluteDirPath: "",
    stories: [],
    subEpics: [],
    orderError: null,
    mainDoc: null,
    ...partial,
  };
}

function storyRef(filePath: string, storyId?: string, documentType: string | null = "story") {
  return {
    fileName: filePath.split("/").pop()!,
    filePath,
    dependsOn: null,
    status: null,
    documentType,
    supportingDocs: null,
    sourceRepo: null,
    team: null,
    storyId,
  };
}

const tree = node({
  subEpics: [
    node({
      dirName: "alpha",
      dirPath: "in-progress/alpha",
      stories: [
        storyRef("/r/alpha/A-1.md", "A-1"),
        storyRef("/r/alpha/README.md", undefined, "doc"),
      ],
    }),
  ],
});

describe("encode/decodePickerId", () => {
  it("round-trips each action kind", () => {
    expect(decodePickerId(encodePickerId({ kind: "workspace", workspaceId: "story:x:1" }))).toEqual(
      {
        kind: "workspace",
        workspaceId: "story:x:1",
      },
    );
    expect(decodePickerId(encodePickerId({ kind: "story", filePath: "/r/a/b.md" }))).toEqual({
      kind: "story",
      filePath: "/r/a/b.md",
    });
    // SW-GW-1: the generic "open a dir" action round-trips via its literal id.
    expect(decodePickerId(encodePickerId({ kind: "generic" }))).toEqual({ kind: "generic" });
  });

  it("returns null for an unrecognized id", () => {
    expect(decodePickerId("garbage")).toBeNull();
  });
});

describe("buildWorkspacePickerItems", () => {
  const openStory: Workspace = {
    id: "ws-A-1",
    kind: "story",
    cwd: "/wt/A-1",
    storyId: "A-1",
    storyFilePath: "/r/alpha/A-1.md",
    status: "ready",
  };

  it("offers no Git Sync or Notes entry (specific-role agents retired)", () => {
    const items = buildWorkspacePickerItems([], null);
    expect(items.find((i) => i.label === "Git Sync")).toBeUndefined();
    expect(items.find((i) => i.label === "Notes")).toBeUndefined();
  });

  it("always offers the Generic Workspace entry (SW-GW-1 'open a dir' door)", () => {
    const items = buildWorkspacePickerItems([], null);
    expect(items.map((i) => decodePickerId(i.id))).toContainEqual({ kind: "generic" });
    const entry = items.find((i) => i.label === "Generic Workspace");
    expect(entry).toBeDefined();
    // Worded as "open a terminal," not "add a project" — distinct from Add repo.
    expect(entry?.sublabel).toBe("open any directory as a terminal");
  });

  it("lists an open generic workspace with its dir basename as the label (SW-GW-1 G2)", () => {
    const genericWs: Workspace = {
      id: "generic:/Users/me/scratch",
      kind: "generic",
      cwd: "/Users/me/scratch",
      status: "ready",
    };
    const items = buildWorkspacePickerItems([genericWs], null);
    expect(items.map((i) => decodePickerId(i.id))).toContainEqual({
      kind: "workspace",
      workspaceId: "generic:/Users/me/scratch",
    });
    expect(items.find((i) => i.label === "scratch")).toBeDefined();
  });

  it("lists actionable stories from the tree (no epic items)", () => {
    const items = buildWorkspacePickerItems([], tree);
    const ids = items.map((i) => decodePickerId(i.id));
    // story A-1 only; README (doc) excluded; epic is a label, never an item.
    expect(ids).toContainEqual({ kind: "story", filePath: "/r/alpha/A-1.md" });
    // No epic items; only story / workspace / generic actions appear.
    const allowed = new Set(["story", "workspace", "generic"]);
    expect(ids.some((a) => a != null && !allowed.has(a.kind))).toBe(false);
    expect(items.some((i) => i.label === "README")).toBe(false);
  });

  it("skips a tree story already represented by an open workspace (no duplicate)", () => {
    const items = buildWorkspacePickerItems([openStory], tree);
    const storyItems = items
      .map((i) => decodePickerId(i.id))
      .filter((a) => a?.kind === "story" && a.filePath === "/r/alpha/A-1.md");
    expect(storyItems).toHaveLength(0);
    // The open workspace IS present.
    expect(items.map((i) => decodePickerId(i.id))).toContainEqual({
      kind: "workspace",
      workspaceId: "ws-A-1",
    });
  });

  it("tolerates a null tree (open workspaces still listed)", () => {
    const items = buildWorkspacePickerItems([openStory], null);
    const ids = items.map((i) => decodePickerId(i.id));
    expect(ids).toContainEqual({ kind: "workspace", workspaceId: "ws-A-1" });
  });

  it("targets story + epic; skips root (no doc) — SW-GS-2a 4-kind model", () => {
    const rootWs: Workspace = { id: "root:acme/web", kind: "root", cwd: "/r", status: "ready" };
    const epicWs: Workspace = {
      id: "integration:SW-EW",
      kind: "epic",
      cwd: "/epic/dir",
      epicDirPath: "SW-EW",
      status: "ready",
    };
    const items = buildWorkspacePickerItems([rootWs, openStory, epicWs], null);
    const ids = items.map((i) => decodePickerId(i.id));
    // root is NOT an activatable picker target...
    expect(ids).not.toContainEqual({ kind: "workspace", workspaceId: "root:acme/web" });
    // ...but the story + epic rows are.
    expect(ids).toContainEqual({ kind: "workspace", workspaceId: "ws-A-1" });
    expect(ids).toContainEqual({ kind: "workspace", workspaceId: "integration:SW-EW" });
  });
});
