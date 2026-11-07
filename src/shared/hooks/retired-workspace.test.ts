// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { isRetiredRow, retiredWorkspaceIds } from "./retired-workspace";

type Row = Parameters<typeof isRetiredRow>[0];
const story = (id: string, storyId: string): Row => ({ id, kind: "story", storyId }) as Row;
// Any kind but "story": only the story id or the workspace id can match.
const other = (id: string): Row => ({ id, kind: "integration" }) as unknown as Row;

describe("isRetiredRow", () => {
  it("matches the story row by its story id", () => {
    expect(isRetiredRow(story("sw-1-slug", "SW-1"), { teamKey: "t", form: "SW-1" })).toBe(true);
    expect(isRetiredRow(story("sw-2-slug", "SW-2"), { teamKey: "t", form: "SW-1" })).toBe(false);
  });

  it("matches any row by the workspace id, and nothing without one", () => {
    expect(isRetiredRow(other("ws-9"), { teamKey: "t", form: "ws-9", workspaceId: "ws-9" })).toBe(
      true,
    );
    expect(isRetiredRow(other("ws-9"), { teamKey: "t", form: "ws-9", workspaceId: null })).toBe(
      false,
    );
  });
});

describe("retiredWorkspaceIds", () => {
  it("finds the retired row whether it is open or minimized, in that team only", () => {
    const state = {
      workspaces: {
        "acme:default": [story("sw-1-slug", "SW-1"), story("sw-2-slug", "SW-2")],
        "acme:other": [story("sw-1-other", "SW-1")],
      },
      softClosedWorkspaces: {
        "acme:default": { "ws-9": { workspace: other("ws-9") } },
      },
    } as unknown as Parameters<typeof retiredWorkspaceIds>[0];

    expect(retiredWorkspaceIds(state, { teamKey: "acme:default", form: "SW-1" })).toEqual({
      open: ["sw-1-slug"],
      minimized: [],
    });
    expect(
      retiredWorkspaceIds(state, { teamKey: "acme:default", form: "ws-9", workspaceId: "ws-9" }),
    ).toEqual({ open: [], minimized: ["ws-9"] });
    expect(retiredWorkspaceIds(state, { teamKey: "acme:missing", form: "SW-1" })).toEqual({
      open: [],
      minimized: [],
    });
  });
});
