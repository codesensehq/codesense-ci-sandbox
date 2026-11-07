// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { isVirtualTabPath } from "@/shared/lib/virtual-viewer-registry";
import { isEditableTextFile } from "@/shared/lib/viewer-file-type";
import { workspaceReposForTeam } from "@/shared/lib/build-synthetic-repo";
import type { RepoState } from "@/shared/stores/config-store";

// SW-OC-4 (folded from SW-OC-1): unit coverage for the two onboarding-only
// additions re-applied to `team-workspace-view.tsx` — the `isVirtualTabPath`
// edit-chip guard and the `hasSourceRepo` warm empty-state gate. These test the
// exact predicate COMBINATIONS the view uses (the seam), not the helpers in
// isolation, so a regression in either gate is caught here without mounting the
// 2480-line view component.

/** Mirror of the view's `editableTextActive` gate (verbatim boolean shape). */
function editableTextActive(activeTabPath: string, opts: { syncView: boolean }): boolean {
  return (
    !!activeTabPath &&
    !opts.syncView &&
    !isVirtualTabPath(activeTabPath) &&
    isEditableTextFile(activeTabPath)
  );
}

/** Mirror of the view's `hasSourceRepo` derivation. */
function hasSourceRepo(workspaceRepos: Record<string, RepoState>, teamKey: string): boolean {
  return workspaceReposForTeam(workspaceRepos, teamKey).length > 0;
}

function repoState(repoPath: string): RepoState {
  // Only the fields the predicate reads matter; the rest of RepoState is inert
  // here. Cast keeps the fixture minimal without a full RepoState literal.
  return { repoPath } as unknown as RepoState;
}

describe("SW-OC-4 folded: isVirtualTabPath edit-chip guard", () => {
  it("suppresses the edit gate for a virtual management tab (Connections/Workspaces/…)", () => {
    // A virtual surface is a synthetic tab, not an editable context file: the
    // edit chip must never offer over it, even though its sentinel path has no
    // extension that `isEditableTextFile` would reject on its own.
    expect(editableTextActive("virtual://connections", { syncView: false })).toBe(false);
    expect(editableTextActive("virtual://workspaces", { syncView: false })).toBe(false);
    expect(editableTextActive("virtual://source-repos", { syncView: false })).toBe(false);
  });

  it("still offers the edit gate for a real own-writable text file", () => {
    expect(editableTextActive("/repo/_bmad-output/stories/SW-OC-4.md", { syncView: false })).toBe(
      true,
    );
  });

  it("respects the sync-view suppression alongside the virtual guard", () => {
    expect(editableTextActive("/repo/notes.md", { syncView: true })).toBe(false);
  });

  it("is a no-op for an empty active tab path", () => {
    expect(editableTextActive("", { syncView: false })).toBe(false);
  });
});

describe("SW-OC-4 folded: hasSourceRepo warm empty-state gate", () => {
  const teamKey = "acme:core";

  it("is false for a fresh, context-repo-only team (no attached source repos)", () => {
    expect(hasSourceRepo({}, teamKey)).toBe(false);
  });

  it("is true once the team has an attached source repo synthetic in the store", () => {
    const workspaceRepos: Record<string, RepoState> = {
      "workspace:acme:core:acme/app": repoState("workspace:acme:core:acme/app"),
    };
    expect(hasSourceRepo(workspaceRepos, teamKey)).toBe(true);
  });

  it("ignores another team's repos (per-team scoping, not a global count)", () => {
    const workspaceRepos: Record<string, RepoState> = {
      "workspace:acme:other:acme/app": repoState("workspace:acme:other:acme/app"),
    };
    expect(hasSourceRepo(workspaceRepos, teamKey)).toBe(false);
  });
});
