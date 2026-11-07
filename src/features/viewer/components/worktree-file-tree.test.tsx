// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import type { FileEntry } from "@/tauri/commands";

const listDirectoryMock = vi.fn<(path: string) => Promise<FileEntry[]>>();
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  listDirectory: (p: string) => listDirectoryMock(p),
}));

import { WorktreeFileTree } from "./worktree-file-tree";

function entry(name: string, path: string, opts: Partial<FileEntry> = {}): FileEntry {
  return { name, path, is_dir: false, extension: name.split(".").pop() ?? null, size: 0, ...opts };
}

const ROOT = "/wt";

beforeEach(() => {
  listDirectoryMock.mockReset();
});

describe("WorktreeFileTree (SLW-WDE-1 AC-2)", () => {
  it("lists the worktree root, keeping files the pipeline filter would drop (§A)", async () => {
    listDirectoryMock.mockResolvedValue([
      entry("src", `${ROOT}/src`, { is_dir: true, extension: null, size: null }),
      // a lockfile + a rust file: NOT viewer files — the dashboard nav hides
      // these; the worktree tree must show them (§A).
      entry("Cargo.lock", `${ROOT}/Cargo.lock`, { size: 12 }),
      entry("main.rs", `${ROOT}/main.rs`, { size: 40 }),
    ]);

    render(<WorktreeFileTree rootPath={ROOT} selectedPath={null} onSelect={vi.fn()} />);

    expect(await screen.findByText("Cargo.lock")).toBeInTheDocument();
    expect(screen.getByText("main.rs")).toBeInTheDocument();
    expect(screen.getByText("src")).toBeInTheDocument();
    expect(listDirectoryMock).toHaveBeenCalledWith(ROOT);
  });

  it("lazily lists a directory's children when expanded", async () => {
    listDirectoryMock.mockImplementation((p) => {
      if (p === ROOT)
        return Promise.resolve([entry("src", `${ROOT}/src`, { is_dir: true, size: null })]);
      if (p === `${ROOT}/src`) return Promise.resolve([entry("app.ts", `${ROOT}/src/app.ts`)]);
      return Promise.resolve([]);
    });

    render(<WorktreeFileTree rootPath={ROOT} selectedPath={null} onSelect={vi.fn()} />);

    // child not fetched until the dir is expanded (lazy)
    const dir = await screen.findByText("src");
    expect(listDirectoryMock).not.toHaveBeenCalledWith(`${ROOT}/src`);
    fireEvent.click(dir);
    expect(await screen.findByText("app.ts")).toBeInTheDocument();
    expect(listDirectoryMock).toHaveBeenCalledWith(`${ROOT}/src`);
  });

  it("fires onSelect with the file's path + size when a file row is clicked (§B)", async () => {
    const onSelect = vi.fn();
    listDirectoryMock.mockResolvedValue([entry("main.rs", `${ROOT}/main.rs`, { size: 40 })]);

    render(<WorktreeFileTree rootPath={ROOT} selectedPath={null} onSelect={onSelect} />);
    fireEvent.click(await screen.findByText("main.rs"));
    expect(onSelect).toHaveBeenCalledWith({ path: `${ROOT}/main.rs`, size: 40 });
  });

  it("does NOT fire onSelect when a directory row is clicked (it toggles instead)", async () => {
    const onSelect = vi.fn();
    listDirectoryMock.mockImplementation((p) =>
      p === ROOT
        ? Promise.resolve([entry("src", `${ROOT}/src`, { is_dir: true, size: null })])
        : Promise.resolve([]),
    );
    render(<WorktreeFileTree rootPath={ROOT} selectedPath={null} onSelect={onSelect} />);
    fireEvent.click(await screen.findByText("src"));
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("de-emphasizes heavy/noise dirs (AC-2) without hiding them", async () => {
    listDirectoryMock.mockResolvedValue([
      entry("node_modules", `${ROOT}/node_modules`, { is_dir: true, extension: null, size: null }),
      entry("src", `${ROOT}/src`, { is_dir: true, extension: null, size: null }),
    ]);
    render(<WorktreeFileTree rootPath={ROOT} selectedPath={null} onSelect={vi.fn()} />);
    const noise = await screen.findByTestId(`worktree-row-${ROOT}/node_modules`);
    const normal = screen.getByTestId(`worktree-row-${ROOT}/src`);
    expect(noise.className).toContain("opacity-60");
    expect(normal.className).not.toContain("opacity-60");
  });

  it("marks the selected file row aria-selected", async () => {
    listDirectoryMock.mockResolvedValue([entry("main.rs", `${ROOT}/main.rs`, { size: 40 })]);
    render(
      <WorktreeFileTree rootPath={ROOT} selectedPath={`${ROOT}/main.rs`} onSelect={vi.fn()} />,
    );
    const row = await screen.findByTestId(`worktree-row-${ROOT}/main.rs`);
    expect(row).toHaveAttribute("aria-selected", "true");
  });

  it("AC-5: a root listing failure shows an inline error + Retry that re-fetches", async () => {
    listDirectoryMock.mockRejectedValueOnce(new Error("boom"));
    render(<WorktreeFileTree rootPath={ROOT} selectedPath={null} onSelect={vi.fn()} />);
    const err = await screen.findByTestId("worktree-tree-error");
    expect(err).toBeInTheDocument();

    // Retry re-fetches — succeed this time.
    listDirectoryMock.mockResolvedValueOnce([entry("main.rs", `${ROOT}/main.rs`, { size: 40 })]);
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("main.rs")).toBeInTheDocument();
  });

  it("SLW-WDE-6 AC-4: a blank cwd shows a not-ready hint and never fetches", () => {
    render(<WorktreeFileTree rootPath="" selectedPath={null} onSelect={vi.fn()} />);

    // Calm not-ready state — NOT the "Couldn't load the worktree." error/Retry.
    expect(screen.getByTestId("worktree-tree-not-ready")).toBeInTheDocument();
    expect(screen.queryByTestId("worktree-tree-error")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    // Never fires `list_directory("")` (which the path sandbox would reject).
    expect(listDirectoryMock).not.toHaveBeenCalled();
  });
});

describe("WorktreeFileTree — reveal the open file", () => {
  it("expands every ancestor folder of `reveal` and scrolls its row into view", async () => {
    listDirectoryMock.mockImplementation((p) => {
      if (p === ROOT)
        return Promise.resolve([entry("src", `${ROOT}/src`, { is_dir: true, size: null })]);
      if (p === `${ROOT}/src`)
        return Promise.resolve([entry("auth", `${ROOT}/src/auth`, { is_dir: true, size: null })]);
      if (p === `${ROOT}/src/auth`)
        return Promise.resolve([entry("session.ts", `${ROOT}/src/auth/session.ts`)]);
      return Promise.resolve([]);
    });
    const scrolled = vi.fn();
    const original = Element.prototype.scrollIntoView;
    Element.prototype.scrollIntoView = scrolled;
    try {
      const { rerender } = render(
        <WorktreeFileTree rootPath={ROOT} selectedPath={null} onSelect={vi.fn()} />,
      );
      await screen.findByText("src");
      expect(screen.queryByText("session.ts")).not.toBeInTheDocument();
      rerender(
        <WorktreeFileTree
          rootPath={ROOT}
          selectedPath={`${ROOT}/src/auth/session.ts`}
          reveal={{ path: `${ROOT}/src/auth/session.ts`, nonce: 1 }}
          onSelect={vi.fn()}
        />,
      );
      const row = await screen.findByText("session.ts");
      expect(row.closest("[role=treeitem]")).toHaveAttribute("aria-selected", "true");
      await vi.waitFor(() => expect(scrolled).toHaveBeenCalled());
    } finally {
      Element.prototype.scrollIntoView = original;
    }
  });
});

describe("WorktreeFileTree — a worktree at the filesystem root (review round 5)", () => {
  it("lists a ready root of '/' (trimmed to '') instead of saying it isn't ready", async () => {
    listDirectoryMock.mockResolvedValue([entry("etc", "/etc", { size: 3 })]);
    render(<WorktreeFileTree rootPath="" ready selectedPath={null} onSelect={vi.fn()} />);
    expect(await screen.findByText("etc")).toBeInTheDocument();
    expect(screen.queryByTestId("worktree-tree-not-ready")).not.toBeInTheDocument();
    expect(listDirectoryMock).toHaveBeenCalledWith("");
  });

  it("still says not ready when the root hasn't been reported", () => {
    render(<WorktreeFileTree rootPath="" ready={false} selectedPath={null} onSelect={vi.fn()} />);
    expect(screen.getByTestId("worktree-tree-not-ready")).toBeInTheDocument();
    expect(listDirectoryMock).not.toHaveBeenCalled();
  });
});
