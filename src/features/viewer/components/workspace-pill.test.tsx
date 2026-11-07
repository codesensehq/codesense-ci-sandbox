// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { WorkspacePill } from "./workspace-pill";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import type { Workspace } from "@/shared/stores/viewer-store";
import { DEFAULT_SUMMARY_HEADINGS } from "@/shared/stores/viewer-store";
import {
  resetWorkspaceArchiveStoreForTest,
  useWorkspaceArchiveStore,
} from "@/shared/stores/workspace-archive-store";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import { useSurfaceSyncStore } from "@/features/shell/stores/surface-sync-store";
import { useContextOperationsStore } from "@/features/shell/stores/context-operations-store";
import {
  _seedStoryDetailsCacheForTests,
  _resetStoryDetailsCacheForTests,
} from "@/shared/lib/pipeline-graph/use-story-details";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import type { StatusState } from "@/tauri/commands";
import { useOrgStore } from "@/shared/stores/org-store";
import type { MachineSummary } from "@/tauri/commands";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import {
  __resetMachineAppearanceForTest,
  setMachineColor,
} from "@/features/machines/lib/machine-appearance";

afterEach(() => {
  _resetStoryDetailsCacheForTests();
  useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
  useStatusTaxonomyStore.getState().resetForTest();
  resetWorkspaceArchiveStoreForTest();
});

const story = (overrides: Partial<Workspace> = {}): Workspace => ({
  id: "30-10",
  kind: "story",
  cwd: "/wt/30-10",
  storyId: "30-10",
  storyFilePath: "/repo/stories/30-10.md",
  storyTitle: "Story 30-10 — title",
  status: "ready",
  sourceRepoSlug: "test/repo",
  ...overrides,
});

function setup(overrides: Partial<React.ComponentProps<typeof WorkspacePill>> = {}) {
  const props: React.ComponentProps<typeof WorkspacePill> = {
    workspace: story(),
    isActive: false,
    isStatusPopoverOpen: false,
    onStatusPopoverOpenChange: vi.fn(),
    onSelect: vi.fn(),
    onClose: vi.fn(),
    ...overrides,
  };
  return { ...render(<WorkspacePill {...props} />), props };
}

describe("WorkspacePill", () => {
  it("renders the story label", () => {
    setup();
    const button = screen.getByRole("button", { name: "Workspace 30-10" });
    expect(button).toBeInTheDocument();
    expect(screen.getByText("30-10")).toBeInTheDocument();
  });

  it("labels an epic (integration) workspace with its prettified dir leaf, not the raw path id (SW-EW-35)", () => {
    setup({
      workspace: {
        id: "integration:/Users/x/.codesense/users/1/orgs/o/codesense-context/_team/default/content/_bmad-output/epic-workspaces/epic.md",
        kind: "epic",
        cwd: "/Users/x/.codesense/.../epic-workspaces",
        epicDirPath: "/Users/x/.codesense/.../epic-workspaces",
        status: "ready",
      },
    });
    // Prettified leaf — never the full `integration:/…` path id.
    expect(screen.getByText("Epic Workspaces")).toBeInTheDocument();
    expect(screen.queryByText(/integration:/)).not.toBeInTheDocument();
  });

  it("shows the epic's compact CODE from its doc title's trailing parenthetical (SW-EW-35)", () => {
    const epicDocPath = "/Users/x/.codesense/.../epic-workspaces/epic.md";
    // Seed the shared story-details cache so the title resolves synchronously
    // on mount (same source as the epic HUD heading).
    _seedStoryDetailsCacheForTests(epicDocPath, DEFAULT_SUMMARY_HEADINGS, {
      title: "Sub-Epic: Epic Planning Workspace (SW-EW)",
      storyText: null,
    });
    setup({
      workspace: {
        id: `integration:${epicDocPath}`,
        kind: "epic",
        cwd: "/Users/x/.codesense/.../epic-workspaces",
        epicDirPath: "/Users/x/.codesense/.../epic-workspaces",
        epicDocPath,
        status: "ready",
      },
    });
    // Compact code, not the verbose H1 and not the dir leaf.
    expect(screen.getByText("SW-EW")).toBeInTheDocument();
    expect(screen.queryByText(/Sub-Epic/)).not.toBeInTheDocument();
    expect(screen.queryByText("Epic Workspaces")).not.toBeInTheDocument();
  });

  it("renders the repo-glyph chip with the identity color when provided", () => {
    setup({ glyph: "RP", glyphColor: "hsl(175, 55%, 55%)" });
    const chip = screen.getByTestId("workspace-tab-30-10-repo-glyph");
    expect(chip.textContent).toBe("RP");
    expect(chip.style.color).toBe("hsl(175, 55%, 55%)");
  });

  it("omits the glyph chip when no glyph is supplied", () => {
    setup();
    expect(screen.queryByTestId("workspace-tab-30-10-repo-glyph")).toBeNull();
  });

  it("renders a spinner for the 'creating' state", () => {
    setup({ workspace: story({ status: "creating" }) });
    expect(screen.queryByTestId("workspace-tab-30-10-creating")).not.toBeNull();
  });

  it("renders the error icon for the 'error' state", () => {
    setup({ workspace: story({ status: "error", statusError: "boom" }) });
    expect(screen.queryByTestId("workspace-tab-30-10-error")).not.toBeNull();
  });

  it("clicking selects; clicking close closes without selecting", () => {
    const onSelect = vi.fn();
    const onClose = vi.fn();
    setup({ onSelect, onClose });
    fireEvent.click(screen.getByRole("button", { name: "Workspace 30-10" }), { detail: 1 });
    expect(onSelect).toHaveBeenCalledWith("30-10", { keyboard: false });
    fireEvent.click(screen.getByTestId("workspace-tab-30-10-close"));
    expect(onClose).toHaveBeenCalledWith("30-10");
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it("tells the caller when Enter/Space activated it (a click with detail 0)", () => {
    const onSelect = vi.fn();
    setup({ onSelect });
    fireEvent.click(screen.getByRole("button", { name: "Workspace 30-10" }), { detail: 0 });
    expect(onSelect).toHaveBeenCalledWith("30-10", { keyboard: true });
  });

  it("root pills carry no close affordance", () => {
    setup({
      workspace: { id: "root", kind: "root", cwd: "/repo", status: "ready", sourceRepoSlug: "a/b" },
    });
    expect(screen.queryByTestId("workspace-tab-root-close")).toBeNull();
  });

  // RF-perf-22 (/ D1-A) / SW-XS-10: root is the raw source checkout, which
  // no explicit operation writes. Prove the `!isRoot` GATE (not just the chip's
  // null-render): put a failed operation on root's path and assert NO chip.
  const failedOp = (workspace: string) =>
    useContextOperationsStore.getState().apply(
      {
        workspace,
        agent: "s-1",
        operationId: "op-1",
        operationKind: "hydrate",
        phase: "failed",
        files: 0,
        bytes: 0,
        total: 1,
        settled: true,
      },
      Date.now(),
    );

  it("root pill shows no operation chip even when its path has one (D1-A gate)", () => {
    useContextOperationsStore.getState().reset();
    failedOp("/repo");
    setup({
      workspace: { id: "root", kind: "root", cwd: "/repo", status: "ready", sourceRepoSlug: "a/b" },
    });
    expect(screen.queryByTestId("operation-chip")).toBeNull();
  });

  it("a story pill DOES show the chip for the same store state (gate is root-only)", () => {
    useContextOperationsStore.getState().reset();
    failedOp("/wt/30-10");
    render(
      <WorkspacePill
        workspace={story()}
        isActive={false}
        isStatusPopoverOpen={false}
        onStatusPopoverOpenChange={vi.fn()}
        onSelect={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    expect(screen.getByTestId("operation-chip").getAttribute("data-tone")).toBe("failed");
  });

  describe("story stage", () => {
    it("draws the stage rail by taxonomy role, in ink rather than the state's colour", () => {
      const custom: StatusState[] = [
        {
          id: "todo",
          label: "Todo",
          color: "#0b0b0b",
          icon: "CircleDot",
          order: 0,
          role: "active",
        },
      ];
      useStatusTaxonomyStore.getState().setStates(custom);
      const { container } = setup({ workspace: story({ storyStatus: "todo" }) });
      const rail = container.querySelector(".cs-rail")!;
      expect(rail).toHaveAttribute("data-rail-role", "active");
      expect(rail.innerHTML).not.toContain("#0b0b0b");
    });
  });

  describe("minimized (dimmed) card status mark (PT-4)", () => {
    /** The pill's tabs: one session per status given, its mark rolled up over them. */
    const tabs = (...statuses: AgentStatus[]) => {
      const ids = statuses.map((_, i) => `p${i}`);
      ids.forEach((id, i) => projectSessionStatus(id, statuses[i]));
      return { markScope: { sessionIds: ids } };
    };

    it("does not claim a tool call when only process liveness is known", () => {
      setup({ dimmed: true, ...tabs("unknown"), liveTerminalCount: 2 });
      const mark = screen.getByTestId("workspace-tab-30-10-soft-closed-dot");
      expect(mark).toHaveAttribute("data-mark", "unknown");
      expect(mark.getAttribute("aria-label")).toBe("Activity unknown");
    });

    it("renders the hollow ready ring when no agent status is live", () => {
      setup({ dimmed: true, liveTerminalCount: 1 });
      const mark = screen.getByTestId("workspace-tab-30-10-soft-closed-dot");
      expect(mark).toHaveAttribute("data-mark", "ready");
      expect(mark.getAttribute("aria-label")).toBe("Ready");
    });

    it("shows a finished turn as Ready", () => {
      setup({ dimmed: true, ...tabs("idle"), liveTerminalCount: 1 });
      const mark = screen.getByTestId("workspace-tab-30-10-soft-closed-dot");
      expect(mark).toHaveAttribute("data-mark", "ready");
      expect(mark.getAttribute("aria-label")).toBe("Ready");
    });

    it("rolls its tabs up to the most urgent mark, and says what it stands for", () => {
      setup({ ...tabs("unknown", "paused", "unknown") });
      const mark = screen.getByTestId("workspace-tab-30-10-ai-status-dot");
      expect(mark).toHaveAttribute("data-mark", "permission");
      expect(mark.getAttribute("aria-label")).toBe("1 needs approval");
      expect(
        screen.getByRole("button", {
          name: "Workspace 30-10, 1 needs approval",
        }),
      ).toBeInTheDocument();
    });

    it("the console bar shows no mark when its scope has no agent status", () => {
      setup({ markScope: { sessionIds: ["quiet"] } });
      expect(screen.queryByTestId("workspace-tab-30-10-ai-status-dot")).toBeNull();
      expect(screen.getByRole("button", { name: "Workspace 30-10" })).toBeInTheDocument();
    });

    it.each([
      ["idle", "ready", "Ready"],
      ["unknown", "unknown", "Activity unknown"],
    ] as const)("shows the console bar's quiet %s status", (status, mark, label) => {
      setup({ ...tabs(status) });
      expect(screen.getByTestId("workspace-tab-30-10-ai-status-dot")).toHaveAttribute(
        "data-mark",
        mark,
      );
      expect(screen.getByTestId("workspace-tab-30-10-ai-status-dot")).toHaveAccessibleName(label);
    });

    it("shows a cleanly ended session on the ordinary console bar", () => {
      useAgentStateStore.setState({
        exitBySession: {
          "quiet-ended": { code: 0, signal: null, requested: false, outcome: "done" },
        },
      });
      setup({ markScope: { sessionIds: ["quiet-ended"] } });
      expect(screen.getByTestId("workspace-tab-30-10-ai-status-dot")).toHaveAttribute(
        "data-mark",
        "ended",
      );
      useAgentStateStore.setState({ exitBySession: {} });
    });

    it("renders the running-terminal count, suppressed at zero", () => {
      const { rerender, props } = setup({
        dimmed: true,
        ...tabs("unknown"),
        liveTerminalCount: 3,
      });
      const count = screen.getByTestId("workspace-tab-30-10-terminal-count");
      expect(count.textContent).toBe("3");
      expect(count.getAttribute("aria-label")).toBe("3 running terminals");

      rerender(<WorkspacePill {...props} liveTerminalCount={0} />);
      expect(screen.queryByTestId("workspace-tab-30-10-terminal-count")).toBeNull();
    });

    it("singularizes the count aria-label at one terminal", () => {
      setup({ dimmed: true, ...tabs("unknown"), liveTerminalCount: 1 });
      expect(
        screen.getByTestId("workspace-tab-30-10-terminal-count").getAttribute("aria-label"),
      ).toBe("1 running terminal");
    });

    it("renders the mark as a prominent LEADING headline (md) on the management card", () => {
      setup({ navRow: true, dimmed: true, ...tabs("idle") });
      const mark = screen.getByTestId("workspace-tab-30-10-soft-closed-dot");
      expect(mark.style.width).toBe("8px");
      const button = screen.getByRole("button", { name: "Workspace 30-10, Ready" });
      const label = screen.getByText("30-10");
      expect(mark.compareDocumentPosition(label) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(button.contains(mark)).toBe(true);
    });

    it("keeps the console bar (navRow falsy) mark small + trailing — no card leak", () => {
      setup({ dimmed: true, ...tabs("unknown") });
      const mark = screen.getByTestId("workspace-tab-30-10-soft-closed-dot");
      expect(mark.style.width).toBe("6px");
      const label = screen.getByText("30-10");
      expect(label.compareDocumentPosition(mark) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    });
  });
});

describe("WorkspacePill — the team folder's workspace and its crew", () => {
  const CONTENT = "/orgs/acme/ctx/default/content";
  const teamFolderWs: Workspace = {
    id: `generic:${CONTENT}`,
    kind: "generic",
    cwd: CONTENT,
    status: "ready",
  };
  afterEach(() => useOrgStore.setState({ workspaceRepos: {} }));
  const seedContentDir = () =>
    useOrgStore.setState({
      workspaceRepos: { "acme::default::repo": { teamContentPath: CONTENT } as never },
    });

  it("names the team's content folder by its folder, like any generic workspace", () => {
    seedContentDir();
    setup({ workspace: teamFolderWs });
    expect(screen.getByRole("button", { name: "Workspace content" })).toBeInTheDocument();
    expect(screen.queryByTestId(/crown/)).not.toBeInTheDocument();
  });

  it("shows the lead's piece and the crew's faces, described in words", () => {
    setup({
      crew: {
        leadFace: { kind: "image", ref: "builtin-funny:tie-cat", src: "data:,bear" },
        leadPiece: "manager",
        leadLabel: "Manager",
        hue: 300,
        crew: [
          {
            key: "a",
            name: "A",
            face: { kind: "image", ref: "builtin-funny:fine-dog", src: "data:,fox" },
          },
          {
            key: "b",
            name: "B",
            face: { kind: "image", ref: "builtin-funny:rubber-duck", src: "data:,owl" },
          },
        ],
        more: 2,
        count: 1,
        needCount: 0,
        mark: "question",
        summary: "5 agents, led by Login lead: 1 needs you",
      },
    });
    const crew = screen.getByTestId("workspace-tab-30-10-crew");
    expect(crew.querySelectorAll(".ag-crew-face")).toHaveLength(2);
    // The lead's face wears its piece as a badge.
    expect(crew.querySelector(".ag-crew-lead .ag-piece")).not.toBeNull();
    expect(crew).toHaveTextContent("+2");
    expect(screen.getByRole("button", { name: "Workspace 30-10" })).toHaveAccessibleDescription(
      "5 agents, led by Login lead: 1 needs you",
    );
  });

  it("shows the agent count, and the need-you count when some agents need a person", () => {
    seedContentDir();
    setup({
      crew: {
        leadFace: { kind: "image", ref: "builtin-funny:fine-dog", src: "data:,fox" },
        leadPiece: "manager",
        leadLabel: "Lead",
        hue: 200,
        crew: [],
        more: 0,
        count: 4,
        needCount: 2,
        mark: "question",
        summary: "4 agents, led by Login lead: 2 need you",
      },
    });
    expect(screen.getByTestId("workspace-tab-30-10-count")).toHaveTextContent("4");
    expect(screen.getByTestId("workspace-tab-30-10-need-you")).toHaveTextContent("2");
  });

  it("shows no need-you badge when nothing needs a person", () => {
    seedContentDir();
    setup({
      crew: {
        leadFace: { kind: "image", ref: "builtin-funny:fine-dog", src: "data:,fox" },
        leadPiece: "worker",
        leadLabel: "Worker",
        hue: 200,
        crew: [],
        more: 0,
        count: 1,
        needCount: 0,
        mark: "ready",
        summary: "1 agent, led by Login lead",
      },
    });
    expect(screen.queryByTestId("workspace-tab-30-10-need-you")).toBeNull();
  });

  it("shows a lone agent's stack on the team folder's workspace, as on any workspace", () => {
    seedContentDir();
    setup({
      workspace: teamFolderWs,
      crew: {
        leadFace: { kind: "image", ref: "builtin-funny:tie-cat", src: "data:tie-cat" },
        leadPiece: "worker",
        leadLabel: "Worker",
        hue: 180,
        crew: [],
        more: 0,
        count: 1,
        needCount: 0,
        mark: "tool",
        summary: "1 agent, led by controller",
      },
    });
    const crew = screen.getByTestId(`workspace-tab-${teamFolderWs.id}-crew`);
    expect(crew.querySelector(".ag-face")).toHaveAttribute("data-face", "builtin-funny:tie-cat");
  });
});

describe("WorkspacePill — compact (a crowded bottom bar)", () => {
  const crew = {
    leadFace: { kind: "image" as const, ref: "builtin-funny:tie-cat", src: "data:,bear" },
    leadPiece: "manager" as const,
    leadLabel: "Manager",
    hue: 300,
    crew: [],
    more: 0,
    count: 1,
    needCount: 0,
    mark: "question" as const,
    summary: "1 agent, led by Login lead: 1 needs you",
  };

  it("drops the crew stack but keeps the crew in words for assistive tech", () => {
    setup({ crew, compact: true });
    expect(screen.queryByTestId("workspace-tab-30-10-crew")).toBeNull();
    expect(screen.getByRole("button", { name: "Workspace 30-10" })).toHaveAccessibleDescription(
      "1 agent, led by Login lead: 1 needs you",
    );
  });

  it("shortens the label, names it in full on hover, and drops the hover ×", () => {
    setup({ compact: true });
    const label = screen.getByText("30-10");
    expect(label.className).toContain("max-w-[5rem]");
    expect(label).toHaveAttribute("title", "30-10");
    expect(screen.queryByTestId("workspace-tab-30-10-close")).toBeNull();
  });

  it("a full pill keeps its crew stack, its ×, and an unbounded label", () => {
    setup({ crew });
    expect(screen.getByTestId("workspace-tab-30-10-crew")).toBeInTheDocument();
    expect(screen.getByTestId("workspace-tab-30-10-close")).toBeInTheDocument();
    const label = screen.getByText("30-10");
    expect(label.className).not.toMatch(/max-w-/);
    expect(label).not.toHaveAttribute("title");
  });

  it("a full pill on the one-row bar caps a long name and names it on hover", () => {
    setup({ oneLine: true });
    const label = screen.getByText("30-10");
    expect(label.className).toContain("max-w-[12rem]");
    expect(label.className).toContain("truncate");
    expect(label).toHaveAttribute("title", "30-10");
  });
});

describe("WorkspacePill — the one-line bar pill", () => {
  const crew = (needCount: number) => ({
    leadFace: { kind: "image" as const, ref: "builtin-funny:tie-cat", src: "data:,bear" },
    leadPiece: "manager" as const,
    leadLabel: "Manager",
    hue: 300,
    crew: [
      {
        key: "a",
        name: "A",
        face: { kind: "image" as const, ref: "builtin-funny:fine-dog", src: "data:,fox" },
      },
    ],
    more: 0,
    count: 3,
    needCount,
    mark: (needCount > 0 ? "question" : "tool") as "question" | "tool",
    summary: "3 agents, led by Login lead",
  });

  it("draws the lead's face alone and the count in a tally, with the Cmd number in the end slot", () => {
    setup({
      oneLine: true,
      crew: crew(0),
      shortcutHint: <span data-tray-shortcut="">⌘2</span>,
    });
    const face = screen.getByTestId("workspace-tab-30-10-crew");
    expect(face.querySelectorAll(".ag-crew-face")).toHaveLength(0);
    expect(face.querySelector(".ag-crew-lead")).not.toBeNull();
    const tally = screen.getByTestId("workspace-tab-30-10-tally");
    expect(screen.getByTestId("workspace-tab-30-10-count")).toHaveTextContent("3");
    expect(tally).not.toHaveAttribute("data-has-needs");
    const end = document.querySelector(".cs-workspace-pill-end")!;
    expect(end).toHaveAttribute("data-has-close");
    expect(end).toHaveTextContent("⌘2");
    expect(end.querySelector('[data-testid="workspace-tab-30-10-close"]')).not.toBeNull();
  });

  it("washes the tally in the needs-you ink while an agent waits on you", () => {
    setup({ oneLine: true, crew: crew(1) });
    expect(screen.getByTestId("workspace-tab-30-10-tally")).toHaveAttribute("data-has-needs");
  });

  it("draws the needs mark for agents waiting on you even when its tabs show no state, and says how many", () => {
    // No mark scope: the tabs report nothing, yet two agents wait on you.
    setup({ oneLine: true, crew: crew(2) });
    const tally = screen.getByTestId("workspace-tab-30-10-tally");
    expect(tally.querySelector('[data-testid="workspace-tab-30-10-ai-status-dot"]')).not.toBeNull();
    expect(tally).toHaveTextContent("2 need you");
    expect(screen.getByTestId("workspace-tab-30-10-count")).toHaveTextContent("3");
    expect(screen.getByRole("button", { name: "Workspace 30-10, 2 need you" })).toBeInTheDocument();
  });

  it("a compact pill keeps the agent count and drops the face first", () => {
    setup({ oneLine: true, compact: true, crew: crew(0) });
    expect(screen.queryByTestId("workspace-tab-30-10-crew")).toBeNull();
    expect(screen.getByTestId("workspace-tab-30-10-count")).toHaveTextContent("3");
  });

  it("keeps the story's stage as the leading control", () => {
    setup({
      oneLine: true,
      crew: crew(0),
      workspace: story({ storyStatus: "todo" }),
      onStoryStatusChange: vi.fn(),
    });
    const stage = screen.getByRole("button", { name: /^Change status for 30-10:/ });
    expect(stage.closest(".cs-workspace-pill-state-slot")).not.toBeNull();
  });
});

describe("WorkspacePill — the machine a remote workspace lives on", () => {
  const machine = (state: "connected" | "unreachable"): MachineSummary => ({
    machineId: "box-1",
    kind: "ssh",
    label: "codesense-test-1 (private)",
    scope: { kind: "personal" },
    declaration: null,
    binding: null,
    canPublish: true,
    status:
      state === "connected"
        ? { state: "connected", workspaces: null }
        : ({ state: "unreachable", lastReachedAtMs: 0 } as MachineSummary["status"]),
    attention: 0,
    capabilities: { agentCallbacks: true },
  });
  const remote = story({
    location: { kind: "machine", machineId: "box-1", requestId: "r-1" },
  });

  afterEach(() => {
    useMachinesStore.setState({ byTeam: {} });
    __resetMachineAppearanceForTest();
  });

  it("wears the machine's color and a server mark instead of the 'on …' words", () => {
    useMachinesStore.setState({ byTeam: { t: [machine("connected")] } });
    setup({ workspace: remote });
    const pill = screen.getByRole("button", {
      name: "Workspace 30-10, on codesense-test-1 (private)",
    });
    expect(pill.dataset.machine).toBe("box-1");
    expect(pill.style.boxShadow).toContain("color-mix");
    expect(pill.style.backgroundImage).toContain("color-mix");
    expect(screen.getByTestId("workspace-tab-30-10-machine-mark")).toBeInTheDocument();
    expect(screen.getByTestId("workspace-tab-30-10-machine-mark").dataset.offline).toBeUndefined();
    // The hostname is in the accessible name and tooltip, not printed on the pill.
    expect(screen.queryByText(/on codesense-test-1/)).not.toBeInTheDocument();
  });

  it("a compact remote pill names its workspace in full in its one tooltip", async () => {
    useMachinesStore.setState({ byTeam: { t: [machine("connected")] } });
    setup({ workspace: remote, compact: true });
    const label = screen.getByText("30-10");
    expect(label.className).toContain("max-w-[5rem]");
    // No native title on top of the machine InfoTip: one tooltip, carrying both.
    expect(label).not.toHaveAttribute("title");
    await userEvent.hover(screen.getByTestId("workspace-tab-30-10-machine"));
    expect(await screen.findByRole("tooltip")).toHaveTextContent(
      "30-10 · lives on codesense-test-1 (private)",
    );
  });

  it("follows the color the user picked for the machine", () => {
    useMachinesStore.setState({ byTeam: { t: [machine("connected")] } });
    setMachineColor("box-1", "#123456");
    setup({ workspace: remote });
    const pill = screen.getByRole("button", { name: /on codesense-test-1/ });
    expect(pill.style.backgroundImage).toMatch(/#123456|rgb\(18, 52, 86\)/);
  });

  it("says offline in words and swaps the mark while the machine is unreachable", () => {
    useMachinesStore.setState({ byTeam: { t: [machine("unreachable")] } });
    setup({ workspace: remote });
    expect(
      screen.getByRole("button", {
        name: "Workspace 30-10, on codesense-test-1 (private) · offline",
      }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("workspace-tab-30-10-machine-mark").dataset.offline).toBe("true");
  });

  it("the active pill keeps the ring but drops the wash", () => {
    useMachinesStore.setState({ byTeam: { t: [machine("connected")] } });
    setup({ workspace: remote, isActive: true });
    const pill = screen.getByRole("button", { name: /on codesense-test-1/ });
    expect(pill.style.boxShadow).toContain("55%");
    expect(pill.style.backgroundImage).toBe("");
  });

  it("a workspace on This Mac looks as it always has", () => {
    setup();
    const pill = screen.getByRole("button", { name: "Workspace 30-10" });
    expect(pill.dataset.machine).toBeUndefined();
    expect(pill.getAttribute("style")).toBeNull();
    expect(screen.queryByTestId("workspace-tab-30-10-machine-mark")).not.toBeInTheDocument();
  });
});

// SW-WC-1 : the archive flag renders a subtle, muted marker — no color
// alarm, just "quieted" styling so the chat compose box's locked state has a
// visible companion in the workspace pill. Reads from the dedicated archive
// store so an event tick re-renders without a Tauri round-trip.
describe("WorkspacePill — archive badge (SW-WC-1)", () => {
  it("renders no archive badge when the workspace is not in the archive store", () => {
    setup();
    expect(screen.queryByTestId("workspace-tab-30-10-archived")).toBeNull();
  });

  it("renders no archive badge when the store reports null (explicitly unarchived)", () => {
    useWorkspaceArchiveStore.getState().setAll({ "30-10": null });
    setup();
    expect(screen.queryByTestId("workspace-tab-30-10-archived")).toBeNull();
  });

  it("renders the archive badge when the store reports a timestamp", () => {
    useWorkspaceArchiveStore.getState().setAll({ "30-10": 1_700_000_000_000 });
    setup();
    const badge = screen.getByTestId("workspace-tab-30-10-archived");
    expect(badge).toBeInTheDocument();
    expect(badge.getAttribute("aria-label")).toBe("Workspace archived");
  });

  it("ignores other workspaces' archive state", () => {
    useWorkspaceArchiveStore.getState().setAll({ "some-other-id": 1_700_000_000_000 });
    setup();
    expect(screen.queryByTestId("workspace-tab-30-10-archived")).toBeNull();
  });
});

describe("WorkspacePill — one-row native controls", () => {
  it("keeps actual portaled operation text outside selection, document and workspace-menu gestures", async () => {
    useContextOperationsStore.getState().reset();
    useContextOperationsStore.getState().apply(
      {
        workspace: "/wt/30-10",
        agent: "s-1",
        operationId: "portal-receiving",
        operationKind: "hydrate",
        phase: "failed",
        files: 0,
        bytes: 0,
        total: 1,
        settled: true,
        note: "Selectable operation detail",
      },
      Date.now(),
    );
    const user = userEvent.setup();
    const onOpenStoryDoc = vi.fn();
    const onContextMenu = vi.fn();
    const { props } = setup({ oneLine: true, navRow: true, onOpenStoryDoc, onContextMenu });
    await user.click(screen.getByTestId("operation-chip"));
    const detail = await screen.findByText("Selectable operation detail");
    fireEvent.click(detail, { detail: 1 });
    fireEvent.click(detail, { detail: 2 });
    fireEvent.contextMenu(detail, { clientX: 10, clientY: 20 });
    expect(props.onSelect).not.toHaveBeenCalled();
    expect(onOpenStoryDoc).not.toHaveBeenCalled();
    expect(onContextMenu).not.toHaveBeenCalled();
    await user.keyboard("{Escape}");
    fireEvent.click(screen.getByRole("button", { name: "Workspace 30-10" }), { detail: 1 });
    expect(props.onSelect).toHaveBeenCalledExactlyOnceWith("30-10", { keyboard: false });
    fireEvent.click(screen.getByRole("button", { name: "Workspace 30-10" }), { detail: 2 });
    expect(onOpenStoryDoc).toHaveBeenCalledExactlyOnceWith("/repo/stories/30-10.md");
  });

  it.each(["ready", "creating"] as const)(
    "routes relocated %s state and main context menus exactly once",
    (status) => {
      const onContextMenu = vi.fn();
      const { props, container } = setup({
        oneLine: true,
        navRow: true,
        workspace: story({ status, storyStatus: "todo" }),
        onStoryStatusChange: vi.fn(),
        onContextMenu,
      });
      const state = container.querySelector(".cs-workspace-pill-state")!;
      for (const target of [
        state.firstElementChild!,
        screen.getByRole("button", { name: "Workspace 30-10" }),
      ]) {
        onContextMenu.mockClear();
        fireEvent.contextMenu(target, { clientX: 10, clientY: 20 });
        expect(onContextMenu).toHaveBeenCalledExactlyOnceWith("30-10", 10, 20);
      }
      expect(props.onSelect).not.toHaveBeenCalled();
      expect(props.onClose).not.toHaveBeenCalled();
    },
  );

  it("keeps root state and main excluded from the workspace menu", () => {
    const onContextMenu = vi.fn();
    const { container } = setup({
      oneLine: true,
      navRow: true,
      workspace: { id: "root", kind: "root", cwd: "/repo", status: "ready", sourceRepoSlug: "a/b" },
      onContextMenu,
    });
    fireEvent.contextMenu(container.querySelector(".cs-workspace-pill-state")!);
    fireEvent.contextMenu(screen.getByRole("button", { name: "Workspace a/b" }));
    expect(onContextMenu).not.toHaveBeenCalled();
  });

  it.each(["{Enter}", " "])(
    "probes and closes from the keyboard %s without selecting or opening the document",
    async (key) => {
      const user = userEvent.setup();
      const onCloseHover = vi.fn();
      const onOpenStoryDoc = vi.fn();
      const { props, container } = setup({
        oneLine: true,
        navRow: true,
        onCloseHover,
        onOpenStoryDoc,
      });
      const close = screen.getByTestId("workspace-tab-30-10-close");
      expect(close.tagName).toBe("BUTTON");
      expect(container.querySelector("button button")).toBeNull();
      close.focus();
      expect(onCloseHover).toHaveBeenCalled();
      await user.keyboard(key);
      expect(props.onClose).toHaveBeenCalledExactlyOnceWith("30-10");
      expect(props.onSelect).not.toHaveBeenCalled();
      expect(onOpenStoryDoc).not.toHaveBeenCalled();
    },
  );

  it("opens the relocated taxonomy-named status control without selecting the workspace", async () => {
    const user = userEvent.setup();
    const { props, container } = setup({
      oneLine: true,
      navRow: true,
      workspace: story({ storyStatus: "todo" }),
      onStoryStatusChange: vi.fn(),
    });
    const stage = screen.getByRole("button", { name: /^Change status for 30-10:/ });
    expect(container.querySelector("button button")).toBeNull();
    await user.click(stage);
    expect(props.onStatusPopoverOpenChange).toHaveBeenCalledWith(true);
    expect(props.onSelect).not.toHaveBeenCalled();
    expect(props.onClose).not.toHaveBeenCalled();
  });
});

describe("WorkspacePill — its menu from the keyboard", () => {
  it("Shift+F10 or the menu key opens the pill's menu under it", () => {
    const onContextMenu = vi.fn();
    setup({ oneLine: true, onContextMenu });
    const pill = screen.getByRole("button", { name: "Workspace 30-10" });
    fireEvent.keyDown(pill, { key: "F10", shiftKey: true });
    fireEvent.keyDown(pill, { key: "ContextMenu" });
    fireEvent.keyDown(pill, { key: "F10" });
    expect(onContextMenu).toHaveBeenCalledTimes(2);
    expect(onContextMenu).toHaveBeenCalledWith("30-10", expect.any(Number), expect.any(Number));
  });
});

describe("WorkspacePill — the menu key's own contextmenu", () => {
  it("is not a second open however late it comes; a right-click after it opens again", () => {
    const onContextMenu = vi.fn();
    setup({ oneLine: true, onContextMenu });
    const pill = screen.getByRole("button", { name: "Workspace 30-10" });
    fireEvent.keyDown(pill, { key: "ContextMenu", timeStamp: 0 });
    expect(onContextMenu).toHaveBeenCalledTimes(1);
    // The platform's echo of that key, seconds later (no pointer pressed in between).
    fireEvent.contextMenu(pill, { timeStamp: 5000 });
    expect(onContextMenu).toHaveBeenCalledTimes(1);
    // A real right-click starts with a press.
    fireEvent.pointerDown(pill, { button: 2 });
    fireEvent.contextMenu(pill, { button: 2, clientX: 10, clientY: 20 });
    expect(onContextMenu).toHaveBeenCalledTimes(2);
    // A right-click straight after the key (its echo never came) is not swallowed either.
    fireEvent.keyDown(pill, { key: "ContextMenu" });
    fireEvent.pointerDown(pill, { button: 2 });
    fireEvent.contextMenu(pill, { button: 2, clientX: 10, clientY: 20 });
    expect(onContextMenu).toHaveBeenCalledTimes(4);
  });
});

describe("WorkspacePill — its menu on the tightest strip", () => {
  it("a right-click anywhere on a one-line pill opens its menu, its end slot and padding too", () => {
    const onContextMenu = vi.fn();
    const { container } = setup({
      oneLine: true,
      onContextMenu,
      shortcutHint: <span data-testid="hint">⌘1</span>,
    });
    for (const target of [
      container.querySelector(".cs-workspace-pill-one-line")!,
      container.querySelector(".cs-workspace-pill-end")!,
      screen.getByTestId("hint"),
    ]) {
      onContextMenu.mockClear();
      fireEvent.contextMenu(target, { clientX: 4, clientY: 5 });
      expect(onContextMenu).toHaveBeenCalledExactlyOnceWith("30-10", 4, 5);
    }
  });

  it("a root pill with Hidden tabs opens its menu by right-click and from the keyboard", () => {
    const onContextMenu = vi.fn();
    const { container } = setup({
      oneLine: true,
      workspace: { id: "root", kind: "root", cwd: "/repo", status: "ready", sourceRepoSlug: "a/b" },
      onContextMenu,
      hiddenTabsControl: <button type="button">1 hidden tab</button>,
    });
    fireEvent.contextMenu(container.querySelector(".cs-workspace-pill-one-line")!, {
      clientX: 4,
      clientY: 5,
    });
    expect(onContextMenu).toHaveBeenCalledExactlyOnceWith("root", 4, 5);
    fireEvent.keyDown(screen.getByRole("button", { name: /^Workspace a\/b/ }), {
      key: "ContextMenu",
    });
    expect(onContextMenu).toHaveBeenCalledTimes(2);
  });
});

describe("WorkspacePill — the menu key and the needs-you words", () => {
  it.each([
    ["the menu key", { key: "ContextMenu" }],
    ["Shift+F10", { key: "F10", shiftKey: true }],
  ])("%s opens the pill's menu once, though the platform's contextmenu follows it", (_k, key) => {
    for (const oneLine of [true, false]) {
      const onContextMenu = vi.fn();
      const view = setup({ oneLine, navRow: true, onContextMenu });
      const pill = screen.getByRole("button", { name: "Workspace 30-10" });
      const down = fireEvent.keyDown(pill, key);
      // The keydown's default (the WebView's own menu) is taken.
      expect(down).toBe(false);
      const echo = fireEvent.contextMenu(pill);
      expect(echo).toBe(false);
      expect(onContextMenu).toHaveBeenCalledTimes(1);
      view.unmount();
    }
  });

  it("a pill with no menu of its own never shows the WebView's", () => {
    const { container } = setup({
      oneLine: true,
      navRow: true,
      workspace: { id: "root", kind: "root", cwd: "/repo", status: "ready", sourceRepoSlug: "a/b" },
      onContextMenu: vi.fn(),
    });
    const pill = screen.getByRole("button", { name: "Workspace a/b" });
    expect(fireEvent.keyDown(pill, { key: "ContextMenu" })).toBe(false);
    expect(fireEvent.contextMenu(pill)).toBe(false);
    expect(fireEvent.contextMenu(container.querySelector(".cs-workspace-pill-state")!)).toBe(false);
  });

  const crew = (needCount: number) => ({
    leadFace: { kind: "image" as const, ref: "builtin-funny:tie-cat", src: "data:,bear" },
    leadPiece: "manager" as const,
    leadLabel: "Manager",
    hue: 300,
    crew: [],
    more: 0,
    count: 3,
    needCount,
    mark: "question" as const,
    summary: "3 agents",
  });

  it.each([
    [1, "1 needs you"],
    [2, "2 need you"],
  ])("%i waiting reads %s on the one-line pill and the panel's card alike", (n, words) => {
    const one = setup({ oneLine: true, crew: crew(n) });
    expect(screen.getByTestId("workspace-tab-30-10-tally")).toHaveTextContent(words);
    one.unmount();
    setup({ crew: crew(n) });
    const badge = screen.getByTestId("workspace-tab-30-10-need-you");
    expect(badge).toHaveAttribute("title", words);
    expect(badge.querySelector(".sr-only")).toHaveTextContent(words);
  });

  it("names the keys that move it in the selection button's description", () => {
    setup({ oneLine: true, description: "Alt+Left or Alt+Right moves it along the bar." });
    expect(screen.getByRole("button", { name: "Workspace 30-10" })).toHaveAccessibleDescription(
      "Alt+Left or Alt+Right moves it along the bar.",
    );
  });
});

it("labels a context worktree with its name, branch and shared crew count", () => {
  setup({
    workspace: story({
      id: "cw_one",
      kind: "context",
      contextWorktreeId: "cw_one",
      contextWorktreeName: "Sprint 1",
      contextWorktreeBranch: "riverside/sprint-1",
      storyId: undefined,
      storyFilePath: undefined,
    }),
    liveTerminalCount: 2,
  });
  expect(screen.getByText("Sprint 1")).toHaveAttribute(
    "title",
    "Sprint 1 · context worktree · branch riverside/sprint-1 · 2 agents",
  );
});

beforeEach(() => resetSessionProjection());
