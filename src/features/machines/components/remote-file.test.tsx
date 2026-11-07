// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { WorktreeFileTree } from "@/features/viewer/components/worktree-file-tree";
import {
  resolveVirtualTabTitle,
  resolveVirtualTabTooltip,
} from "@/shared/lib/virtual-viewer-registry";
import { connectMachine, disconnectMachine } from "@/tauri/commands";

import {
  installMachinesBackend,
  resetMachinesUi,
  TEAM_KEY,
} from "../__test-utils__/machines-backend";
import { machineFilePath } from "../lib/machine-file-path";
import { registerMachineFileTab } from "../machine-file-tab";
import { findMachine, loadTeamMachines, useMachinesStore } from "../stores/machines-store";
import { RemoteFileTree } from "./remote-file-tree";
import { RemoteFileViewer } from "./remote-file-viewer";

vi.mock("@/shared/components/code-editor", () => ({
  CodeEditor: ({
    value,
    readOnly,
    ariaLabel,
  }: {
    value: string;
    readOnly: boolean;
    ariaLabel: string;
  }) => (
    <pre data-testid="code-editor" data-readonly={String(readOnly)} aria-label={ariaLabel}>
      {value}
    </pre>
  ),
  detectLanguageId: () => "plain",
  useEditorAffordances: () => ({ folding: false, showWhitespace: false, tabSize: 2 }),
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  listDirectory: vi.fn(() =>
    Promise.resolve([
      {
        name: "README.md",
        path: "/Users/me/repo/README.md",
        is_dir: false,
        extension: "md",
        size: 10,
      },
    ]),
  ),
}));

const ROOT = "/home/dev/src/codesense";

beforeEach(async () => {
  resetMachinesUi();
  installMachinesBackend("demo");
  await loadTeamMachines(TEAM_KEY);
});

function workspace() {
  return {
    workspaceId: "ws-auth-fix",
    name: "auth-fix",
    worktreePath: ROOT,
    branch: "feat/auth-fix",
    tabs: [],
  };
}

function Tree({ machineId }: { machineId: string }) {
  const machine = useMachinesStore(
    (s) => s.byTeam[TEAM_KEY]!.find((m) => m.machineId === machineId)!,
  );
  return (
    <RemoteFileTree
      machine={machine}
      workspace={workspace()}
      selectedPath={null}
      onOpenFile={vi.fn()}
    />
  );
}

describe("RemoteFileTree", () => {
  it("header names the machine (never truncated away), the path and the branch", async () => {
    render(<Tree machineId="gpu-box-2" />);
    const header = screen.getByTestId("remote-tree-header");
    expect(header).toHaveTextContent("gpu-box-2· ~/src/codesense · feat/auth-fix");
    expect(within(header).getByText("gpu-box-2").className).toMatch(/shrink-0/);
    // The tooltip keeps what a narrow tile truncates (the branch).
    expect(header).toHaveAttribute("title", `gpu-box-2:${ROOT} · feat/auth-fix`);
    expect(await screen.findByTestId(`worktree-row-${ROOT}/src`)).toBeInTheDocument();
    expect(await screen.findByTestId("remote-tree-updated")).toHaveTextContent("Updated just now");
  });

  it("offline: one quiet line names the machine (no per-tile actions); the loaded tree stays, dimmed", async () => {
    render(<Tree machineId="gpu-box-2" />);
    await screen.findByTestId("remote-tree-updated");
    await act(async () => {
      useMachinesStore.getState().apply(await disconnectMachine("gpu-box-2"));
    });
    const strip = screen.getByTestId("remote-tree-offline");
    expect(strip).toHaveTextContent("gpu-box-2: Not connected");
    // Recovery lives once, in the CLI area's state panel — not in every tile.
    expect(within(strip).queryAllByRole("button")).toHaveLength(0);
    const tree = screen.getByTestId("worktree-tree");
    expect(tree.parentElement?.className).toMatch(/opacity-50/);
  });

  it("⟳ and window focus re-list what's loaded; coming back online does too", async () => {
    const { calls } = installMachinesBackend("demo");
    render(<Tree machineId="gpu-box-2" />);
    await screen.findByTestId("remote-tree-updated");
    const lists = () => calls.filter((c) => c.cmd === "machine_fs_list").length;
    const before = lists();
    await userEvent.click(screen.getByRole("button", { name: "Refresh files from gpu-box-2" }));
    await waitFor(() => expect(lists()).toBeGreaterThan(before));
    const afterClick = lists();
    act(() => void window.dispatchEvent(new Event("focus")));
    await waitFor(() => expect(lists()).toBeGreaterThan(afterClick));

    await act(async () => {
      useMachinesStore.getState().apply(await disconnectMachine("gpu-box-2"));
    });
    const offline = lists();
    act(() => void window.dispatchEvent(new Event("focus")));
    expect(lists()).toBe(offline);
    await act(async () => {
      useMachinesStore.getState().apply(await connectMachine("gpu-box-2"));
    });
    await waitFor(() => expect(lists()).toBeGreaterThan(offline));
  });

  it("unreachable before anything loaded says so honestly — once", () => {
    render(<Tree machineId="devbox-01" />);
    expect(screen.getByTestId("remote-tree-nothing-loaded")).toHaveTextContent(
      "Nothing loaded from devbox-01 yet — files appear once it's back.",
    );
    // No stale tree to explain, so no second offline line.
    expect(screen.queryByTestId("remote-tree-offline")).toBeNull();
  });

  it("latency: spinner after 150 ms, then Still loading from ‹machine›… + Cancel after 2 s", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      let release: () => void = () => {};
      installMachinesBackend("demo", {
        machine_fs_list: () => new Promise((resolve) => (release = () => resolve([]))),
      });
      render(<Tree machineId="gpu-box-2" />);
      expect(screen.queryByTestId("remote-tree-loading")).toBeNull();
      await act(async () => vi.advanceTimersByTime(200));
      expect(screen.getByTestId("remote-tree-loading")).toBeInTheDocument();
      await act(async () => vi.advanceTimersByTime(2000));
      const slow = screen.getByTestId("remote-tree-slow");
      expect(slow).toHaveTextContent("Still loading from gpu-box-2…");
      await act(async () => within(slow).getByRole("button", { name: "Cancel" }).click());
      expect(screen.queryByTestId("remote-tree-slow")).toBeNull();
      // Cancel really stops: nothing re-lists until the user asks.
      const stopped = screen.getByTestId("remote-tree-stopped");
      expect(stopped).toHaveTextContent("Stopped loading from gpu-box-2.");
      release();
      await act(async () => within(stopped).getByRole("button", { name: "Load again" }).click());
      expect(screen.queryByTestId("remote-tree-stopped")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("RemoteFileViewer", () => {
  const ref = { machineId: "gpu-box-2", absPath: `${ROOT}/src/shared/server.rs` };

  it("read-only, the machine repeated in the header, Open in terminal runs $EDITOR ON the machine", async () => {
    const { calls } = installMachinesBackend("demo");
    render(<RemoteFileViewer file={ref} />);
    expect(await screen.findByTestId("code-editor")).toHaveAttribute("data-readonly", "true");
    expect(screen.getByTestId("remote-file-readonly")).toHaveTextContent("Read-only");
    expect(screen.getByTestId("remote-file-viewer")).toHaveTextContent(`gpu-box-2:${ref.absPath}`);
    await userEvent.click(screen.getByTestId("remote-file-open-in-terminal"));
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "open_machine_terminal")?.args).toEqual({
        machineId: "gpu-box-2",
        cwd: `${ROOT}/src/shared`,
        command: `\${EDITOR:-vi} ${ref.absPath}`,
      }),
    );
    expect(screen.getByRole("button", { name: /Copy path$/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy path on machine" })).toBeInTheDocument();
  });

  it("shell-escapes the path it hands the machine's shell", async () => {
    const { calls } = installMachinesBackend("demo", {
      machine_fs_read: () => ({ content: "x", size: 1, truncated: false }),
    });
    render(<RemoteFileViewer file={{ machineId: "gpu-box-2", absPath: "/home/dev/a b;rm.txt" }} />);
    await screen.findByTestId("code-editor");
    await userEvent.click(screen.getByTestId("remote-file-open-in-terminal"));
    await waitFor(() =>
      expect(calls.find((c) => c.cmd === "open_machine_terminal")?.args.command).toBe(
        "${EDITOR:-vi} /home/dev/a\\ b\\;rm.txt",
      ),
    );
  });

  it("gates a file over ~1 MB: Load first 1 MB · Open in terminal (less)", async () => {
    render(<RemoteFileViewer file={{ machineId: "gpu-box-2", absPath: `${ROOT}/server.log` }} />);
    const gate = await screen.findByTestId("remote-file-size-gate");
    expect(gate).toHaveTextContent("server.log is 2.3 MB on gpu-box-2");
    expect(within(gate).getByRole("button", { name: "Open in terminal (less)" })).toBeEnabled();
    await userEvent.click(within(gate).getByRole("button", { name: "Load first 1 MB" }));
    expect(screen.getByTestId("code-editor").textContent).toHaveLength(1024 * 1024);
  });

  it("offline keeps the loaded buffer — labeled out of date — and Open in terminal says why it can't", async () => {
    const offline = { machineId: "lab-pi", absPath: "/home/dev/src/lab/README.md" };
    const { unmount } = render(<RemoteFileViewer file={offline} />);
    await screen.findByTestId("code-editor");
    unmount();
    await act(async () => {
      useMachinesStore.getState().apply(await disconnectMachine("lab-pi"));
    });
    // A tab switch remounts the viewer: the session buffer survives it.
    render(<RemoteFileViewer file={offline} />);
    expect(await screen.findByTestId("remote-file-stale")).toHaveTextContent(
      /Last loaded .*, may be out of date/,
    );
    expect(screen.getByTestId("code-editor")).toHaveTextContent("README.md");
    const open = screen.getByTestId("remote-file-open-in-terminal");
    expect(open).toBeDisabled();
    expect(open).toHaveAttribute("title", "lab-pi isn't connected");
  });
});

describe("doc tabs — a mark always means remote", () => {
  it("a remote tab is prefixed 🖥 ‹machine› · file with a machine:/path tooltip", () => {
    registerMachineFileTab();
    const path = machineFilePath({
      machineId: "gpu-box-2",
      absPath: `${ROOT}/src/shared/server.rs`,
    });
    expect(findMachine("gpu-box-2")?.label).toBe("gpu-box-2");
    expect(resolveVirtualTabTitle(path)).toBe("🖥 gpu-box-2 · server.rs");
    expect(resolveVirtualTabTooltip(path)).toBe(`gpu-box-2:${ROOT}/src/shared/server.rs`);
  });

  it("local files carry no machine mark — no tab prefix, no tooltip, no remote header", async () => {
    registerMachineFileTab();
    expect(resolveVirtualTabTitle("/Users/me/repo/README.md")).toBeNull();
    expect(resolveVirtualTabTooltip("/Users/me/repo/README.md")).toBeNull();
    render(<WorktreeFileTree rootPath="/Users/me/repo" selectedPath={null} onSelect={vi.fn()} />);
    expect(await screen.findByTestId("worktree-row-/Users/me/repo/README.md")).toBeInTheDocument();
    expect(screen.queryByTestId("remote-tree-header")).toBeNull();
    expect(document.body.textContent).not.toMatch(/🖥/);
  });
});
