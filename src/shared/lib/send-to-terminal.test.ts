// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, type Mock } from "vitest";
import {
  canPasteToTerminal,
  pasteToActiveTerminal,
  sendTextToTerminal,
  subscribePasteTarget,
} from "./send-to-terminal";
import { writePty } from "@/tauri/commands";
import { toast } from "sonner";
import {
  getActiveWorkspaceTabs,
  getActiveWorkspaceActiveTab,
} from "@/features/viewer/hooks/use-active-workspace-terminal-tabs";
import { getActiveWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { resolveRepo } from "@/shared/lib/resolve-repo";
import { resolveSourceRepoPath } from "@/shared/lib/resolve-source-repo-path";
import { useOrgStore } from "@/shared/stores/org-store";

// T.6: send-to-terminal reads (teamKey, workspace, repoPath) from
// getActiveWorkspaceContext() instead of selectedRepoPath. The mock lets
// each test drive the active-workspace fixture explicitly.
//
// SW-FX-28: `workspace` and `repoPath` are decoupled. A focused epic
// (`kind:"integration"`) workspace is slug-less, so its identity `repoPath`
// is null even though a workspace IS focused — drive that here by setting
// `mockWorkspace.kind = "integration"` with `mockRepoPath = null`. A `null`
// `mockWorkspace` simulates "nothing focused".
type MockWorkspace = {
  id: string;
  kind: "root" | "story" | "epic" | "generic";
  cwd: string;
  sourceRepoPath?: string;
  status: string;
} | null;

let mockWorkspace: MockWorkspace = {
  id: "w1",
  kind: "root",
  cwd: "/test-repo",
  sourceRepoPath: "/test-repo",
  status: "ready",
};
// `repoPath` is the synthetic workspace key the live hook derives via
// `buildSyntheticRepoPathFromPrompt(teamKey + sourceRepoSlug)` — null when
// the workspace is slug-less (every epic workspace).
let mockRepoPath: string | null = "/test-repo";

const buildCtx = () => ({
  teamKey: "test-org:default",
  workspace: mockWorkspace,
  sourceRepoSlug: null,
  repoPath: mockRepoPath,
});

vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  useActiveWorkspaceContext: () => buildCtx(),
  getActiveWorkspaceContext: () => buildCtx(),
}));

const mockShowNoClonePrompt = vi.fn();
const viewerListeners = new Set<(s: object, prev: object) => void>();
vi.mock("@/shared/stores/viewer-store", () => {
  const store = () => ({});
  store.getState = () => ({ showNoClonePrompt: mockShowNoClonePrompt });
  store.subscribe = (listener: (s: object, prev: object) => void) => {
    viewerListeners.add(listener);
    return () => viewerListeners.delete(listener);
  };
  return {
    useViewerStore: store,
    promptIdentityFromRepoPath: vi.fn(() => null),
  };
});

vi.mock("@/tauri/commands", () => ({
  writePty: vi.fn(() => Promise.resolve()),
}));

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn() }),
}));

vi.mock("@/features/viewer/hooks/use-active-workspace-terminal-tabs", () => ({
  getActiveWorkspaceTabs: vi.fn(() => []),
  getActiveWorkspaceActiveTab: vi.fn(() => undefined),
}));

vi.mock("@/shared/lib/resolve-repo", () => ({
  resolveRepo: vi.fn(() => null),
}));

vi.mock("@/shared/lib/resolve-source-repo-path", () => ({
  resolveSourceRepoPath: vi.fn(() => "/test-repo"),
}));

beforeEach(() => {
  vi.clearAllMocks();
  mockWorkspace = {
    id: "w1",
    kind: "root",
    cwd: "/test-repo",
    sourceRepoPath: "/test-repo",
    status: "ready",
  };
  mockRepoPath = "/test-repo";
  (resolveRepo as Mock).mockReturnValue(null);
});

describe("sendTextToTerminal", () => {
  it("sends text to the active terminal tab via writePty", () => {
    (getActiveWorkspaceActiveTab as Mock).mockReturnValue("tab-1");
    (getActiveWorkspaceTabs as Mock).mockReturnValue([
      { id: "tab-1", sessionId: "sess-1", name: "Terminal 1" },
    ]);

    sendTextToTerminal("echo hello");

    expect(writePty).toHaveBeenCalledWith("sess-1", "echo hello");
    expect(toast).not.toHaveBeenCalled();
  });

  it("sends to shell tab in worktree context (no claude tab)", () => {
    (getActiveWorkspaceActiveTab as Mock).mockReturnValue("wt-tab");
    (getActiveWorkspaceTabs as Mock).mockReturnValue([
      { id: "wt-tab", sessionId: "wt-sess", name: "Terminal 1" },
    ]);

    sendTextToTerminal("cargo test");

    expect(writePty).toHaveBeenCalledWith("wt-sess", "cargo test");
    expect(toast).not.toHaveBeenCalled();
  });

  it("shows 'No terminal active' toast when no active tab", () => {
    (getActiveWorkspaceActiveTab as Mock).mockReturnValue(undefined);
    (getActiveWorkspaceTabs as Mock).mockReturnValue([]);

    sendTextToTerminal("echo hello");

    expect(writePty).not.toHaveBeenCalled();
    expect(toast).toHaveBeenCalledWith("No terminal active", { duration: 3000 });
  });

  it("shows toast when active tab ID doesn't match any tab", () => {
    (getActiveWorkspaceActiveTab as Mock).mockReturnValue("stale-id");
    (getActiveWorkspaceTabs as Mock).mockReturnValue([
      { id: "tab-1", sessionId: "sess-1", name: "Terminal 1" },
    ]);

    sendTextToTerminal("echo hello");

    expect(writePty).not.toHaveBeenCalled();
    expect(toast).toHaveBeenCalledWith("No terminal active", { duration: 3000 });
  });

  it("shows 'No workspace open' toast when no workspace is focused (T.6)", () => {
    // T.6 / SW-FX-28: the early-return predicate is now `!workspace`.
    mockWorkspace = null;
    mockRepoPath = null;

    sendTextToTerminal("echo hello");

    expect(writePty).not.toHaveBeenCalled();
    expect(toast).toHaveBeenCalledWith("No workspace open", { duration: 3000 });
  });

  it("shows error toast with details when writePty rejects", async () => {
    (getActiveWorkspaceActiveTab as Mock).mockReturnValue("tab-1");
    (getActiveWorkspaceTabs as Mock).mockReturnValue([
      { id: "tab-1", sessionId: "sess-1", name: "Terminal 1" },
    ]);
    (writePty as Mock).mockRejectedValueOnce(new Error("IPC channel closed"));

    sendTextToTerminal("echo hello");

    // Flush the microtask queue so the .catch() handler fires
    await new Promise((r) => setTimeout(r, 0));

    expect(toast.error).toHaveBeenCalledWith("Failed to send to terminal: IPC channel closed");
  });

  it("stays SILENT when writePty rejects with an unknown-session race (SW-EW-39)", async () => {
    (getActiveWorkspaceActiveTab as Mock).mockReturnValue("tab-1");
    (getActiveWorkspaceTabs as Mock).mockReturnValue([
      { id: "tab-1", sessionId: "sess-1", name: "Terminal 1" },
    ]);
    // write_pty now rejects an unknown session instead of a silent Ok; a target
    // whose PTY is still registering is a transient race — no scary toast.
    (writePty as Mock).mockRejectedValueOnce(new Error("write_pty: session not found: sess-1"));

    sendTextToTerminal("echo hello");
    await new Promise((r) => setTimeout(r, 0));

    expect(toast.error).not.toHaveBeenCalled();
  });

  // SW-FX-28: a focused epic (`kind:"integration"`) workspace is slug-less, so
  // its identity `repoPath` is null even though a workspace IS focused. The send
  // must resolve the epic's own terminal tab and bypass the story-only
  // synthetic-clone / no-clone gate entirely.
  describe("epic (integration) workspace", () => {
    beforeEach(() => {
      mockWorkspace = { id: "epic1", kind: "epic", cwd: "/epic/dir", status: "ready" };
      mockRepoPath = null;
    });

    it("sends to the epic's active terminal tab even though repoPath is null (AC-1/AC-2)", () => {
      (getActiveWorkspaceActiveTab as Mock).mockReturnValue("etab-1");
      (getActiveWorkspaceTabs as Mock).mockReturnValue([
        { id: "etab-1", sessionId: "esess-1", name: "Terminal 1" },
      ]);

      sendTextToTerminal("/some/path");

      expect(writePty).toHaveBeenCalledWith("esess-1", "/some/path");
      expect(toast).not.toHaveBeenCalled();
    });

    it("bypasses the no-clone gate (never consults resolveRepo / showNoClonePrompt) (AC-3)", () => {
      // Force the no-clone TRIGGER: a resolvable repo whose source path is null
      // would surface the attach modal for a STORY workspace. An epic workspace
      // must skip that block and send anyway.
      (resolveRepo as Mock).mockReturnValue({ id: "r" });
      (getActiveWorkspaceActiveTab as Mock).mockReturnValue("etab-1");
      (getActiveWorkspaceTabs as Mock).mockReturnValue([
        { id: "etab-1", sessionId: "esess-1", name: "Terminal 1" },
      ]);

      sendTextToTerminal("/some/path");

      expect(resolveRepo).not.toHaveBeenCalled();
      expect(mockShowNoClonePrompt).not.toHaveBeenCalled();
      expect(writePty).toHaveBeenCalledWith("esess-1", "/some/path");
    });

    it("shows an actionable, non-transient error (not the silent flash) when the epic has no tab yet (AC-4)", () => {
      (getActiveWorkspaceActiveTab as Mock).mockReturnValue(undefined);
      (getActiveWorkspaceTabs as Mock).mockReturnValue([]);

      sendTextToTerminal("/some/path");

      expect(writePty).not.toHaveBeenCalled();
      // Distinct from the story-path "No terminal active" 3s flash.
      expect(toast).not.toHaveBeenCalledWith("No terminal active", { duration: 3000 });
      expect(toast.error).toHaveBeenCalledWith(
        "Epic workspace is still starting its terminal — try again in a moment.",
      );
    });
  });

  // SW-AW-5 (AC-2): a slug-less workspace (generic / epic) has a real dir cwd
  // but `repoPath` is null. Before this story it fell into the `!repoPath`
  // early-return ("No workspace open") and never reached the terminal
  // (deferred-work.md:6 clone-gate bug). It must now skip the synthetic-clone
  // gate via the `skipsCloneGate` capability and send to its own tab.
  describe("generic workspace (AC-2)", () => {
    beforeEach(() => {
      mockWorkspace = { id: "gs1", kind: "generic", cwd: "/org/root", status: "ready" };
      mockRepoPath = null;
    });

    it("sends to the generic workspace's active tab even though repoPath is null", () => {
      (getActiveWorkspaceActiveTab as Mock).mockReturnValue("gtab-1");
      (getActiveWorkspaceTabs as Mock).mockReturnValue([
        { id: "gtab-1", sessionId: "gsess-1", name: "Terminal 1" },
      ]);

      sendTextToTerminal("git status");

      expect(writePty).toHaveBeenCalledWith("gsess-1", "git status");
      expect(toast).not.toHaveBeenCalled();
    });

    it("bypasses the no-clone gate (never consults resolveRepo / showNoClonePrompt)", () => {
      // Force the no-clone TRIGGER shape: a resolvable repo whose source path is
      // null would surface the attach modal for a STORY workspace. A generic
      // workspace must skip that block and send anyway.
      (resolveRepo as Mock).mockReturnValue({ id: "r" });
      (getActiveWorkspaceActiveTab as Mock).mockReturnValue("gtab-1");
      (getActiveWorkspaceTabs as Mock).mockReturnValue([
        { id: "gtab-1", sessionId: "gsess-1", name: "Terminal 1" },
      ]);

      sendTextToTerminal("git status");

      expect(resolveRepo).not.toHaveBeenCalled();
      expect(mockShowNoClonePrompt).not.toHaveBeenCalled();
      expect(writePty).toHaveBeenCalledWith("gsess-1", "git status");
    });
  });

  // SW-AW-5 (AC-2) characterization: `root` also skips the gate now (it resolves a
  // real repo cwd). This is the deliberate root behavior change the story flags —
  // in the forced synthetic-no-clone shape, root used to hit the gate; it now
  // bypasses it like git-sync/epic. Pinned so the flip is intentional, not silent.
  describe("root workspace — skips the clone gate (deliberate flip)", () => {
    beforeEach(() => {
      mockWorkspace = {
        id: "root1",
        kind: "root",
        cwd: "/repo",
        sourceRepoPath: "/repo",
        status: "ready",
      };
      mockRepoPath = "/repo";
    });

    it("does not consult the no-clone gate (sends straight to the active tab)", () => {
      (resolveRepo as Mock).mockReturnValue({ id: "r" });
      (resolveSourceRepoPath as Mock).mockReturnValueOnce(null); // synthetic-no-clone shape
      (getActiveWorkspaceActiveTab as Mock).mockReturnValue("rtab-1");
      (getActiveWorkspaceTabs as Mock).mockReturnValue([
        { id: "rtab-1", sessionId: "rsess-1", name: "Terminal 1" },
      ]);

      sendTextToTerminal("ls");

      expect(resolveRepo).not.toHaveBeenCalled();
      expect(mockShowNoClonePrompt).not.toHaveBeenCalled();
      expect(writePty).toHaveBeenCalledWith("rsess-1", "ls");
    });
  });

  // SW-AW-5 (AC-2) characterization: the synthetic-clone gate must STILL fire for
  // `story` after the migration off the `kind === "epic"` literal — story's cwd
  // depends on an attached clone, so `skipsCloneGate` is false for it.
  describe("story workspace — clone gate still fires (characterization)", () => {
    beforeEach(() => {
      mockWorkspace = {
        id: "s1",
        kind: "story",
        cwd: "/wt/story",
        sourceRepoPath: "/wt/story",
        status: "ready",
      };
      mockRepoPath = "team:my-repo"; // synthetic slug-key
    });

    it("consults the no-clone gate when the source repo has no attached clone", () => {
      (resolveRepo as Mock).mockReturnValue({ id: "r" });
      (resolveSourceRepoPath as Mock).mockReturnValueOnce(null); // no clone attached
      (getActiveWorkspaceActiveTab as Mock).mockReturnValue("stab-1");
      (getActiveWorkspaceTabs as Mock).mockReturnValue([
        { id: "stab-1", sessionId: "ssess-1", name: "Terminal 1" },
      ]);

      sendTextToTerminal("npm test");

      // Gate fired: resolveRepo consulted, send NOT dispatched to the tab.
      expect(resolveRepo).toHaveBeenCalled();
      expect(writePty).not.toHaveBeenCalled();
    });
  });
});

describe("the active-terminal paste", () => {
  const tabs = [{ id: "tab-1", sessionId: "sess-1", name: "Terminal 1" }];
  const selectTab = (id: string | undefined) => {
    (getActiveWorkspaceActiveTab as Mock).mockReturnValue(id);
    (getActiveWorkspaceTabs as Mock).mockReturnValue(id ? tabs : []);
  };

  it("types into the selected tab without Enter and reports typed", async () => {
    selectTab("tab-1");
    expect(canPasteToTerminal()).toBe(true);
    await expect(pasteToActiveTerminal("line one\n  two ✓")).resolves.toEqual({
      status: "typed",
      message: null,
    });
    expect(writePty).toHaveBeenCalledWith("sess-1", "line one\n  two ✓");
    expect(toast).not.toHaveBeenCalled();
  });

  it("reports no-terminal with classic Notes' toast when the workspace has none", async () => {
    selectTab(undefined);
    expect(canPasteToTerminal()).toBe(false);
    await expect(pasteToActiveTerminal("x")).resolves.toEqual({
      status: "no-terminal",
      message: "No terminal active",
    });
    expect(writePty).not.toHaveBeenCalled();
    expect(toast).toHaveBeenCalledWith("No terminal active", { duration: 3000 });
  });

  it("reports no-workspace when nothing is focused", async () => {
    mockWorkspace = null;
    selectTab("tab-1");
    expect(canPasteToTerminal()).toBe(false);
    await expect(pasteToActiveTerminal("x")).resolves.toEqual({
      status: "no-workspace",
      message: "No workspace open",
    });
    expect(writePty).not.toHaveBeenCalled();
    expect(toast).toHaveBeenCalledWith("No workspace open", { duration: 3000 });
  });

  it("reports no-terminal when the session ended before the write", async () => {
    selectTab("tab-1");
    (writePty as Mock).mockRejectedValueOnce(new Error("write_pty: session not found: sess-1"));
    await expect(pasteToActiveTerminal("x")).resolves.toMatchObject({ status: "no-terminal" });
  });

  it("rejects a genuine write failure instead of claiming typed", async () => {
    selectTab("tab-1");
    (writePty as Mock).mockRejectedValueOnce(new Error("IPC channel closed"));
    await expect(pasteToActiveTerminal("x")).rejects.toThrow("IPC channel closed");
  });

  it("never opens the attach-a-clone prompt: it pastes exactly when it can", async () => {
    mockWorkspace = { id: "s1", kind: "story", cwd: "/wt/s", status: "ready" };
    mockRepoPath = "team:my-repo";
    (resolveRepo as Mock).mockReturnValue({ id: "r" });
    (resolveSourceRepoPath as Mock).mockReturnValueOnce(null);
    selectTab("tab-1");
    await expect(pasteToActiveTerminal("x")).resolves.toMatchObject({ status: "typed" });
    expect(mockShowNoClonePrompt).not.toHaveBeenCalled();
  });

  it("returns the toast the host showed: an epic still starting its terminal", async () => {
    mockWorkspace = { id: "e1", kind: "epic", cwd: "/epic", status: "ready" };
    mockRepoPath = null;
    selectTab(undefined);
    const message = "Epic workspace is still starting its terminal — try again in a moment.";
    await expect(pasteToActiveTerminal("x")).resolves.toEqual({ status: "no-terminal", message });
    expect(toast.error).toHaveBeenCalledWith(message);
  });

  it("an exited tab, open read-only, is no paste target", async () => {
    (getActiveWorkspaceActiveTab as Mock).mockReturnValue("tab-1");
    (getActiveWorkspaceTabs as Mock).mockReturnValue([
      { ...tabs[0], exit: { code: 0, signal: null } },
    ]);
    expect(canPasteToTerminal()).toBe(false);
    await expect(pasteToActiveTerminal("x")).resolves.toMatchObject({ status: "no-terminal" });
    expect(writePty).not.toHaveBeenCalled();
  });

  it("notifies once per change of the answer, not per store change", () => {
    selectTab(undefined);
    const seen: boolean[] = [];
    const off = subscribePasteTarget((can) => seen.push(can));
    // A layout change: the slice the subscription watches gets a new reference.
    const changed = () =>
      viewerListeners.forEach((listener) =>
        listener({ terminalLayout: {} }, { terminalLayout: {} }),
      );
    changed();
    selectTab("tab-1");
    changed();
    changed();
    selectTab(undefined);
    changed();
    selectTab("tab-1");
    changed();
    (getActiveWorkspaceTabs as Mock).mockReturnValue([
      { ...tabs[0], exit: { code: 0, signal: null } },
    ]);
    changed();
    off();
    selectTab("tab-1");
    changed();
    expect(seen).toEqual([true, false, true, false]);
  });

  it("ignores viewer-store writes that leave its slices alone (status and progress ticks)", () => {
    selectTab(undefined);
    const seen: boolean[] = [];
    const off = subscribePasteTarget((can) => seen.push(can));
    selectTab("tab-1");
    const same = { terminalLayout: {} };
    viewerListeners.forEach((listener) => listener(same, same));
    off();
    expect(seen).toEqual([]);
  });

  it("follows a team switch, which changes only the org store", () => {
    selectTab(undefined);
    const seen: boolean[] = [];
    const off = subscribePasteTarget((can) => seen.push(can));
    selectTab("tab-1");
    useOrgStore.setState({ activeTeamName: "other" });
    off();
    expect(seen).toEqual([true]);
  });
});

// Silence "value imported but never used" warning — these are imported
// for their side-effect (Mock typing) above.
void getActiveWorkspaceContext;
