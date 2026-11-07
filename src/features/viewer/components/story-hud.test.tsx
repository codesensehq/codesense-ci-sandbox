// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, onTestFinished } from "vitest";
import { render, screen, fireEvent, act } from "@/test-utils";

import { StoryHud } from "./story-hud";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import { warmScopes } from "@/shared/lib/warm-scopes";

// The expanded view reuses the real markdown viewer; stub it to a marker.
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({
  MarkdownViewer: ({ filePath, hostTeamKey }: { filePath: string; hostTeamKey?: string }) => (
    <div data-testid="mock-markdown-viewer" data-host-team={hostTeamKey}>
      {filePath}
    </div>
  ),
}));

// The compact card's story summary comes from the lazy details hook.
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({
    data: { title: null, storyText: "As a dev, I want X." },
    loading: false,
  })),
}));

import { useStoryDetails } from "@/shared/lib/pipeline-graph/use-story-details";

const realToggleSize = useViewerStore.getState().toggleStoryHudSize;
const realToggleShown = useViewerStore.getState().toggleStoryHudShown;

const storyWs: Workspace = {
  id: "w1",
  kind: "story",
  cwd: "/repo",
  status: "ready",
  storyId: "SW-EW-28",
  storyTitle: "Story-context HUD",
  storyStatus: "in-progress",
  storyFilePath: "/repo/story.md",
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(useStoryDetails).mockReturnValue({
    data: { title: null, storyText: "As a dev, I want X." },
    loading: false,
  });
  useViewerStore.setState({
    storyHudShown: {},
    storyHudExpanded: {},
    toggleStoryHudSize: realToggleSize,
    toggleStoryHudShown: realToggleShown,
  });
});

describe("StoryHud — hidden by default", () => {
  it("renders nothing when the tile isn't shown (the chrome toggle opens it)", () => {
    const { container } = render(<StoryHud workspace={storyWs} />);
    expect(container).toBeEmptyDOMElement();
  });
});

describe("StoryHud — compact card", () => {
  beforeEach(() => useViewerStore.setState({ storyHudShown: { w1: true } }));

  it("renders the story summary + an expand control (no redundant title)", () => {
    render(<StoryHud workspace={storyWs} />);
    const card = screen.getByTestId("story-hud-w1");
    expect(card).toHaveTextContent("As a dev, I want X.");
    expect(card).toHaveTextContent("Story");
    expect(card).not.toHaveTextContent("Story-context HUD"); // title lives in the chrome
    expect(screen.getByTestId("story-hud-expand-w1")).toBeInTheDocument();
  });

  it("the expand control switches the tile to the full view", () => {
    const spy = vi.fn();
    useViewerStore.setState({ toggleStoryHudSize: spy });
    render(<StoryHud workspace={storyWs} />);
    fireEvent.click(screen.getByTestId("story-hud-expand-w1"));
    expect(spy).toHaveBeenCalledWith("w1", true);
  });

  it("shows a placeholder while the summary is still loading", () => {
    vi.mocked(useStoryDetails).mockReturnValue({ data: undefined, loading: true });
    render(<StoryHud workspace={storyWs} />);
    expect(screen.getByTestId("story-hud-w1")).toHaveTextContent("Loading…");
  });

  it("falls back to the H1 title when the story has no `## Story` section", () => {
    vi.mocked(useStoryDetails).mockReturnValue({
      data: { title: "Full H1 title of the story", storyText: null },
      loading: false,
    });
    render(<StoryHud workspace={storyWs} />);
    expect(screen.getByTestId("story-hud-w1")).toHaveTextContent("Full H1 title of the story");
  });

  it("Escape on the focused tile closes the compact card", () => {
    const spy = vi.fn();
    useViewerStore.setState({ toggleStoryHudShown: spy });
    render(<StoryHud workspace={storyWs} isFocused />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(spy).toHaveBeenCalledWith("w1", false);
  });

  it("a warm team's hud does not take Escape while another team is active", () => {
    const spy = vi.fn();
    useViewerStore.setState({ toggleStoryHudShown: spy });
    act(() => {
      warmScopes.activate("o:hud-a");
      warmScopes.activate("o:hud-b");
    });
    render(<StoryHud workspace={storyWs} hostTeamKey="o:hud-a" isFocused />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(spy).not.toHaveBeenCalled();
    act(() => warmScopes.activate("o:hud-a"));
    fireEvent.keyDown(document, { key: "Escape" });
    expect(spy).toHaveBeenCalledWith("w1", false);
    act(() => {
      warmScopes.forget("o:hud-a");
      warmScopes.forget("o:hud-b");
    });
  });

  it("Escape returns focus to the shown tile's toggle, not an earlier hidden team's", () => {
    useViewerStore.setState({ toggleStoryHudShown: vi.fn() });
    const frames = document.createElement("div");
    document.body.append(frames);
    onTestFinished(() => frames.remove());
    frames.innerHTML = `<div style="display: none"><div data-pane-id="w1">
         <button data-testid="terminal-grid-caption-hud-hidden"></button></div></div>
       <div data-pane-id="w1"><button data-testid="terminal-grid-caption-hud-shown"></button></div>`;
    render(<StoryHud workspace={storyWs} isFocused />);
    const card = screen.getByTestId("story-hud-w1");
    card.tabIndex = 0;
    card.focus();
    fireEvent.keyDown(card, { key: "Escape" });
    expect(document.activeElement).toBe(
      document.querySelector('[data-testid="terminal-grid-caption-hud-shown"]'),
    );
  });

  it("Escape is ignored when focus is inside the terminal (xterm keeps its Escape)", () => {
    const spy = vi.fn();
    useViewerStore.setState({ toggleStoryHudShown: spy });
    render(
      <>
        <div className="xterm">
          <button data-testid="xt">x</button>
        </div>
        <StoryHud workspace={storyWs} isFocused />
      </>,
    );
    fireEvent.keyDown(screen.getByTestId("xt"), { key: "Escape" });
    expect(spy).not.toHaveBeenCalled();
  });

  it("an epic tile shows its summary (SW-EW-32: self-fetched via the configured headings)", () => {
    // SW-EW-32: the epic card no longer takes a parent-resolved `epicPurpose`
    // prop — it self-fetches via `useStoryDetails(epicDocPath, headings)`, the
    // same path story tiles use.
    vi.mocked(useStoryDetails).mockReturnValue({
      data: { title: null, storyText: "Multiplex terminals seamlessly." },
      loading: false,
    });
    const epicWs: Workspace = {
      id: "e1",
      kind: "epic",
      cwd: "/r",
      status: "ready",
      epicDirPath: "terminal-multiplexing",
      epicDocPath: "/r/epic.md",
    };
    useViewerStore.setState({ storyHudShown: { e1: true } });
    render(<StoryHud workspace={epicWs} />);
    const card = screen.getByTestId("story-hud-e1");
    expect(card).toHaveTextContent("Multiplex terminals seamlessly.");
    expect(card).toHaveTextContent("Epic");
  });

  it("an epic with no matching section falls back to its H1 title", () => {
    vi.mocked(useStoryDetails).mockReturnValue({
      data: { title: "Git Sync Workbench (SW-GS)", storyText: null },
      loading: false,
    });
    const epicWs: Workspace = {
      id: "e2",
      kind: "epic",
      cwd: "/r",
      status: "ready",
      epicDocPath: "/r/epic.md",
    };
    useViewerStore.setState({ storyHudShown: { e2: true } });
    render(<StoryHud workspace={epicWs} />);
    expect(screen.getByTestId("story-hud-e2")).toHaveTextContent("Git Sync Workbench (SW-GS)");
  });

  it("never mounts the markdown viewer in compact mode (lazy)", () => {
    render(<StoryHud workspace={storyWs} />);
    expect(screen.queryByTestId("mock-markdown-viewer")).toBeNull();
  });
});

describe("StoryHud — expanded full doc", () => {
  beforeEach(() =>
    useViewerStore.setState({ storyHudShown: { w1: true }, storyHudExpanded: { w1: true } }),
  );

  it("renders the full story doc via the reused markdown viewer (inset panel)", () => {
    render(<StoryHud workspace={storyWs} repoPath="repo:key" hostTeamKey="acme/ctx:child" />);
    expect(screen.getByTestId("mock-markdown-viewer")).toHaveAttribute(
      "data-host-team",
      "acme/ctx:child",
    );
    expect(screen.getByTestId("story-hud-expanded-w1")).toBeInTheDocument();
    expect(screen.getByTestId("mock-markdown-viewer")).toHaveTextContent("/repo/story.md");
    expect(screen.queryByTestId("story-hud-w1")).toBeNull(); // not the compact card
  });

  it("an epic tile expands to its README (epicDocPath)", () => {
    const epicWs: Workspace = {
      id: "e1",
      kind: "epic",
      cwd: "/r",
      status: "ready",
      epicDocPath: "/r/epic.md",
    };
    useViewerStore.setState({ storyHudShown: { e1: true }, storyHudExpanded: { e1: true } });
    render(<StoryHud workspace={epicWs} repoPath="repo:key" />);
    expect(screen.getByTestId("mock-markdown-viewer")).toHaveTextContent("/r/epic.md");
  });

  it("the minimize control collapses back to the compact card", () => {
    const spy = vi.fn();
    useViewerStore.setState({ toggleStoryHudSize: spy });
    render(<StoryHud workspace={storyWs} repoPath="repo:key" />);
    fireEvent.click(screen.getByTestId("story-hud-collapse-w1"));
    expect(spy).toHaveBeenCalledWith("w1", false);
  });

  it("Escape hides the FOCUSED tile's panel (back to the chrome icon)", () => {
    const spy = vi.fn();
    useViewerStore.setState({ toggleStoryHudShown: spy });
    render(<StoryHud workspace={storyWs} isFocused repoPath="repo:key" />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(spy).toHaveBeenCalledWith("w1", false);
  });

  it("Escape does NOT hide a non-focused tile's panel", () => {
    const spy = vi.fn();
    useViewerStore.setState({ toggleStoryHudShown: spy });
    render(<StoryHud workspace={storyWs} isFocused={false} repoPath="repo:key" />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(spy).not.toHaveBeenCalled();
  });

  it("Escape is ignored while the tile is zoomed (zoom-restore owns Escape)", () => {
    const spy = vi.fn();
    useViewerStore.setState({ toggleStoryHudShown: spy });
    render(<StoryHud workspace={storyWs} isFocused zoomActive repoPath="repo:key" />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(spy).not.toHaveBeenCalled();
  });

  it("falls back to the compact card when there's no doc to render", () => {
    const noDoc: Workspace = { ...storyWs, storyFilePath: undefined };
    render(<StoryHud workspace={noDoc} repoPath="repo:key" />);
    expect(screen.queryByTestId("mock-markdown-viewer")).toBeNull();
    expect(screen.getByTestId("story-hud-w1")).toBeInTheDocument();
  });
});

describe("StoryHud — never over its own toggle", () => {
  it("both sizes start below the tab captions, so the caption toggle stays clickable", () => {
    useViewerStore.setState({ storyHudShown: { w1: true } });
    const { rerender } = render(<StoryHud workspace={storyWs} isFocused />);
    expect(screen.getByTestId("story-hud-w1").className).toContain(
      "top-[calc(var(--tile-caption-height,32px)+8px)]",
    );
    useViewerStore.setState({ storyHudExpanded: { w1: true } });
    rerender(<StoryHud workspace={storyWs} isFocused />);
    const expanded = screen.getByTestId("story-hud-expanded-w1");
    expect(expanded.className).toContain("top-[calc(var(--tile-caption-height,32px)+8px)]");
    expect(expanded.className).not.toMatch(/\binset-3\b/);
  });

  it("Esc from inside the card hides it and puts focus back on the caption toggle", () => {
    useViewerStore.setState({ storyHudShown: { w1: true } });
    render(
      <div data-pane-id="w1">
        <div data-pane-active="">
          <button data-testid="terminal-grid-caption-hud-t1">Story summary</button>
        </div>
        <StoryHud workspace={storyWs} isFocused />
      </div>,
    );
    const expand = screen.getByTestId("story-hud-expand-w1");
    expand.focus();
    fireEvent.keyDown(expand, { key: "Escape" });
    expect(useViewerStore.getState().storyHudShown.w1).toBe(false);
    expect(document.activeElement).toBe(screen.getByTestId("terminal-grid-caption-hud-t1"));
  });

  it("a press outside the expanded peek folds it back; a press on its own toggle does not", () => {
    useViewerStore.setState({ storyHudShown: { w1: true }, storyHudExpanded: { w1: true } });
    render(
      <div>
        <button data-testid="terminal-grid-caption-hud-t1">Story summary</button>
        <div data-testid="outside">terminal</div>
        <StoryHud workspace={storyWs} isFocused />
      </div>,
    );
    fireEvent.pointerDown(screen.getByTestId("terminal-grid-caption-hud-t1"));
    expect(useViewerStore.getState().storyHudExpanded.w1).toBe(true);
    fireEvent.pointerDown(screen.getByTestId("story-hud-expanded-w1"));
    expect(useViewerStore.getState().storyHudExpanded.w1).toBe(true);
    fireEvent.pointerDown(screen.getByTestId("outside"));
    expect(useViewerStore.getState().storyHudExpanded.w1).toBe(false);
  });

  it("a hidden team's expanded hud does not fold the shown team's same-id hud on an inside press", () => {
    useViewerStore.setState({ storyHudShown: { w1: true }, storyHudExpanded: { w1: true } });
    act(() => {
      warmScopes.activate("o:hud-a");
      warmScopes.activate("o:hud-b");
    });
    render(
      <div>
        <div style={{ display: "none" }}>
          <StoryHud workspace={storyWs} hostTeamKey="o:hud-a" isFocused />
        </div>
        <StoryHud workspace={storyWs} hostTeamKey="o:hud-b" isFocused />
      </div>,
    );
    const [, shown] = screen.getAllByTestId("story-hud-expanded-w1");
    fireEvent.pointerDown(shown!);
    expect(useViewerStore.getState().storyHudExpanded.w1).toBe(true);
    act(() => {
      warmScopes.forget("o:hud-a");
      warmScopes.forget("o:hud-b");
    });
  });
});
