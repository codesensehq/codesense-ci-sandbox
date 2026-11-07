// SPDX-License-Identifier: MIT
import { describe, expect, it } from "vitest";
import { findOpenStoryWorkspace } from "./workspace-liveness";
import type { TerminalTab, Workspace } from "@/shared/stores/viewer-store";

// Minimal structural fixtures — only the fields the selector reads.
const story = (id: string, storyId: string): Workspace =>
  ({ id, kind: "story", storyId, cwd: `/wt/${id}`, status: "ready" }) as unknown as Workspace;
const tab = (sessionId: string, role?: string): TerminalTab =>
  ({ id: sessionId, name: "sh", sessionId, ...(role ? { role } : {}) }) as TerminalTab;

describe("findOpenStoryWorkspace (SW-GW-3 open-state oracle)", () => {
  it("finds an open story by storyId across team buckets and returns its first tab's sessionId", () => {
    const workspaces = {
      "org:teamA": [story("ws-x", "A-1")],
      "org:teamB": [story("ws-y", "GW-3")],
    };
    const tabs = {
      "org:teamB": { "ws-y": [tab("sess-9"), tab("sess-ignored")] },
    };
    expect(findOpenStoryWorkspace(workspaces, tabs, "GW-3")).toEqual({
      teamKey: "org:teamB",
      workspaceId: "ws-y",
      sessionId: "sess-9",
    });
  });

  it("prefers the role-tagged AGENT tab over the plain shell at [0] (the signal_done session)", () => {
    // Review r2 (edge, Major): a story workspace opens a plain shell at [0], and
    // the story-dev agent is a SEPARATE role-tagged tab appended after it. The
    // agent — not the shell — is what calls signal_done and whose exit means
    // "done", so it must win. (Probing [0]=shell would read live forever + never
    // match the agent's delta.)
    const workspaces = { "org:team": [story("ws-y", "GW-3")] };
    const tabs = {
      "org:team": { "ws-y": [tab("shell-0"), tab("agent-1", "story-dev:ws-y")] },
    };
    expect(findOpenStoryWorkspace(workspaces, tabs, "GW-3")).toEqual({
      teamKey: "org:team",
      workspaceId: "ws-y",
      sessionId: "agent-1",
    });
  });

  it("SW-GW-6: prefers the MAIN session (role without '#') over a named session in the same bucket", () => {
    // A story hosts N sessions in ONE bucket: the main dev session plus named
    // ones (`story-dev:<id>#<label>`). The story-keyed status read must track the
    // MAIN session, not whichever named session sorts first — even when the named
    // tab precedes the main in array order.
    const workspaces = { "org:team": [story("ws-y", "GW-6")] };
    const tabs = {
      "org:team": {
        "ws-y": [
          tab("named-arch", "story-dev:ws-y#arch-review"),
          tab("main-dev", "story-dev:ws-y"),
          tab("named-ux", "story-dev:ws-y#ux-review"),
        ],
      },
    };
    expect(findOpenStoryWorkspace(workspaces, tabs, "GW-6")).toEqual({
      teamKey: "org:team",
      workspaceId: "ws-y",
      sessionId: "main-dev",
    });
  });

  it("SW-GW-6: falls back to a named session when ONLY named sessions are open (no main tab)", () => {
    const workspaces = { "org:team": [story("ws-y", "GW-6")] };
    const tabs = {
      "org:team": { "ws-y": [tab("named-arch", "story-dev:ws-y#arch-review")] },
    };
    expect(findOpenStoryWorkspace(workspaces, tabs, "GW-6")).toMatchObject({
      sessionId: "named-arch",
    });
  });

  it("returns null when no open story workspace matches (the not-started case)", () => {
    const workspaces = { "org:teamA": [story("ws-x", "A-1")] };
    expect(findOpenStoryWorkspace(workspaces, {}, "GW-3")).toBeNull();
  });

  it("returns sessionId:null when the story is open but has no terminal tab yet", () => {
    const workspaces = { "org:teamA": [story("ws-x", "A-1")] };
    expect(findOpenStoryWorkspace(workspaces, {}, "A-1")).toEqual({
      teamKey: "org:teamA",
      workspaceId: "ws-x",
      sessionId: null,
    });
  });

  it("ignores non-story workspaces that happen to share the id shape", () => {
    const epic = {
      id: "A-1",
      kind: "epic",
      storyId: "A-1",
      cwd: "/e",
      status: "ready",
    } as unknown as Workspace;
    const workspaces = { "org:teamA": [epic] };
    expect(
      findOpenStoryWorkspace(workspaces, { "org:teamA": { "A-1": [tab("s")] } }, "A-1"),
    ).toBeNull();
  });

  it("returns null for an empty storyId (never a wildcard match)", () => {
    const workspaces = { "org:teamA": [story("ws-x", "")] };
    expect(findOpenStoryWorkspace(workspaces, {}, "")).toBeNull();
  });

  it("prefers the active team on a cross-org same-id collision (agrees with the launch side)", () => {
    // Same story_id open under two orgs (only unique WITHIN an org). Insertion
    // order would return orgA first; preferTeamKey flips it to the active org.
    const workspaces = {
      "orgA:default": [story("ws-a", "SW-GW-3")],
      "orgB:default": [story("ws-b", "SW-GW-3")],
    };
    const tabs = {
      "orgA:default": { "ws-a": [tab("sess-a")] },
      "orgB:default": { "ws-b": [tab("sess-b")] },
    };
    // No preference → first insertion-order match (unchanged behavior).
    expect(findOpenStoryWorkspace(workspaces, tabs, "SW-GW-3")).toMatchObject({
      teamKey: "orgA:default",
      sessionId: "sess-a",
    });
    // Active org = orgB → the exact-team match wins.
    expect(findOpenStoryWorkspace(workspaces, tabs, "SW-GW-3", "orgB:default")).toMatchObject({
      teamKey: "orgB:default",
      sessionId: "sess-b",
    });
    // A preferred team with no match falls back to the first match (no regression).
    expect(findOpenStoryWorkspace(workspaces, tabs, "SW-GW-3", "orgC:none")).toMatchObject({
      teamKey: "orgA:default",
    });
  });
});
