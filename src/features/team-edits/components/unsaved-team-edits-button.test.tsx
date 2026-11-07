// SPDX-License-Identifier: MIT
// SW-XS-27: the scope row's "N unsaved" counter and its list. Hidden at zero; the
// list's Publish and Discard call the existing explicit operations with the
// workspace's own target; Discard says what it does first; a workspace that can't
// run an action now says why; no tool names anywhere.
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { render } from "@/test-utils";
import type { TeamFileEditsStatus } from "@/tauri/commands";

import { useMachinesStore } from "@/features/machines/stores/machines-store";

import { useTeamEditsStore } from "../stores/team-edits-store";
import { UnsavedTeamEditsButton } from "./unsaved-team-edits-button";

const h = vi.hoisted(() => ({
  status: vi.fn(),
  publish: vi.fn(),
  discard: vi.fn(),
  forget: vi.fn(),
}));
vi.mock("@/tauri/commands", () => ({
  teamFileEditsStatus: h.status,
  forgetMissingWorkspace: h.forget,
  publishContextEdits: h.publish,
  discardTeamFileEdits: h.discard,
  readFile: vi.fn().mockResolvedValue(""),
  sourceRemoteFileRead: vi.fn(),
}));

const TEAM = "acme/api:backend";
const remote = { kind: "remote" as const, machineId: "pi", workspaceHandle: "remote-workspace-1" };

function statusWith(total: number): TeamFileEditsStatus {
  return {
    total,
    cleanWorkspaces: 1,
    workspaces:
      total === 0
        ? []
        : [
            {
              target: remote,
              name: "SW-9",
              state: "checked",
              checkedAtMs: Date.now(),
              machineOnline: true,
              counts: { changed: 1, new: 1, removed: 1, conflicts: 1, cannotPublish: 0 },
              files: [
                {
                  path: "_bmad-output/story.md",
                  kind: "changed",
                  destination: { kind: "team", name: "backend" },
                  teamPath: "/team/_bmad-output/story.md",
                  workspaceHash: "blake3:aa",
                  teamHash: "blake3:bb",
                },
                {
                  path: "_bmad-output/notes.md",
                  kind: "new",
                  destination: { kind: "team", name: "backend" },
                  teamPath: "/team/_bmad-output/notes.md",
                },
                {
                  path: "old.md",
                  kind: "removed",
                  destination: { kind: "team", name: "backend" },
                  teamPath: "/team/old.md",
                },
                {
                  path: "CLAUDE.md",
                  kind: "conflict",
                  reason:
                    "the team file changed since it was copied here (base 0123456789ab, team ba9876543210)",
                  destination: { kind: "team", name: "backend" },
                  teamPath: "/team/CLAUDE.md",
                },
              ],
            },
            {
              target: { kind: "local", codeRepoPath: "/wt/closed" },
              name: "closed",
              state: "checked",
              counts: { changed: 1, new: 0, removed: 0, conflicts: 0, cannotPublish: 0 },
              files: [
                {
                  path: "docs/a.md",
                  kind: "changed",
                  destination: { kind: "private" },
                  teamPath: "/team/docs/a.md",
                },
              ],
            },
          ],
  };
}

beforeEach(() => {
  useTeamEditsStore.setState({ byTeam: {} });
  h.publish.mockResolvedValue({ status: "published", failedCount: 0, conflicts: [] });
  h.discard.mockResolvedValue({ status: "ready", failedCount: 0 });
});
afterEach(() => vi.clearAllMocks());

describe("UnsavedTeamEditsButton", () => {
  it("is hidden when nothing is unsaved", async () => {
    h.status.mockResolvedValue(statusWith(0));
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    await waitFor(() => expect(h.status).toHaveBeenCalled());
    expect(screen.queryByTestId("unsaved-team-edits")).toBeNull();
  });

  it("counts, lists by workspace, marks conflicts, and uses plain words", async () => {
    h.status.mockResolvedValue(statusWith(5));
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    const button = await screen.findByTestId("unsaved-team-edits");
    expect(button).toHaveTextContent("5 unsaved");
    expect(button.getAttribute("aria-label")).toMatch(/^5 unsaved team edits in 2 workspaces/);
    expect(button.getAttribute("aria-label")).toMatch(/1 changed here and in the team too$/);
    expect(screen.getByTestId("unsaved-conflicts")).toHaveTextContent("1");
    fireEvent.click(button);
    const card = await screen.findByTestId("unsaved-team-edits-card");
    expect(card).toHaveTextContent("SW-9");
    expect(card).toHaveTextContent("Changed here and in the team since it was copied");
    expect(card).toHaveTextContent("goes to your private files");
    expect(card.textContent).not.toMatch(/reverse_sync|hydrate_|projection|lockfile|0123456789ab/);
    const conflictRow = screen
      .getAllByTestId("team-edit-row")
      .find((r) => r.getAttribute("data-kind") === "conflict")!;
    expect(conflictRow.textContent).not.toContain("Publish");
  });

  it("publishes one file through the existing explicit publish, then reads again", async () => {
    h.status.mockResolvedValue(statusWith(4));
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
    const row = (await screen.findAllByTestId("team-edit-row"))[0];
    await act(async () => {
      fireEvent.click(row.querySelector("button[title='Send this change to the team']")!);
    });
    expect(h.publish).toHaveBeenCalledWith(remote, ["_bmad-output/story.md"], false, {
      "_bmad-output/story.md": { workspaceHash: "blake3:aa", teamHash: "blake3:bb" },
    });
    await waitFor(() => expect(row).toHaveTextContent("Published."));
    expect(h.status).toHaveBeenCalledWith(remote);
  });

  it("publishing a file removed here sends the removal, and only for it", async () => {
    h.status.mockResolvedValue(statusWith(4));
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
    const rows = await screen.findAllByTestId("team-edit-row");
    const removed = rows.find((r) => r.getAttribute("data-kind") === "removed")!;
    await act(async () => {
      fireEvent.click(removed.querySelector("button[title='Remove it from the team too']")!);
    });
    expect(h.publish).toHaveBeenCalledWith(remote, ["old.md"], true, {});
  });

  it("never says Published when the publish skipped the file", async () => {
    h.status.mockResolvedValue(statusWith(4));
    h.publish.mockResolvedValue({
      status: "nothingToPublish",
      failedCount: 0,
      conflicts: [],
      skipped: [["_bmad-output/story.md", "tracked by the repo; publish it with git"]],
    });
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
    const row = (await screen.findAllByTestId("team-edit-row"))[0];
    await act(async () => {
      fireEvent.click(row.querySelector("button[title='Send this change to the team']")!);
    });
    await waitFor(() =>
      expect(row).toHaveTextContent("Not published: the repository tracks this file"),
    );
  });

  it("reads again when an explicit operation settles", async () => {
    const { useContextOperationsStore } =
      await import("@/features/shell/stores/context-operations-store");
    h.status.mockResolvedValue(statusWith(4));
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      render(<UnsavedTeamEditsButton teamKey={TEAM} />);
      await screen.findByTestId("unsaved-team-edits");
      const before = h.status.mock.calls.length;
      act(() => {
        useContextOperationsStore.setState({
          byId: {
            "op-9": {
              id: "op-9",
              workspace: "/wt/x",
              caller: "explicit",
              startedAt: Date.now(),
              settledAt: Date.now(),
            } as never,
          },
        });
      });
      vi.advanceTimersByTime(500);
      await waitFor(() => expect(h.status.mock.calls.length).toBeGreaterThan(before));
    } finally {
      vi.useRealTimers();
    }
  });

  it("says what Discard does before doing it, and Cancel does nothing", async () => {
    h.status.mockResolvedValue(statusWith(4));
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
    const rows = await screen.findAllByTestId("team-edit-row");
    const newRow = rows.find((r) => r.getAttribute("data-kind") === "new")!;
    const discardButton = [...newRow.querySelectorAll("button")].find(
      (b) => b.textContent === "Discard",
    )!;
    fireEvent.click(discardButton);
    expect(newRow).toHaveTextContent("Delete notes.md from this workspace?");
    // The safe choice holds focus; Escape cancels the confirm only.
    expect(document.activeElement?.textContent).toBe("Cancel");
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    expect(newRow).not.toHaveTextContent("Delete notes.md");
    expect(screen.getByTestId("unsaved-team-edits-card")).toBeInTheDocument();
    fireEvent.click(
      [...newRow.querySelectorAll("button")].find((b) => b.textContent === "Discard")!,
    );
    fireEvent.click(
      [...newRow.querySelectorAll("button")].find((b) => b.textContent === "Cancel")!,
    );
    expect(h.discard).not.toHaveBeenCalled();
    fireEvent.click(
      [...newRow.querySelectorAll("button")].find((b) => b.textContent === "Discard")!,
    );
    await act(async () => {
      fireEvent.click(
        [...newRow.querySelectorAll("button")].find((b) => b.textContent === "Discard")!,
      );
    });
    expect(h.discard).toHaveBeenCalledWith(remote, ["_bmad-output/notes.md"], {});
  });

  it("a workspace on This Mac acts as the app's own request, open terminal or not", async () => {
    h.status.mockResolvedValue(statusWith(4));
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
    const sections = await screen.findAllByTestId("team-edits-workspace");
    const closed = sections.find((s) => s.textContent?.includes("closed"))!;
    const publish = [...closed.querySelectorAll("button")].find(
      (b) => b.textContent === "Publish",
    )!;
    expect(publish).toBeEnabled();
    await act(async () => {
      fireEvent.click(publish);
    });
    expect(h.publish).toHaveBeenCalledWith(
      { kind: "local", codeRepoPath: "/wt/closed" },
      ["docs/a.md"],
      false,
      {},
    );
  });

  it("an offline machine's actions are disabled with the reason", async () => {
    const st = statusWith(4);
    st.workspaces[0] = { ...st.workspaces[0], machineOnline: false, state: "offline" };
    h.status.mockResolvedValue(st);
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
    const sections = await screen.findAllByTestId("team-edits-workspace");
    expect(sections[0]).toHaveTextContent("The machine is offline");
    const publish = [...sections[0].querySelectorAll("button")].find(
      (b) => b.textContent === "Publish",
    )!;
    expect(publish).toBeDisabled();
  });

  it("opening the list reads again", async () => {
    h.status.mockResolvedValue(statusWith(4));
    render(<UnsavedTeamEditsButton teamKey={TEAM} />);
    const button = await screen.findByTestId("unsaved-team-edits");
    const before = h.status.mock.calls.length;
    fireEvent.click(button);
    await waitFor(() => expect(h.status.mock.calls.length).toBeGreaterThan(before));
  });

  it("a local agent's turn ending reads again", async () => {
    const { useViewerStore } = await import("@/shared/stores/viewer-store");
    h.status.mockResolvedValue(statusWith(4));
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: "w1", kind: "story", cwd: "/wt/closed" } as never] },
      aiStatusBySession: { s1: "running" },
    } as never);
    const tabs = await import("@/shared/stores/viewer-store");
    const spy = vi
      .spyOn(tabs, "selectWorkspaceTabs")
      .mockReturnValue([{ sessionId: "s1" } as never]);
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      render(<UnsavedTeamEditsButton teamKey={TEAM} />);
      await screen.findByTestId("unsaved-team-edits");
      const before = h.status.mock.calls.length;
      act(() => {
        useViewerStore.setState({ aiStatusBySession: {} } as never);
      });
      vi.advanceTimersByTime(500);
      await waitFor(() => expect(h.status.mock.calls.length).toBeGreaterThan(before));
    } finally {
      vi.useRealTimers();
      spy.mockRestore();
    }
  });

  it("reads again when the window gets focus", async () => {
    h.status.mockResolvedValue(statusWith(4));
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      render(<UnsavedTeamEditsButton teamKey={TEAM} />);
      await screen.findByTestId("unsaved-team-edits");
      const before = h.status.mock.calls.length;
      vi.advanceTimersByTime(20_000);
      await act(async () => {
        window.dispatchEvent(new Event("focus"));
      });
      await waitFor(() => expect(h.status.mock.calls.length).toBeGreaterThan(before));
    } finally {
      vi.useRealTimers();
    }
  });
});

it("offers Remove only for missing folders, without Check now or file actions", async () => {
  h.status.mockResolvedValue({
    total: 0,
    cleanWorkspaces: 0,
    workspaces: [
      {
        target: remote,
        name: "gone",
        state: "missing",
        machineOnline: true,
        counts: { changed: 3, new: 0, removed: 0, conflicts: 0, cannotPublish: 0 },
        files: [],
      },
    ],
  });
  render(<UnsavedTeamEditsButton teamKey={TEAM} />);
  fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
  expect(await screen.findByText("Remove from list")).toBeInTheDocument();
  expect(screen.getByText(/folder no longer exists on pi/)).toBeInTheDocument();
  expect(screen.queryByText("Check now")).toBeNull();
  expect(screen.queryByTestId("team-edit-row")).toBeNull();
  expect(screen.queryByText("Publish")).toBeNull();
  expect(screen.queryByText("Discard")).toBeNull();
});

function missingStatus(): TeamFileEditsStatus {
  return {
    total: 0,
    cleanWorkspaces: 0,
    workspaces: [
      { target: remote, name: "gone", state: "missing", machineOnline: true, files: [] },
    ],
  };
}

it("removes a missing row, announces success, and focuses the list heading", async () => {
  h.status.mockResolvedValue(missingStatus());
  let resolve!: (value: { status: "removed" }) => void;
  h.forget.mockImplementation(
    () =>
      new Promise((done) => {
        resolve = done;
      }),
  );
  render(<UnsavedTeamEditsButton teamKey={TEAM} />);
  fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
  fireEvent.click(await screen.findByRole("button", { name: "Remove from list" }));
  expect(screen.getByText("Removing…")).toBeDisabled();
  expect(screen.getByText("Checking the folder is still gone…")).toBeInTheDocument();
  expect(h.forget).toHaveBeenCalledWith(remote);
  h.status.mockResolvedValue(statusWith(0));
  await act(async () => resolve({ status: "removed" }));
  await waitFor(() => expect(screen.queryByText("gone")).toBeNull());
  expect(screen.getByTestId("unsaved-team-edits")).toHaveTextContent("No unsaved");
  await waitFor(() =>
    expect(screen.getByRole("heading", { name: "Unsaved team edits" })).toHaveFocus(),
  );
});

it.each([
  ["checked", "restored", "Not removed: the folder is there again."],
  ["offline", "offline", "Not removed: pi went offline. Try again when it's back."],
  ["unknown", "uncertain", "Not removed: couldn't confirm the folder is gone."],
  ["missing", "updateTerminalServer", "Not removed: update the terminal server on pi first."],
] as const)("shows one refusal and the found state: %s", async (state, reason, text) => {
  h.status.mockResolvedValue(missingStatus());
  h.forget.mockResolvedValue({ status: "refused", state, reason, edits: { state, files: [] } });
  render(<UnsavedTeamEditsButton teamKey={TEAM} />);
  fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
  fireEvent.click(await screen.findByRole("button", { name: "Remove from list" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(text);
  expect(screen.queryAllByRole("alert")).toHaveLength(1);
  expect(screen.queryByText("Remove from list") !== null).toBe(state === "missing");
});

it("turns a cached missing row offline when the machine registry changes", async () => {
  h.status.mockResolvedValue(missingStatus());
  render(<UnsavedTeamEditsButton teamKey={TEAM} />);
  fireEvent.click(await screen.findByTestId("unsaved-team-edits"));
  expect(await screen.findByText("Remove from list")).toBeInTheDocument();
  const offline = missingStatus();
  offline.workspaces[0]!.state = "offline";
  offline.workspaces[0]!.machineOnline = false;
  h.status.mockResolvedValue(offline);
  act(() => useMachinesStore.setState({ byTeam: { [TEAM]: [] } }));
  expect(await screen.findByText("machine offline · not checked yet")).toBeInTheDocument();
  expect(screen.queryByText("Remove from list")).toBeNull();
  expect(screen.queryByText("Check now")).toBeNull();
});
