// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";

// Mock the command + store boundaries so the test exercises the add-vs-remove
// dispatch in isolation. `withErrorToast` is stubbed (it only swallows+toasts), so
// the mocked commands need not return real promises. Spies are hoisted so the
// `vi.mock` factories can reference them.
const {
  assembleEpicWorktrees,
  reconcileEpicWorktrees,
  collectInUsePathsFromStore,
  withErrorToast,
  warn,
} = vi.hoisted(() => ({
  assembleEpicWorktrees: vi.fn(() => Promise.resolve()),
  reconcileEpicWorktrees: vi.fn(() => Promise.resolve()),
  collectInUsePathsFromStore: vi.fn(() => ["/live/cwd"]),
  withErrorToast: vi.fn(),
  warn: vi.fn(),
}));
vi.mock("@/tauri/commands", () => ({ assembleEpicWorktrees, reconcileEpicWorktrees }));
vi.mock("./collect-in-use-paths", () => ({ collectInUsePathsFromStore }));
vi.mock("@/shared/lib/with-error-toast", () => ({ withErrorToast }));
vi.mock("@/shared/lib/logger", () => ({ logger: { warn } }));

import { syncEpicWorktreesAfterRepoChange } from "./sync-epic-worktrees-after-repo-change";

const CTX = { codesenseHome: "/home", githubUserId: 42, orgSlug: "acme", team: "default" };
const DOC = "/ctx/epic-workspaces/SW-EW.md";

beforeEach(() => vi.clearAllMocks());

describe("syncEpicWorktreesAfterRepoChange", () => {
  it("an add assembles the new repo and never reconciles", () => {
    syncEpicWorktreesAfterRepoChange(CTX, DOC, { assemble: true });
    expect(assembleEpicWorktrees).toHaveBeenCalledWith({ ...CTX, epicFilePath: DOC });
    expect(reconcileEpicWorktrees).not.toHaveBeenCalled();
    expect(collectInUsePathsFromStore).not.toHaveBeenCalled();
    expect(withErrorToast).toHaveBeenCalledTimes(1);
  });

  it("a remove reconciles with the live in-use paths and never assembles", () => {
    syncEpicWorktreesAfterRepoChange(CTX, DOC, { assemble: false });
    // The in-use guard MUST be forwarded — dropping it would delete a live worktree.
    expect(reconcileEpicWorktrees).toHaveBeenCalledWith({ ...CTX, epicFilePath: DOC }, [
      "/live/cwd",
    ]);
    expect(collectInUsePathsFromStore).toHaveBeenCalledTimes(1);
    expect(assembleEpicWorktrees).not.toHaveBeenCalled();
    expect(withErrorToast).toHaveBeenCalledTimes(1);
  });

  it("an unresolved context is logged, not dispatched (either branch)", () => {
    syncEpicWorktreesAfterRepoChange(null, DOC, { assemble: false });
    syncEpicWorktreesAfterRepoChange(null, DOC, { assemble: true });
    expect(assembleEpicWorktrees).not.toHaveBeenCalled();
    expect(reconcileEpicWorktrees).not.toHaveBeenCalled();
    expect(withErrorToast).not.toHaveBeenCalled();
    expect(warn).toHaveBeenCalledTimes(2);
  });
});
