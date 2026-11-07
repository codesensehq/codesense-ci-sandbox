// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({ active: vi.fn() }));

vi.mock("@/shared/hooks/use-active-workspace-context", async (importOriginal) => {
  const real = await importOriginal<typeof import("@/shared/hooks/use-active-workspace-context")>();
  const none = { teamKey: "acme:core", workspace: null, sourceRepoSlug: null, repoPath: null };
  return {
    ...real,
    getActiveWorkspaceContext: () => {
      h.active();
      return none;
    },
  };
});

import { SHORTCUT_ACTIONS } from "./registry";

/** Acts on the tab the keyboard is on, which only the active team's CLI draws. */
const READS_FOCUSED_ELEMENT = new Set(["viewer.rename-terminal-tab"]);

/**
 * The shortcut gate lets a workspace action fire when the active team has a focused workspace, so
 * each such action must read the active team's context: one that read another would fire where it
 * can't act (and be swallowed), or be blocked where it can.
 */
describe("a workspace action reads the active team's context", () => {
  beforeEach(() => h.active.mockClear());

  it.each(
    SHORTCUT_ACTIONS.filter((a) => a.scope === "workspace" && !READS_FOCUSED_ELEMENT.has(a.id)).map(
      (a) => [a.id, a] as const,
    ),
  )("%s", (_id, action) => {
    action.run();
    expect(h.active).toHaveBeenCalled();
  });
});
