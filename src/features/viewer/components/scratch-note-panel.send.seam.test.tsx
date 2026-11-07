// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, renderHook, screen, fireEvent, act, waitFor } from "@/test-utils";

import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import type { TeamAgentIdentity } from "@/features/viewer/hooks/use-team-agent-identities";
import { AGENT_STATUS_WORDS } from "@/shared/lib/agent-status-config";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import type { AgentSessionRow } from "@/tauri/commands";

import { ScratchNotePanel } from "./scratch-note-panel";

// The send seam end to end through a REAL tiptap editor: hover a task line, send it
// to a running agent, and the line now carries a chip whose status is the shared
// status pipe's (the same read the agents list makes), with the handoff saved into
// the note's markdown as `[@Name](cs:agent:handle)`.

const h = vi.hoisted(() => ({
  agents: [] as unknown[],
  sendPromptToAgent: vi.fn((..._args: unknown[]) =>
    Promise.resolve({
      status: "submitted",
      submitted: true,
      machine: null,
      code: null,
      message: null,
    } as unknown),
  ),
  toastError: vi.fn(),
  toastWarning: vi.fn(),
  setScratchNoteContent: vi.fn(),
}));

vi.mock("@/features/viewer/hooks/use-team-agent-identities", () => ({
  useTeamAgentIdentities: () => h.agents,
}));
vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToTerminal: vi.fn(),
  sendTextToSession: vi.fn(() => Promise.reject(new Error("not the agent send path"))),
}));
vi.mock("@/features/viewer/lib/show-agent", () => ({ showAgent: vi.fn() }));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: h.toastError, warning: h.toastWarning }),
}));
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  useActiveWorkspaceContext: () => ({
    teamKey: "acme:platform",
    repoPath: null,
    workspace: null,
    sourceRepoSlug: null,
  }),
}));
vi.mock("@/shared/lib/use-resolved-repo", () => ({ useResolvedRepo: () => null }));
vi.mock("@/shared/lib/pipeline-graph/use-pipeline-tree", () => ({
  usePipelineTree: () => ({ tree: null, loading: false, error: null, refetch: vi.fn() }),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ onFocusChanged: () => Promise.resolve(() => {}) }),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  notesDirPath: vi.fn(() => Promise.resolve("/notes")),
  notesHistory: vi.fn(() => Promise.reject(new Error("not mocked: notes_history"))),
  readNote: vi.fn(() => Promise.resolve({ content: "" })),
  sendPromptToAgent: h.sendPromptToAgent,
}));

const NOTE = { id: "n1", title: "Plans", pinned: false, lastOpenedAt: 1 };
const storeState: Record<string, unknown> = {
  scratchNoteContent: "- [ ] Redesign the drop down",
  notes: [NOTE],
  activeNoteId: "n1",
  noteSavedAt: null,
  notesLoadedScope: null,
  scratchNoteLoadStatus: "loaded",
  activeNoteHeadings: [],
  pendingHeadingJump: null,
  setScratchNoteContent: h.setScratchNoteContent,
  loadNotes: vi.fn(),
  setActiveNote: vi.fn(),
  createNote: vi.fn(),
  renameNote: vi.fn(),
  deleteNote: vi.fn(),
  setNotePinned: vi.fn(),
  flushNotes: vi.fn(() => Promise.resolve()),
  setActiveNoteHeadings: vi.fn(),
  clearHeadingJump: vi.fn(),
};
vi.mock("@/shared/stores/viewer-store", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/stores/viewer-store")>();
  const store = (selector?: (s: Record<string, unknown>) => unknown) =>
    selector ? selector({ ...actual.useViewerStore.getState(), ...storeState }) : {};
  store.getState = () => ({ ...actual.useViewerStore.getState(), ...storeState });
  store.setState = actual.useViewerStore.setState;
  store.subscribe = actual.useViewerStore.subscribe;
  return { ...actual, useViewerStore: store };
});

const ROW: AgentSessionRow = {
  handle: "BUG-142#dev",
  sessionId: "sess-dev",
  kind: "dashboard-launched",
  alive: true,
  status: "running",
  backgrounded: false,
  startedWith: null,
  startedAtMs: Date.now() - 60_000,
  launcher: "virtual://agents",
  agentRevision: null,
  lastOutputMs: null,
};
const JIMMY: TeamAgentIdentity = {
  row: ROW,
  depth: 0,
  label: "Jimmy",
  face: { kind: "mark", text: "J", color: null },
  role: { piece: "worker", label: "Worker" },
  agentKey: null,
};

beforeEach(() => {
  h.agents = [JIMMY];
  h.sendPromptToAgent.mockClear();
  h.toastError.mockClear();
  h.toastWarning.mockClear();
  h.setScratchNoteContent.mockClear();
});

describe("Send a line to an agent (real editor)", () => {
  it("hover send → pick the agent → the text goes to its session and the line carries its live chip", async () => {
    render(<ScratchNotePanel />);
    const item = await waitFor(() => {
      const li = document.querySelector<HTMLElement>(".scratch-note-editor li");
      if (!li?.textContent?.includes("Redesign")) throw new Error("editor not loaded");
      return li;
    });

    fireEvent.mouseMove(item.querySelector("p") ?? item);
    fireEvent.click(await screen.findByRole("button", { name: "Send to… (⌘↵)" }));

    // The picker lists the running agent with the shared status words.
    const { result } = renderHook(() => useAgentStatusOf([ROW]));
    const status = result.current(ROW);
    const option = await screen.findByRole("option", { name: /Jimmy/ });
    expect(option).toHaveTextContent(AGENT_STATUS_WORDS[status].toLowerCase());

    // Leave the note out of the message for this send.
    fireEvent.click(screen.getByRole("checkbox", { name: "Include the note as context" }));
    await act(async () => {
      fireEvent.click(option);
    });

    expect(h.sendPromptToAgent).toHaveBeenCalledWith(
      "BUG-142#dev",
      "sess-dev",
      "Redesign the drop down",
    );

    const chip = await screen.findByRole("button", { name: /Jimmy/ });
    expect(chip).toHaveClass("nt-agent-chip");
    expect(chip).toHaveAttribute("data-mark", markOfAgentStatus(status));

    // The handoff is saved into the note's markdown, readable as plain text.
    const saved = h.setScratchNoteContent.mock.calls.at(-1)?.[0] as string;
    expect(saved).toContain("- [ ] Redesign the drop down [@Jimmy](cs:agent:BUG-142%23dev)");
  });

  // An agent whose tab runs on a machine: the send goes by its handle to the machine,
  // never looked up among this Mac's sessions ("local session not found").
  const REMOTE: TeamAgentIdentity = {
    ...JIMMY,
    label: "ac4-warm",
    row: {
      ...ROW,
      handle: "remote-tab-f0529fed",
      sessionId: "remote-shell-f0529fed",
      kind: "registered",
    },
  };

  async function sendFirstLineTo(name: RegExp) {
    render(<ScratchNotePanel />);
    const item = await waitFor(() => {
      const li = document.querySelector<HTMLElement>(".scratch-note-editor li");
      if (!li?.textContent?.includes("Redesign")) throw new Error("editor not loaded");
      return li;
    });
    fireEvent.mouseMove(item.querySelector("p") ?? item);
    fireEvent.click(await screen.findByRole("button", { name: "Send to… (⌘↵)" }));
    const option = await screen.findByRole("option", { name });
    await act(async () => {
      fireEvent.click(option);
    });
  }

  it("a remote agent is sent to by its handle and gets its chip once submitted", async () => {
    h.agents = [REMOTE];
    h.sendPromptToAgent.mockResolvedValueOnce({
      status: "submitted",
      submitted: true,
      machine: "codesense-test-1",
      code: null,
      message: null,
    });
    await sendFirstLineTo(/ac4-warm/);
    expect(h.sendPromptToAgent).toHaveBeenCalledWith(
      "remote-tab-f0529fed",
      "remote-shell-f0529fed",
      expect.stringMatching(/^Redesign the drop down/),
    );
    expect(h.toastError).not.toHaveBeenCalled();
    expect(await screen.findByRole("button", { name: /ac4-warm/ })).toHaveClass("nt-agent-chip");
  });

  it("a disconnected machine is named and nothing is marked sent", async () => {
    h.agents = [REMOTE];
    h.sendPromptToAgent.mockResolvedValueOnce({
      status: "refused",
      submitted: false,
      machine: "codesense-test-1",
      code: "staleOrigin",
      message: "codesense-test-1 is disconnected",
    });
    await sendFirstLineTo(/ac4-warm/);
    expect(h.toastError).toHaveBeenCalledWith(
      "Couldn't send to ac4-warm: codesense-test-1 is disconnected. Nothing was sent.",
    );
    expect(h.sendPromptToAgent).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: /ac4-warm/ })).toBeNull();
  });

  it("an unknown remote delivery warns, is never resent, and adds no chip", async () => {
    h.agents = [REMOTE];
    h.sendPromptToAgent.mockResolvedValueOnce({
      status: "unknown",
      submitted: null,
      machine: "codesense-test-1",
      code: "deliveryUnknown",
      message: "connection lost",
    });
    await sendFirstLineTo(/ac4-warm/);
    expect(h.toastWarning).toHaveBeenCalledWith(
      "Sent to ac4-warm on codesense-test-1, but couldn't confirm it arrived. Check its terminal before sending again.",
    );
    expect(h.toastError).not.toHaveBeenCalled();
    expect(h.sendPromptToAgent).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: /ac4-warm/ })).toBeNull();
  });
});
