// SPDX-License-Identifier: MIT
import { openAgentInboxRail } from "@/features/viewer/stores/agent-inbox-rail-store";
import { REGISTERED_AGENT_CLIS } from "@/shared/lib/agent-runtimes";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import type React from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, act, cleanup, fireEvent, screen, waitFor } from "@/test-utils";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => undefined),
  emit: vi.fn(),
}));

// SW-TP CAP-11 default: the prompt resolver answers no cwd so the caption
// falls back to tab.name for assertions that don't exercise the cwd path.
const NO_PROMPT = { cwd: null, home: null, git: null };
const sessionPromptMock = vi.fn().mockResolvedValue(NO_PROMPT);
const agentReadSessionsMock = vi.fn().mockResolvedValue([]);

vi.mock("@/features/inbox/api", async (original) => ({
  ...(await original<typeof import("@/features/inbox/api")>()),
  loadInboxSessions: () => agentReadSessionsMock(),
}));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsRunningProcesses: vi.fn().mockResolvedValue([{ foreground: null, background: [] }]),
  sessionPrompt: (ref: unknown) => sessionPromptMock(ref),
  agentReadSessions: () => agentReadSessionsMock(),
}));

// `useTabSourceRepo` is heavy — stub it per test so the capability gates are
// controllable without a real resolver.
const useTabSourceRepoMock = vi.fn();
vi.mock("@/features/viewer/hooks/use-tab-source-repo", () => ({
  useTabSourceRepo: (ws: unknown) => useTabSourceRepoMock(ws),
}));

import { TerminalGridCaption } from "./terminal-grid-caption";
import { useAgentInboxRailStore } from "@/features/viewer/stores/agent-inbox-rail-store";
import { useInboxSessionsStore } from "@/features/inbox/hooks/use-inbox-sessions";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { resetSessionPrompts } from "@/features/viewer/hooks/use-session-prompt";
import { takeSourceDiffReturnFocus } from "./source-diff-return-focus";

const story = { id: "ws-story", kind: "story", cwd: "/w/story", status: "ready" } as const;
const epic = { id: "ws-epic", kind: "epic", cwd: "/w/epic", status: "ready" } as const;
const generic = { id: "ws-gen", kind: "generic", cwd: "/w/gen", status: "ready" } as const;

const tab = {
  id: "tab-a",
  sessionId: "sid-a",
  name: "barb",
  exit: null,
  origin: null,
};

function mountFor(
  workspace: typeof story | typeof epic | typeof generic,
  overrides: Partial<React.ComponentProps<typeof TerminalGridCaption>> = {},
) {
  return render(
    <TerminalGridCaption
      tab={tab as never}
      isActive={true}
      background="#000"
      foreground="#fff"
      onFocusCell={() => undefined}
      onZoomCell={() => undefined}
      onClose={() => undefined}
      workspace={workspace as never}
      teamKey="t1"
      {...overrides}
    />,
  );
}

beforeEach(() => {
  useAgentStateStore.setState({
    bySession: {},
    activityBySession: {},
    exitBySession: {},
    processAgentBySession: { "sid-a": "codex" },
  });
  useAgentInboxRailStore.setState({ bySession: {} });
  useViewerStore.setState({
    sourceDiffShownBySession: {},
    worktreeEditorOpenBySession: {},
    storyHudShown: {},
    sourceDiffSelectedFile: {},
    sourceDiffMode: {},
    sourceDiffBaseMode: {},
    sourceDiffSelectedSha: {},
    sourceDiffHistoryMode: {},
    sourceDiffViewOptions: {},
    progressBySession: {},
  });
  useTabSourceRepoMock.mockReturnValue({ folder: null, repo: null });
  vi.clearAllMocks();
  resetSessionPrompts();
  sessionPromptMock.mockResolvedValue(NO_PROMPT);
});

describe("TerminalGridCaption Inbox toggle (SW-TP CAP-10)", () => {
  it.each(["gemini", "aider", "cursor-agent"])("hides inbox controls for detected %s", (cli) => {
    useAgentStateStore.getState().setProcessAgent("sid-a", cli);
    mountFor(story);
    expect(screen.queryByTestId("terminal-grid-caption-inbox-tab-a")).toBeNull();
    openAgentInboxRail("sid-a");
    expect(useAgentInboxRailStore.getState().bySession["sid-a"]).toBeUndefined();
    cleanup();
  });
  it("hides the inbox on a shell, restores it for a live agent, and removes it at exit", () => {
    useAgentStateStore.getState().setProcessAgent("sid-a", null);
    mountFor(story);
    expect(screen.queryByTestId("terminal-grid-caption-inbox-tab-a")).toBeNull();
    act(() => useAgentStateStore.getState().setProcessAgent("sid-a", "codex"));
    expect(screen.getByTestId("terminal-grid-caption-inbox-tab-a")).toBeInTheDocument();
    act(() => useAgentStateStore.getState().setProcessAgent("sid-a", null));
    expect(screen.queryByTestId("terminal-grid-caption-inbox-tab-a")).toBeNull();
    cleanup();
  });

  it.each(REGISTERED_AGENT_CLIS.map((row) => row.runtime))(
    "opens the inbox for detected %s on the tab's sessionId on first click",
    (cli) => {
      useAgentStateStore.getState().setProcessAgent("sid-a", cli);
      const { getByTestId } = mountFor(story);
      expect(useAgentInboxRailStore.getState().bySession["sid-a"]).toBeUndefined();
      const inbox = getByTestId("terminal-grid-caption-inbox-tab-a");
      expect(inbox).toHaveAccessibleName("Inbox");
      fireEvent.click(inbox);
      expect(useAgentInboxRailStore.getState().bySession["sid-a"]).toBeDefined();
      cleanup();
    },
  );

  it.each(REGISTERED_AGENT_CLIS.map((row) => row.runtime))(
    "closes the inbox for detected %s on the next click",
    (cli) => {
      useAgentStateStore.getState().setProcessAgent("sid-a", cli);
      const { getByTestId } = mountFor(story);
      fireEvent.click(getByTestId("terminal-grid-caption-inbox-tab-a"));
      fireEvent.click(getByTestId("terminal-grid-caption-inbox-tab-a"));
      expect(useAgentInboxRailStore.getState().bySession["sid-a"]).toBeUndefined();
      cleanup();
    },
  );

  it("stays reliable under rapid repeated clicks (10 toggles → closed)", () => {
    const { getByTestId } = mountFor(story);
    const btn = getByTestId("terminal-grid-caption-inbox-tab-a");
    for (let i = 0; i < 10; i++) fireEvent.click(btn);
    expect(useAgentInboxRailStore.getState().bySession["sid-a"]).toBeUndefined();
    fireEvent.click(btn);
    expect(useAgentInboxRailStore.getState().bySession["sid-a"]).toBeDefined();
    cleanup();
  });
  it('names itself: the toggle reads "Inbox" beside its glyph, folding to the glyph in a narrow tile', () => {
    const { getByTestId } = mountFor(story);
    const btn = getByTestId("terminal-grid-caption-inbox-tab-a");
    expect(btn).toHaveAccessibleName("Inbox");
    const word = Array.from(btn.querySelectorAll("span")).find((el) => el.textContent === "Inbox");
    expect(word).toBeDefined();
    // Hidden until the caption's container is wide enough to carry the word.
    expect(word!.className).toContain("hidden");
    expect(word!.className).toContain("@[18rem]/caption:inline");
    expect(getByTestId("terminal-grid-caption-tab-a").className).toContain("@container/caption");
    cleanup();
  });

  it("carries the session's unread count as a chip, and says it in its name", async () => {
    agentReadSessionsMock.mockResolvedValueOnce([
      {
        handle: "sid-a",
        displayName: "A",
        state: "idle",
        launchedBy: null,
        unread: 2,
        oldestUnreadAt: null,
      },
    ]);
    mountFor(story, { teamKey: "acme:default" });
    expect(await screen.findByTestId("terminal-grid-caption-inbox-tab-a-count")).toHaveTextContent(
      "2",
    );
    expect(screen.getByTestId("terminal-grid-caption-inbox-tab-a")).toHaveAccessibleName(
      "Inbox — 2 messages · 0 in group chat",
    );
    cleanup();
  });

  it("shows no count chip when nothing is unread", async () => {
    // The roster is one store for every inbox surface: drop the last test's read.
    useInboxSessionsStore.setState({ sessions: null });
    mountFor(story, { teamKey: "acme:default" });
    await waitFor(() => expect(agentReadSessionsMock).toHaveBeenCalled());
    await Promise.resolve();
    expect(screen.queryByTestId("terminal-grid-caption-inbox-tab-a-count")).toBeNull();
    cleanup();
  });

  it("the close × after it is faintly visible at rest, so its space never reads as empty margin", () => {
    const { getByTestId } = mountFor(story);
    const close = getByTestId("terminal-grid-caption-close-tab-a");
    expect(close.className).not.toContain("opacity-0");
    expect(close.className).toContain("opacity-35");
    cleanup();
  });
});

describe("TerminalGridCaption Diff / Editor / HUD toggles (SW-TP CAP-6)", () => {
  it("Diff click flips sourceDiffShownBySession for the tab's sessionId", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    const { getByTestId } = mountFor(story);
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBeFalsy();
    fireEvent.click(getByTestId("terminal-grid-caption-diff-tab-a"));
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBe(true);
    fireEvent.click(getByTestId("terminal-grid-caption-diff-tab-a"));
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBe(false);
    cleanup();
  });

  it("Editor click flips worktreeEditorOpenBySession and closes any open Diff on the same session", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    useViewerStore.setState({ sourceDiffShownBySession: { "sid-a": true } });
    const { getByTestId } = mountFor(story);
    fireEvent.click(getByTestId("terminal-grid-caption-editor-tab-a"));
    expect(useViewerStore.getState().worktreeEditorOpenBySession["sid-a"]).toBe(true);
    // Sibling-close (closeSiblingOverlays): Editor opens, Diff closes on same sid.
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBe(false);
    cleanup();
  });

  it("HUD click flips storyHudShown for the workspace", () => {
    const { getByTestId } = mountFor(story);
    expect(useViewerStore.getState().storyHudShown["ws-story"]).toBeFalsy();
    fireEvent.click(getByTestId("terminal-grid-caption-hud-tab-a"));
    expect(useViewerStore.getState().storyHudShown["ws-story"]).toBe(true);
    cleanup();
  });
});

describe("TerminalGridCaption capability-gated icon visibility (SW-TP CAP-6)", () => {
  it("story + git working tree renders HUD + Editor + Diff + Inbox", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    mountFor(story);
    expect(screen.getByTestId("terminal-grid-caption-hud-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-editor-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-diff-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-inbox-tab-a")).toBeTruthy();
    cleanup();
  });

  // SW-VE-12: Editor renders on every folder-rooted kind; the Diff icon
  // still gates on `diffCapable` so an `epic` / `git-sync` cwd (which lives
  // inside its own git parent — the context repo — and would show a
  // misleading enclosing-repo diff) keeps the icon off while gaining the
  // editor affordance.
  it("SW-VE-12: epic tile renders HUD + Editor + Inbox (Diff stays off — not diffCapable)", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/epic" },
      repo: null,
    });
    mountFor(epic);
    expect(screen.getByTestId("terminal-grid-caption-hud-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-editor-tab-a")).toBeTruthy();
    expect(screen.queryByTestId("terminal-grid-caption-diff-tab-a")).toBeNull();
    expect(screen.getByTestId("terminal-grid-caption-inbox-tab-a")).toBeTruthy();
    cleanup();
  });

  it("SW-VE-12: a `generic` cwd that is NOT a git working tree still renders Diff + Editor", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/gen" },
      repo: null,
    });
    mountFor(generic);
    expect(screen.getByTestId("terminal-grid-caption-diff-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-editor-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-inbox-tab-a")).toBeTruthy();
    cleanup();
  });

  it("generic cwd that IS a git working tree keeps Diff + Editor (AC-1 full)", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/gen" },
      repo: { root: "/w/gen", isRoot: true },
    });
    mountFor(generic);
    expect(screen.getByTestId("terminal-grid-caption-diff-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-editor-tab-a")).toBeTruthy();
    cleanup();
  });
});

describe("TerminalGridCaption progress-note slot (SW-TP CAP-7)", () => {
  it("renders the latest signal_progress note between the identity and the icons", () => {
    useViewerStore.setState({
      progressBySession: {
        "sid-a": {
          phase: "dev",
          note: "writing src/foo.tsx",
          updatedAtMs: 1000,
          settled: false,
          step: null,
          total: null,
          stepsSeen: 0,
        } as never,
      },
    });
    mountFor(story);
    const slot = screen.getByTestId("terminal-grid-caption-progress-tab-a");
    expect(slot.textContent).toContain("writing src/foo.tsx");
    cleanup();
  });

  it("italicizes a settled note (muted appearance)", () => {
    useViewerStore.setState({
      progressBySession: {
        "sid-a": {
          phase: "done",
          note: "Finished",
          updatedAtMs: 1000,
          settled: true,
          step: null,
          total: null,
          stepsSeen: 0,
        } as never,
      },
    });
    mountFor(story);
    const slot = screen.getByTestId("terminal-grid-caption-progress-tab-a");
    expect(slot.className).toMatch(/italic/);
    cleanup();
  });

  it("omits the slot entirely when no progress note exists", () => {
    mountFor(story);
    expect(screen.queryByTestId("terminal-grid-caption-progress-tab-a")).toBeNull();
    cleanup();
  });
});

describe("TerminalGridCaption live cwd slot (SW-TP CAP-11 / CAP-13)", () => {
  it("shows the tab name as fallback when the resolver returns null", () => {
    mountFor(story);
    expect(screen.getByText("barb")).toBeTruthy();
    cleanup();
  });

  it("keeps the absolute path off the caption face but gives it to screen readers", async () => {
    const abs = "/Users/dev/.codesense/users/1234567/orgs/acme/team-context/default/content";
    sessionPromptMock.mockResolvedValue({ cwd: abs, home: "/Users/dev", git: null });
    const { getByTestId } = mountFor(story);
    const name = getByTestId("terminal-grid-caption-focus-tab-a");
    await vi.waitFor(() => expect(name.textContent).toContain("default/content"));
    expect(name.textContent).not.toContain("/Users/");
    // The full path stays available to screen readers (no hover card, #1973).
    expect(name.getAttribute("aria-description")).toBe(abs);
    cleanup();
  });

  it("takes its height from the shared --tile-caption-height token", () => {
    const { getByTestId } = mountFor(story);
    expect(getByTestId("terminal-grid-caption-tab-a").className).toContain(
      "h-[var(--tile-caption-height)]",
    );
    cleanup();
  });

  it("invokes the prompt resolver with the tab's sessionId on mount", () => {
    mountFor(story);
    expect(sessionPromptMock).toHaveBeenCalledWith("sid-a");
    cleanup();
  });
});

describe("TerminalGridCaption prompt segment", () => {
  const GIT = {
    branch: "feat/prompt",
    detached: false,
    sha: "0123456789abcdef0123456789abcdef01234567",
    upstream: "origin/feat/prompt",
    staged: 1,
    modified: 2,
    untracked: 0,
    conflicted: 0,
    ahead: 3,
    behind: 0,
  };

  it("reads the cwd like a prompt: ~, the last two segments, the leaf emphasized", async () => {
    sessionPromptMock.mockResolvedValue({
      cwd: "/Users/dev/.codesense/users/1234567/_worker/acme/team-context/default/demo-child",
      home: "/Users/dev",
      git: null,
    });
    mountFor(story);
    const label = await screen.findByTestId("prompt-path");
    expect(label.getAttribute("aria-label")).toBe("~/…/default/demo-child");
    expect(screen.getByTestId("prompt-path-leaf").textContent).toBe("demo-child");
    expect(screen.queryByText("barb")).toBeNull();
    expect(screen.queryByTestId("terminal-grid-caption-git-tab-a")).toBeNull();
    cleanup();
  });

  it("shows the git segment inside a checkout and opens the diff from it", async () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    sessionPromptMock.mockResolvedValue({ cwd: "/w/story", home: "/Users/me", git: GIT });
    mountFor(story);
    const segment = await screen.findByTestId("terminal-grid-caption-git-tab-a");
    expect(screen.getByTestId("prompt-git-ref").textContent).toBe("feat/prompt");
    expect(screen.getByTestId("prompt-git-+").textContent).toBe("+1");
    expect(screen.getByTestId("prompt-git-~").textContent).toBe("~2");
    expect(screen.getByTestId("prompt-git-⇡").textContent).toBe("⇡3");
    expect(segment.tagName).toBe("BUTTON");
    fireEvent.click(segment);
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBe(true);
    // The same element stays mounted (the diff returns focus to it on Esc),
    // now aria-disabled.
    expect(screen.getByTestId("terminal-grid-caption-git-tab-a")).toBe(segment);
    expect(segment.getAttribute("aria-disabled")).toBe("true");
    // Only ever opens: a second click leaves it open.
    fireEvent.click(segment);
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBe(true);
    cleanup();
  });

  it("the caption menu's Open diff leaves focus with the overlay and never reaches the caption", async () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    sessionPromptMock.mockResolvedValue({ cwd: "/w/story", home: "/Users/me", git: GIT });
    const onFocusCell = vi.fn();
    const onZoomCell = vi.fn();
    mountFor(story, { onFocusCell, onZoomCell });
    await screen.findByTestId("terminal-grid-caption-git-tab-a");
    // Stands in for the diff overlay, which focuses itself (in an effect) on opening.
    const overlay = document.createElement("div");
    overlay.tabIndex = -1;
    document.body.appendChild(overlay);
    const unsub = useViewerStore.subscribe((st) => {
      if (st.sourceDiffShownBySession["sid-a"]) queueMicrotask(() => overlay.focus());
    });
    const name = screen.getByText("story");
    name.closest("button")!.focus();
    fireEvent.contextMenu(name);
    const item = await screen.findByRole("menuitem", { name: "Open diff" });
    fireEvent.click(item);
    fireEvent.doubleClick(item);
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBe(true);
    // Radix restores focus on a timer after the menu unmounts.
    await new Promise((r) => setTimeout(r, 20));
    expect(document.activeElement).toBe(overlay);
    expect(onFocusCell).not.toHaveBeenCalled();
    expect(onZoomCell).not.toHaveBeenCalled();
    unsub();
    overlay.remove();
    cleanup();
  });

  it("the menu's Open diff records what had focus before the menu as the diff's return target", async () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    sessionPromptMock.mockResolvedValue({ cwd: "/w/story", home: "/Users/me", git: GIT });
    mountFor(story);
    await screen.findByTestId("terminal-grid-caption-git-tab-a");
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    terminal.focus();
    fireEvent.contextMenu(screen.getByText("story"));
    const item = await screen.findByRole("menuitem", { name: "Open diff" });
    expect(document.activeElement).not.toBe(terminal);
    fireEvent.click(item);
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBe(true);
    expect(takeSourceDiffReturnFocus("sid-a")).toBe(terminal);
    terminal.remove();
    cleanup();
  });

  it("the menu's Open diff hands focus back to what had it when the overlay takes none", async () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    sessionPromptMock.mockResolvedValue({ cwd: "/w/story", home: "/Users/me", git: GIT });
    mountFor(story);
    await screen.findByTestId("terminal-grid-caption-git-tab-a");
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    terminal.focus();
    fireEvent.contextMenu(screen.getByText("story"));
    fireEvent.click(await screen.findByRole("menuitem", { name: "Open diff" }));
    expect(useViewerStore.getState().sourceDiffShownBySession["sid-a"]).toBe(true);
    // Nothing took focus on opening: it must not be left on the unmounted menu item.
    await waitFor(() => expect(document.activeElement).toBe(terminal));
    terminal.remove();
    cleanup();
  });

  it("SW-VE-12: the git segment opens the diff on every folder-rooted kind", async () => {
    // Every tab now carries the diff affordance — a non-git cwd just lands
    // in the overlay's "No git repo" empty state. So the segment is a
    // live button, not a read-only span, on a `generic` tab too.
    sessionPromptMock.mockResolvedValue({ cwd: "/w/gen", home: null, git: GIT });
    mountFor(generic);
    const segment = await screen.findByTestId("terminal-grid-caption-git-tab-a");
    expect(segment.tagName).toBe("BUTTON");
    cleanup();
  });

  it("puts the Machine's label before a remote tab's path, abbreviating its home", async () => {
    sessionPromptMock.mockResolvedValue({
      cwd: "/home/ubuntu/work/codesense",
      home: "/home/ubuntu",
      git: null,
    });
    render(
      <TerminalGridCaption
        tab={{ ...tab, origin: { machineId: "codesense-test-1", member: "m" } } as never}
        isActive={true}
        background="#000"
        foreground="#fff"
        onFocusCell={() => undefined}
        onZoomCell={() => undefined}
        onClose={() => undefined}
        workspace={story as never}
        teamKey="t1"
      />,
    );
    const label = await screen.findByTestId("prompt-path");
    expect(label.getAttribute("aria-label")).toBe("~/work/codesense");
    expect(screen.getByTestId("prompt-machine").textContent).toBe("codesense-test-1");
    cleanup();
  });
});

// ---------------------------------------------------------------------------
// SW-IB1-D1: focus-line underline rule, capability gating, tooltips, no scale
// ---------------------------------------------------------------------------

describe("TerminalGridCaption focus-line rule (SW-IB1-D1)", () => {
  // The underline rule: exactly ONE caption on screen may carry the 2px
  // --focus underline — the active tab in the focused tile. Every other
  // caption shows the 1px --border baseline. The 2px replaces the 1px (no
  // stacking). Driven by `tileFocusIndicator` which the mount site threads
  // only for that caption.
  it("renders a 2px solid border on the active+focused caption", () => {
    const { getByTestId } = mountFor(story, { tileFocusIndicator: "on" });
    const caption = getByTestId("terminal-grid-caption-tab-a");
    // Tailwind `border-b-2` stamps the 2px width; the color is supplied by
    // the `.cs-focus-line[data-focus-indicator="on"]` CSS rule in
    // styles/focus-indicator.css. We assert on both signals.
    expect(caption.className).toContain("border-b-2");
    expect(caption.className).not.toContain("border-b "); // no 1px baseline stacking
    expect(caption.className).toMatch(/\bcs-focus-line\b/);
    expect(caption.dataset.focusIndicator).toBe("on");
    expect(caption.dataset.focusLine).toBe("on");
    cleanup();
  });

  it("renders a 1px baseline on captions without the focus indicator", () => {
    const { getByTestId } = mountFor(story, { tileFocusIndicator: undefined });
    const caption = getByTestId("terminal-grid-caption-tab-a");
    // 1px (`border-b`) and the focus-line class without an indicator attr —
    // which the CSS paints as the neutral border tone (NOT focus-blue).
    expect(caption.className).toMatch(/\bborder-b\b/);
    expect(caption.className).not.toContain("border-b-2");
    expect(caption.className).toMatch(/\bcs-focus-line\b/);
    expect(caption.dataset.focusIndicator).toBeUndefined();
    expect(caption.dataset.focusLine).toBeUndefined();
    cleanup();
  });

  it("exactly one caption shows the 2px underline across a split grid state", () => {
    // Render four captions as a parent grid would: ONE tile focused with its
    // active tab (tab-a → tileFocusIndicator="on"), its sibling tabs and
    // every tab of unfocused tiles get no indicator.
    const { container, unmount } = render(
      <div>
        <TerminalGridCaption
          tab={{ ...tab, id: "t1", sessionId: "sid-1", name: "a" } as never}
          isActive={true}
          background="#000"
          foreground="#fff"
          onFocusCell={() => undefined}
          onZoomCell={() => undefined}
          onClose={() => undefined}
          workspace={story as never}
          teamKey="t1"
          tileFocusIndicator="on"
        />
        <TerminalGridCaption
          tab={{ ...tab, id: "t2", sessionId: "sid-2", name: "b" } as never}
          isActive={false}
          background="#000"
          foreground="#fff"
          onFocusCell={() => undefined}
          onZoomCell={() => undefined}
          onClose={() => undefined}
          workspace={story as never}
          teamKey="t1"
        />
        <TerminalGridCaption
          tab={{ ...tab, id: "t3", sessionId: "sid-3", name: "c" } as never}
          isActive={true}
          background="#000"
          foreground="#fff"
          onFocusCell={() => undefined}
          onZoomCell={() => undefined}
          onClose={() => undefined}
          workspace={story as never}
          teamKey="t1"
        />
        <TerminalGridCaption
          tab={{ ...tab, id: "t4", sessionId: "sid-4", name: "d" } as never}
          isActive={false}
          background="#000"
          foreground="#fff"
          onFocusCell={() => undefined}
          onZoomCell={() => undefined}
          onClose={() => undefined}
          workspace={story as never}
          teamKey="t1"
        />
      </div>,
    );
    // Caption root testids are exactly `terminal-grid-caption-t{1..4}`; the
    // regex filter rules out focus-button / icon-button children (which share
    // the prefix but add their own suffix after another dash).
    const captions = Array.from(
      container.querySelectorAll<HTMLElement>('[data-testid^="terminal-grid-caption-t"]'),
    ).filter((el) => /^terminal-grid-caption-t[1-4]$/.test(el.dataset.testid ?? ""));
    expect(captions).toHaveLength(4);
    const underlined = captions.filter((el) => el.className.includes("border-b-2"));
    expect(underlined).toHaveLength(1);
    expect(underlined[0].dataset.testid).toBe("terminal-grid-caption-t1");
    // The three others get the 1px baseline — no stacking anywhere.
    for (const el of captions.filter((c) => c !== underlined[0])) {
      expect(el.className).toMatch(/\bborder-b\b/);
      expect(el.className).not.toContain("border-b-2");
      // No `data-focus-indicator` → `.cs-focus-line` paints the neutral tone.
      expect(el.dataset.focusIndicator).toBeUndefined();
    }
    unmount();
  });

  it("dim focus indicator still widens the border to 2px (focus moved to app chrome, not away)", () => {
    const { getByTestId } = mountFor(story, { tileFocusIndicator: "dim" });
    const caption = getByTestId("terminal-grid-caption-tab-a");
    expect(caption.className).toContain("border-b-2");
    cleanup();
  });
});

describe("TerminalGridCaption capability gating (SW-IB1-D1 + SW-VE-12)", () => {
  // Post SW-VE-12: Editor + Diff icons are no longer probe-gated — they mount
  // on every folder-rooted kind. The HUD icon still gates on `caps.hudToggle`
  // (kind-level; story + epic only).

  it("HUD icon does NOT render when caps.hudToggle is false", () => {
    // A workspace kind without hudToggle is `generic`.
    useTabSourceRepoMock.mockReturnValue({ folder: null, repo: null });
    mountFor(generic);
    expect(screen.queryByTestId("terminal-grid-caption-hud-tab-a")).toBeNull();
    cleanup();
  });

  it("SW-VE-12: Editor + Diff icons render on a `story` tile independent of probe state", () => {
    // Story is both `sourceCapable` + `diffCapable`, so the two icons mount
    // on the kind alone — probe state no longer gates them.
    useTabSourceRepoMock.mockReturnValue({ folder: null, repo: null });
    mountFor(story);
    expect(screen.getByTestId("terminal-grid-caption-editor-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-diff-tab-a")).toBeTruthy();
    expect(screen.getByTestId("terminal-grid-caption-inbox-tab-a")).toBeTruthy();
    cleanup();
  });

  it("SW-VE-12: the Editor button carries its canonical aria-label on every kind", () => {
    useTabSourceRepoMock.mockReturnValue({ folder: null, repo: null });
    const { container } = mountFor(story);
    const editor = container.querySelector('[aria-label="Code editor"]');
    expect(editor).not.toBeNull();
    cleanup();
  });
});

describe("TerminalGridCaption two-register tooltips + canonical labels (SW-IB1-D1)", () => {
  it("every rendered icon carries its canonical aria-label from the DESIGN tooltip table", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    mountFor(story);
    // Canonical labels per EXPERIENCE § Voice and Tone § Tooltips.
    expect(screen.getByTestId("terminal-grid-caption-hud-tab-a")).toHaveAttribute(
      "aria-label",
      "Story summary",
    );
    expect(screen.getByTestId("terminal-grid-caption-editor-tab-a")).toHaveAttribute(
      "aria-label",
      "Code editor",
    );
    expect(screen.getByTestId("terminal-grid-caption-diff-tab-a")).toHaveAttribute(
      "aria-label",
      "Diff",
    );
    expect(screen.getByTestId("terminal-grid-caption-inbox-tab-a")).toHaveAttribute(
      "aria-label",
      "Inbox",
    );
    cleanup();
  });

  it("epic kind swaps the HUD label to `Epic summary` (same shortcut, same slot)", () => {
    mountFor(epic);
    expect(screen.getByTestId("terminal-grid-caption-hud-tab-a")).toHaveAttribute(
      "aria-label",
      "Epic summary",
    );
    cleanup();
  });

  it("every icon button carries aria-pressed reflecting its open/closed state", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    const { getByTestId } = mountFor(story);
    for (const kind of ["hud", "editor", "diff", "inbox"]) {
      const btn = getByTestId(`terminal-grid-caption-${kind}-tab-a`);
      expect(btn.getAttribute("aria-pressed")).toBe("false");
    }
    fireEvent.click(getByTestId("terminal-grid-caption-inbox-tab-a"));
    expect(getByTestId("terminal-grid-caption-inbox-tab-a").getAttribute("aria-pressed")).toBe(
      "true",
    );
    cleanup();
  });
});

describe("TerminalGridCaption press-is-color-not-motion (SW-IB1-D1)", () => {
  // Pressed = overlay-open: the icon paints a color-mix accent fill at 85%
  // over the toolbar bg, with --accent-foreground text (readable on the fill) —
  // NO scale transform anywhere.
  it("the pressed Inbox button paints an accent-fill background and sets no transform", () => {
    const { getByTestId } = mountFor(story);
    fireEvent.click(getByTestId("terminal-grid-caption-inbox-tab-a"));
    const btn = getByTestId("terminal-grid-caption-inbox-tab-a");
    // The pressed background is a Tailwind arbitrary utility (DESIGN §
    // toolbar-icon-pressed-bg: color-mix 85% accent). We stamp the exact
    // mix on the className so it stays assertable in jsdom.
    const className = btn.className;
    expect(className).toContain("color-mix");
    expect(className).toContain("--accent");
    expect(className).toContain("--accent-foreground");
    // No `transform: scale(...)` anywhere on the button — press is color.
    expect(btn.style.transform || "").not.toContain("scale");
    expect(btn.className).not.toContain("scale-");
    expect(getComputedStyle(btn).transform || "").not.toContain("scale");
    cleanup();
  });

  it("no scale transform appears in any caption class or style when toggled", () => {
    useTabSourceRepoMock.mockReturnValue({
      folder: { kind: "folder", cwd: "/w/story" },
      repo: { root: "/w/story", isRoot: true },
    });
    const { container, getByTestId } = mountFor(story);
    fireEvent.click(getByTestId("terminal-grid-caption-diff-tab-a"));
    // Scan every descendant for scale( — the file must stay motion-free on press.
    for (const el of container.querySelectorAll<HTMLElement>("*")) {
      expect(el.className?.toString?.() ?? "").not.toContain("scale-");
      expect(el.style.transform || "").not.toContain("scale");
    }
    cleanup();
  });
});
