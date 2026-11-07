// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";

import { useViewerStore } from "@/shared/stores/viewer-store";
import { useConfigStore, type RepoState } from "@/shared/stores/config-store";

// ---------- Mocks ----------

// Tauri commands the section probes — return safe defaults.
// SS-9: `readInstallLockfile` retired with RepoHealthSection. SW-WT-8 removed
// the validate-worktree commands + the worktree-config probe; only Reveal's
// `revealContextRepo` remains.
vi.mock("@/tauri/commands", () => ({
  revealContextRepo: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@/shared/lib/resolve-source-repo-path", () => ({
  resolveSourceRepoPath: (repo: { repoPath: string }) => repo.repoPath,
}));

// Sonner toast — spy on error.
vi.mock("sonner", () => ({
  toast: { error: vi.fn(), info: vi.fn(), success: vi.fn() },
}));

const mockCommands = await import("@/tauri/commands");
const { toast } = await import("sonner");

import { RepoSection } from "./repo-section";

const baseRepo: RepoState = {
  repoPath: "/repos/x",
  repoName: "x",
  contextRepoPath: "/repos/x/.codesense",
  teamContentPath: "",
  hasRemote: false,
  remoteUrl: null,
  mainDoc: "",
};

function resetAll() {
  useConfigStore.setState({ repos: [baseRepo] });
  // T.6: tests pass `repo={baseRepo}` directly to RepoSection, so the
  // active-workspace derivation chain isn't exercised here — the store
  // reset just clears any cross-test leakage.
  useViewerStore.setState({});
  vi.mocked(mockCommands.revealContextRepo).mockReset();
  vi.mocked(mockCommands.revealContextRepo).mockResolvedValue(undefined);
  vi.mocked(toast.error).mockReset();
}

beforeEach(() => {
  resetAll();
});

afterEach(() => {
  vi.clearAllMocks();
});

// ---------- Section visibility ----------
//
// SW-CP-17: the ViewerRootRow + PipelineRootRow editing surfaces are retired.
// The Main doc row + Reveal action are the surviving content; visibility probes
// key on those.

describe("RepoSection — visibility", () => {
  it("renders the Repo header always (regardless of search filter)", () => {
    render(<RepoSection repo={baseRepo} query="zzznotmatching" />);
    expect(screen.getByRole("heading", { name: "Repo", level: 2 })).toBeInTheDocument();
    // Content hidden when filter doesn't match.
    expect(screen.queryByTestId("settings-repo-main-doc")).toBeNull();
  });

  it("renders content when search matches a keyword", () => {
    render(<RepoSection repo={baseRepo} query="reveal" />);
    expect(screen.getByTestId("settings-repo-main-doc")).toBeInTheDocument();
    // SW-CP-17: viewer-root + pipeline-root inputs removed from Settings UI.
    expect(screen.queryByTestId("settings-repo-viewer-root")).toBeNull();
    expect(screen.queryByTestId("settings-repo-pipeline-root")).toBeNull();
  });

  it("renders all rows when query is empty", () => {
    render(<RepoSection repo={baseRepo} query="" />);
    expect(screen.getByTestId("settings-repo-main-doc")).toBeInTheDocument();
    // SW-CP-17: viewer-root + pipeline-root inputs removed from Settings UI.
    expect(screen.queryByTestId("settings-repo-viewer-root")).toBeNull();
    expect(screen.queryByTestId("settings-repo-pipeline-root")).toBeNull();
    // SS-9: Verify/Repair/Uninstall retired. SW-WT-8: validate-worktree retired
    // too — Reveal is the sole surviving action.
    expect(screen.getByTestId("settings-repo-reveal")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-repo-validate-worktree")).toBeNull();
    expect(screen.queryByTestId("settings-repo-verify")).toBeNull();
    expect(screen.queryByTestId("settings-repo-repair")).toBeNull();
    expect(screen.queryByTestId("settings-repo-uninstall")).toBeNull();
  });

  it("renders empty state when repo is null", () => {
    render(<RepoSection repo={null} query="" />);
    expect(screen.getByRole("heading", { name: "Repo", level: 2 })).toBeInTheDocument();
    expect(screen.getByTestId("settings-repo-empty-state")).toBeInTheDocument();
    expect(screen.getByText(/No repos configured yet/)).toBeInTheDocument();
    expect(screen.queryByTestId("settings-repo-main-doc")).not.toBeInTheDocument();
  });
});

// SW-CP-17: the per-row draft+blur tests for the viewer-root input were removed
// with the row. SW-CP-18: the `viewerRoot` field + `setViewerRoot` action are
// fully retired — the file tree + scanner walk the whole team content root, and
// the watcher re-sources its dirs from the live projection dest set.

// ---------- Actions ----------
//
// SS-9: Verify/Repair/Uninstall retired with the install surface. SW-WT-8
// retired the validate-worktree lifecycle (commands + probe + listener).
// Reveal is the sole surviving action.

describe("RepoSection — Actions", () => {
  it("Reveal calls revealContextRepo (rejection swallowed)", async () => {
    vi.mocked(mockCommands.revealContextRepo).mockRejectedValue(new Error("nope"));
    render(<RepoSection repo={baseRepo} query="" />);
    await act(async () => {
      fireEvent.click(screen.getByTestId("settings-repo-reveal"));
    });
    expect(mockCommands.revealContextRepo).toHaveBeenCalledWith("/repos/x");
  });
});

// ---------- a11y smoke ----------

describe("RepoSection — accessibility", () => {
  it("path inputs have aria-label", () => {
    render(<RepoSection repo={baseRepo} query="" />);
    expect(screen.getByLabelText("Main doc")).toBeInTheDocument();
    // SW-CP-17: Viewer root + Pipeline root inputs removed from Settings UI.
  });

  it("Repo header has id='settings-section-repo' for openSettings('repo') anchor", () => {
    render(<RepoSection repo={baseRepo} query="" />);
    const header = screen.getByRole("heading", { name: "Repo", level: 2 });
    expect(header.id).toBe("settings-section-repo");
  });

  it("dark-mode smoke: mounts without errors when html.dark is set", () => {
    document.documentElement.classList.add("dark");
    try {
      expect(() => render(<RepoSection repo={baseRepo} query="" />)).not.toThrow();
      expect(screen.getByTestId("settings-repo-main-doc")).toBeInTheDocument();
    } finally {
      document.documentElement.classList.remove("dark");
    }
  });
});
