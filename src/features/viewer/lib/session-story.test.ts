// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";

import {
  buildSessionStoryIndex,
  sessionStoryFromIndex,
  type SessionStory,
  type WorkspaceSnapshot,
} from "./session-story";
import type { Workspace } from "@/shared/stores/viewer-store";

/** A minimal `kind:"story"` workspace record; overrides tune each case. */
function storyWs(over: Partial<Workspace> = {}): Workspace {
  return {
    id: "ws-am-1",
    kind: "story",
    status: "ready",
    storyId: "SW-AM-1",
    storyTitle: "Agents panel = CLI workspace manager",
    storyStatus: "in-progress",
    storyFilePath: "/repo/stories/SW-AM-1.md",
    ...over,
  } as Workspace;
}

function snapshot(over: Partial<WorkspaceSnapshot> = {}): WorkspaceSnapshot {
  return { workspaces: {}, softClosedWorkspaces: {}, ...over };
}

/** One-shot resolve: index the snapshot, then look the handle up. */
function resolveSessionStory(
  handle: string,
  snap: WorkspaceSnapshot,
  preferTeamKey?: string | null,
): SessionStory | null {
  return sessionStoryFromIndex(handle, buildSessionStoryIndex(snap, preferTeamKey));
}

describe("session story resolution", () => {
  it("resolves a live story workspace by handle → storyId", () => {
    const snap = snapshot({ workspaces: { alpha: [storyWs()] } });
    expect(resolveSessionStory("SW-AM-1", snap)).toEqual({
      storyId: "SW-AM-1",
      title: "Agents panel = CLI workspace manager",
      status: "in-progress",
      storyFilePath: "/repo/stories/SW-AM-1.md",
      teamKey: "alpha",
      workspaceId: "ws-am-1",
    });
  });

  it("strips a `#label` fork suffix from the handle before matching", () => {
    const snap = snapshot({ workspaces: { alpha: [storyWs()] } });
    expect(resolveSessionStory("SW-AM-1#attempt-2", snap)?.storyId).toBe("SW-AM-1");
  });

  it("resolves a minimized (soft-closed) workspace with no live tab", () => {
    const snap = snapshot({
      softClosedWorkspaces: { alpha: { "ws-am-1": { workspace: storyWs() } } },
    });
    const resolved = resolveSessionStory("SW-AM-1", snap);
    expect(resolved?.status).toBe("in-progress");
    expect(resolved?.workspaceId).toBe("ws-am-1");
  });

  it("finds a story in a non-active team (cross-team, tabless)", () => {
    const snap = snapshot({
      workspaces: { other: [storyWs({ id: "ws-x", storyId: "SW-AM-1" })] },
    });
    expect(resolveSessionStory("SW-AM-1", snap)?.teamKey).toBe("other");
  });

  it("prefers the host team when the same storyId is open in two teams", () => {
    const snap = snapshot({
      workspaces: {
        other: [storyWs({ id: "ws-other", storyStatus: "todo" })],
        host: [storyWs({ id: "ws-host", storyStatus: "review" })],
      },
    });
    // Without a preference, whichever team enumerates first wins (here "other").
    // With the host preference, the host team's record wins the tie.
    const resolved = resolveSessionStory("SW-AM-1", snap, "host");
    expect(resolved?.teamKey).toBe("host");
    expect(resolved?.workspaceId).toBe("ws-host");
  });

  it("lets a live non-host tile outrank a host-team soft-closed shadow (liveness > team)", () => {
    const snap = snapshot({
      workspaces: { other: [storyWs({ id: "ws-live", storyStatus: "review" })] },
      softClosedWorkspaces: {
        host: { "ws-shadow": { workspace: storyWs({ id: "ws-shadow", storyStatus: "todo" }) } },
      },
    });
    // Even with host preference, the live tile wins over the host's minimized shadow.
    const resolved = resolveSessionStory("SW-AM-1", snap, "host");
    expect(resolved?.workspaceId).toBe("ws-live");
    expect(resolved?.teamKey).toBe("other");
  });

  it("returns null for a non-story handle (`::` branch handle)", () => {
    const snap = snapshot({ workspaces: { alpha: [storyWs()] } });
    expect(resolveSessionStory("myrepo::feature-branch", snap)).toBeNull();
  });

  it("returns null for a `/`-bearing epic/path handle", () => {
    const snap = snapshot({ workspaces: { alpha: [storyWs()] } });
    expect(resolveSessionStory("team/epic/README.md", snap)).toBeNull();
  });

  it("returns null when no workspace record backs the story (fully closed → AC-6)", () => {
    expect(resolveSessionStory("SW-AM-1", snapshot())).toBeNull();
  });

  it("returns null when the workspace lacks a storyStatus (no chip, like the pill)", () => {
    const snap = snapshot({
      workspaces: { alpha: [storyWs({ storyStatus: undefined })] },
    });
    expect(resolveSessionStory("SW-AM-1", snap)).toBeNull();
  });

  it("returns null when the workspace lacks a storyFilePath (not writable)", () => {
    const snap = snapshot({
      workspaces: { alpha: [storyWs({ storyFilePath: undefined })] },
    });
    expect(resolveSessionStory("SW-AM-1", snap)).toBeNull();
  });

  it("ignores a same-id workspace of a non-story kind", () => {
    const snap = snapshot({
      workspaces: { alpha: [storyWs({ kind: "epic" })] },
    });
    expect(resolveSessionStory("SW-AM-1", snap)).toBeNull();
  });

  it("returns null for a not-yet-ready workspace (shared showsStoryStatus gate)", () => {
    const snap = snapshot({
      workspaces: { alpha: [storyWs({ status: "creating" })] },
    });
    expect(resolveSessionStory("SW-AM-1", snap)).toBeNull();
  });

  it("prefers a live tile over a soft-closed shadow of the same story", () => {
    const snap = snapshot({
      workspaces: { alpha: [storyWs({ id: "live-ws", storyStatus: "review" })] },
      softClosedWorkspaces: {
        alpha: { "shadow-ws": { workspace: storyWs({ id: "shadow-ws", storyStatus: "todo" }) } },
      },
    });
    const resolved = resolveSessionStory("SW-AM-1", snap);
    expect(resolved?.workspaceId).toBe("live-ws");
    expect(resolved?.status).toBe("review");
  });
});

describe("buildSessionStoryIndex + sessionStoryFromIndex", () => {
  it("indexes by storyId once for O(1) per-row lookups", () => {
    const snap = snapshot({
      workspaces: { alpha: [storyWs(), storyWs({ id: "ws-2", storyId: "SW-AM-2" })] },
    });
    const index = buildSessionStoryIndex(snap);
    expect(index.size).toBe(2);
    expect(sessionStoryFromIndex("SW-AM-1#fork", index)?.workspaceId).toBe("ws-am-1");
    expect(sessionStoryFromIndex("SW-AM-2", index)?.storyId).toBe("SW-AM-2");
    expect(sessionStoryFromIndex("unknown", index)).toBeNull();
    expect(sessionStoryFromIndex("repo::branch", index)).toBeNull();
  });
});
