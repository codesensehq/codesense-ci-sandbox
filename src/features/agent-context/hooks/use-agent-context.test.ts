// SPDX-License-Identifier: MIT
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentContextEditReply, AgentContextView, AgentRow } from "@/tauri/commands";

import type { AgentContextSelection } from "../lib/agent-context-model";
import { useAgentContext } from "./use-agent-context";

const agentContextPreview = vi.fn<(key: string, repo?: string) => Promise<AgentContextView>>();
const agentContextUpdate =
  vi.fn<(key: string, base: number, patch: unknown) => Promise<AgentContextEditReply>>();
vi.mock("@/tauri/commands", () => ({
  agentContextPreview: (key: string, repo?: string) => agentContextPreview(key, repo),
  agentContextUpdate: (key: string, base: number, patch: unknown) =>
    agentContextUpdate(key, base, patch),
}));

const { toast } = vi.hoisted(() => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));
vi.mock("sonner", () => ({ toast }));

type Listener = (e: { payload: unknown }) => void;
const listeners: Record<string, Listener[]> = {};
const agentsListeners = {
  get length() {
    return (listeners["agents:changed"] ?? []).length;
  },
};
function emit(event: string, payload: unknown) {
  for (const cb of listeners[event] ?? []) cb({ payload });
}
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, cb: Listener) => {
    (listeners[event] ??= []).push(cb);
    return () => {};
  }),
}));

const agentsState: { rows: AgentRow[] | null } = { rows: null };
const watchAgentList = vi.fn(() => () => {});
vi.mock("@/features/agents/stores/agents-store", () => ({
  useAgentsStore: (sel: (s: typeof agentsState) => unknown) => sel(agentsState),
  watchAgentList: () => watchAgentList(),
}));

vi.mock("@/shared/lib/pipeline-graph/use-source-repo-selection", () => ({
  useTeamSourceRepoSlugs: () => [{ owner: "alderco", repo: "codesense" }],
}));

const viewer = {
  selectedAgentContext: {} as Record<string, AgentContextSelection>,
  setSelectedAgentContext: vi.fn((teamKey: string, next: AgentContextSelection | null) => {
    if (next) viewer.selectedAgentContext = { ...viewer.selectedAgentContext, [teamKey]: next };
    else delete viewer.selectedAgentContext[teamKey];
  }),
};
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: (sel: (s: typeof viewer) => unknown) => sel(viewer),
}));

const TEAM_KEY = "alderco:default";
const BASIL_KEY = "alderco/default/basil";
const CODESENSE = "github.com/alderco/codesense";

function row(key: string, name: string, over: Partial<AgentRow> = {}): AgentRow {
  return {
    key,
    name,
    scope: "team",
    org: "alderco",
    team: "default",
    archived: false,
    avatar: null,
    ...over,
  } as AgentRow;
}

function view(revision: number, over: Partial<AgentContextView> = {}): AgentContextView {
  return {
    key: BASIL_KEY,
    name: "Basil",
    revision,
    context: null,
    runtime_default: [{ path: "CLAUDE.md" }, { path: ".claude/skills" }],
    startup: ["CLAUDE.md"],
    repo_overrides: [],
    repo: CODESENSE,
    defaulted: true,
    entries: [{ path: ".claude/skills", source: "default", files: 1, bytes: 10 }],
    roots: [{ tier: { kind: "team", org: "alderco", team: "default" }, path: "/c" }],
    files: [
      {
        destination: ".claude/skills/a/SKILL.md",
        logical: ".claude/skills/a/SKILL.md",
        bytes: 10,
        root: 0,
      },
    ],
    truncated: false,
    totals: { files: 1214, bytes: 19_084_000 },
    warnings: [],
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  for (const k of Object.keys(listeners)) delete listeners[k];
  agentsState.rows = [
    row(BASIL_KEY, "Basil"),
    row("alderco/default/buck", "Buck"),
    row("alderco/default/old", "Old", { archived: true }),
    row("alderco/other/zed", "Zed", { team: "other" }),
    row("me/basil-copy", "Mine", { scope: "personal" }),
  ];
  viewer.selectedAgentContext = {
    [TEAM_KEY]: { agentKey: BASIL_KEY, repoSlug: "alderco/codesense" },
  };
  agentContextPreview.mockResolvedValue(view(4));
});

describe("useAgentContext (SW-XS-15)", () => {
  it("offers the team's live agents only, and resolves the picked one's want set in the repo", async () => {
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    expect(result.current.agents.map((a) => a.name)).toEqual(["Basil", "Buck"]);
    expect(result.current.agent?.name).toBe("Basil");
    expect(result.current.repo).toEqual({ owner: "alderco", repo: "codesense" });
    await waitFor(() => expect(result.current.view?.revision).toBe(4));
    expect(agentContextPreview).toHaveBeenCalledWith(BASIL_KEY, CODESENSE);
    expect(result.current.membership?.(".claude/skills")).toEqual({ state: "in", tier: "team" });
    expect(result.current.membership?.("_bmad-output")).toEqual({ state: "out" });
  });

  it("an agent that is gone reads as Team (everything), with no write", () => {
    viewer.selectedAgentContext = { [TEAM_KEY]: { agentKey: "alderco/default/gone" } };
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    expect(result.current.agent).toBeNull();
    expect(result.current.view).toBeNull();
    expect(agentContextPreview).not.toHaveBeenCalled();
    expect(viewer.setSelectedAgentContext).not.toHaveBeenCalled();
  });

  it("AC-2: 'Add to Basil's context for codesense only' is one server-side op against the revision, optimistic, and the reply is the one re-resolve", async () => {
    let finish: (reply: AgentContextEditReply) => void = () => {};
    agentContextUpdate.mockImplementation(() => new Promise((r) => (finish = r)));
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    expect(result.current.membership?.("docs/architecture")).toEqual({ state: "out" });

    act(() => result.current.edit("docs/architecture", "add", true));
    await waitFor(() =>
      expect(agentContextUpdate).toHaveBeenCalledWith(BASIL_KEY, 4, {
        op: "add",
        path: "docs/architecture",
        repo: CODESENSE,
        viewRepo: CODESENSE,
      }),
    );
    // Optimistic: in the set before the write lands.
    expect(result.current.membership?.("docs/architecture/adr.md")).toEqual({ state: "in" });

    const after = view(5, {
      files: [
        ...view(4).files,
        {
          destination: "docs/architecture/adr.md",
          logical: "docs/architecture/adr.md",
          bytes: 3,
          root: 0,
        },
      ],
    });
    await act(async () => {
      finish({ outcome: "saved", key: BASIL_KEY, revision: 5, view: after });
      // The save's own `agents:changed` echo (revision 5, already shown) needs no re-read.
      emit("agents:changed", { key: BASIL_KEY, revision: 5 });
    });
    await waitFor(() => expect(result.current.view?.revision).toBe(5));
    expect(agentContextPreview).toHaveBeenCalledTimes(1);
    expect(result.current.membership?.("docs/architecture/adr.md")).toEqual({
      state: "in",
      tier: "team",
    });
  });

  it("two quick edits run in turn, the second against the first's saved revision (no self-conflict)", async () => {
    agentContextUpdate
      .mockResolvedValueOnce({ outcome: "saved", key: BASIL_KEY, revision: 5, view: view(5) })
      .mockResolvedValueOnce({ outcome: "saved", key: BASIL_KEY, revision: 6, view: view(6) });
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    act(() => {
      result.current.edit("docs", "add", false);
      result.current.edit("notes", "add", false);
    });
    await waitFor(() => expect(agentContextUpdate).toHaveBeenCalledTimes(2));
    expect(agentContextUpdate.mock.calls.map((c) => c[1])).toEqual([4, 5]);
    await waitFor(() => expect(result.current.view?.revision).toBe(6));
  });

  it("AC-3: a teammate's concurrent edit is a conflict toast offering Reload, and the optimistic change is undone", async () => {
    agentContextUpdate.mockResolvedValue({ outcome: "conflict", current_revision: 5 });
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());

    await act(async () => result.current.edit("docs", "add", false));
    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    const [title, opts] = toast.error.mock.calls[0] as [
      string,
      { action: { label: string; onClick: () => void } },
    ];
    expect(title).toBe("Basil's context changed since it was loaded");
    expect(opts.action.label).toBe("Reload");
    expect(result.current.membership?.("docs")).toEqual({ state: "out" });

    agentContextPreview.mockResolvedValue(view(5));
    act(() => opts.action.onClick());
    await waitFor(() => expect(result.current.view?.revision).toBe(5));
    expect(agentContextPreview).toHaveBeenCalledTimes(2);
  });

  it("an edit that cannot be made here says why and writes nothing", async () => {
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    act(() => result.current.edit("CLAUDE.md", "remove", true));
    expect(agentContextUpdate).not.toHaveBeenCalled();
    expect(toast).toHaveBeenCalledWith(expect.stringMatching(/Basil starts with CLAUDE\.md/));
  });

  it("re-resolves when the agent changes elsewhere, not when another agent does", async () => {
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await waitFor(() => expect(agentsListeners.length).toBeGreaterThan(0));
    act(() => emit("agents:changed", { key: "alderco/default/buck" }));
    expect(agentContextPreview).toHaveBeenCalledTimes(1);
    agentContextPreview.mockResolvedValue(view(9));
    act(() => emit("agents:changed", { key: BASIL_KEY }));
    await waitFor(() => expect(result.current.view?.revision).toBe(9));
  });

  it("re-resolves on a file change under its want set only", async () => {
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await waitFor(() => expect(agentsListeners.length).toBeGreaterThan(0));
    act(() => emit("context-file-change", { repo_path: "/c", file_path: "/c/_bmad-output/x.md" }));
    await new Promise((r) => setTimeout(r, 400));
    expect(agentContextPreview).toHaveBeenCalledTimes(1);
    agentContextPreview.mockResolvedValue(view(7));
    act(() =>
      emit("context-file-change", { repo_path: "/c", file_path: "/c/.claude/skills/a/new.md" }),
    );
    await waitFor(() => expect(result.current.view?.revision).toBe(7));
  });
  it("re-resolves admitted ancestor files without exposing their directory as a root", async () => {
    const inherited = view(4, {
      files: [
        {
          destination: "shared.md",
          logical: "shared.md",
          bytes: 2,
          root: null,
          source: "/parent/content/shared.md",
          tier: { kind: "team", org: "alderco", team: "parent" },
        },
      ],
    });
    agentContextPreview.mockResolvedValue(inherited);
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    agentContextPreview.mockResolvedValue(view(7));
    act(() => emit("context-file-change", { file_path: "/parent/content/shared.md" }));
    await waitFor(() => expect(result.current.view?.revision).toBe(7));
    agentContextPreview.mockResolvedValue(view(8));
    act(() =>
      emit("context-file-change", {
        file_path: "/parent/content/new-shared.md",
        inherited_by: "/c",
      }),
    );
    await waitFor(() => expect(result.current.view?.revision).toBe(8));
  });
  it("N1: a team-wide edit made while a repo is shown asks for, and shows, that repo's view", async () => {
    agentContextUpdate.mockResolvedValueOnce({
      outcome: "saved",
      key: BASIL_KEY,
      revision: 5,
      view: view(5, { repo: "GitHub.com/alderco/codesense" }),
    });
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    act(() => result.current.edit("docs", "add", false));
    await waitFor(() =>
      expect(agentContextUpdate).toHaveBeenCalledWith(BASIL_KEY, 4, {
        op: "add",
        path: "docs",
        repo: undefined,
        viewRepo: CODESENSE,
      }),
    );
    // Repos compare case-insensitively: shown straight from the reply, no second read.
    await waitFor(() => expect(result.current.view?.revision).toBe(5));
    expect(agentContextPreview).toHaveBeenCalledTimes(1);
  });

  it("N1 fallback: a reply for another repo than the one shown is re-read", async () => {
    agentContextUpdate.mockResolvedValueOnce({
      outcome: "saved",
      key: BASIL_KEY,
      revision: 5,
      view: view(5, { repo: null }),
    });
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    agentContextPreview.mockResolvedValue(view(5));
    act(() => result.current.edit("docs", "add", false));
    await waitFor(() => expect(agentContextPreview).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(result.current.view?.repo).toBe(CODESENSE));
  });

  it("an agents:changed for a newer revision than shown is re-read; one already shown is not", async () => {
    const { result } = renderHook(() => useAgentContext(TEAM_KEY));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await waitFor(() => expect(agentsListeners.length).toBeGreaterThan(0));
    act(() => emit("agents:changed", { key: BASIL_KEY, revision: 4 }));
    expect(agentContextPreview).toHaveBeenCalledTimes(1);
    agentContextPreview.mockResolvedValue(view(6));
    act(() => emit("agents:changed", { key: BASIL_KEY, revision: 6 }));
    await waitFor(() => expect(result.current.view?.revision).toBe(6));
  });
});
