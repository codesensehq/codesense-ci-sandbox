// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it } from "vitest";

import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

import { openInWorktreeEditor, useWorktreeEditorRevealStore } from "./open-in-worktree-editor";

// The stable entry point other tools call: one spelling per file, no paths out
// of the worktree, absolute paths inside it accepted, and a one-shot reveal.

const TEAM = "org/team";

function seedWorkspace(ws: Partial<Workspace> & { id: string }) {
  useViewerStore.setState((s) => ({
    workspaces: { ...s.workspaces, [TEAM]: [ws as Workspace] },
  }));
}

const open = (path: string, line?: number, column?: number) =>
  openInWorktreeEditor({ sessionId: "s1", workspaceId: "w1", teamKey: TEAM, path, line, column });

const request = () => useWorktreeEditorRevealStore.getState().requests.w1 ?? null;

beforeEach(() => {
  useWorktreeEditorRevealStore.setState({ requests: {} });
  useViewerStore.setState((s) => ({
    workspaces: { ...s.workspaces, [TEAM]: [] },
    worktreeEditorFile: {},
  }));
});

describe("openInWorktreeEditor", () => {
  it("normalises spellings of the same file", () => {
    open("./src//a.ts");
    expect(request()?.path).toBe("src/a.ts");
    expect(useViewerStore.getState().worktreeEditorFile.w1).toBe("src/a.ts");
  });

  it("ignores an empty path or one that climbs out of the worktree", () => {
    open("../etc/passwd");
    open("./");
    expect(request()).toBeNull();
  });

  it("opens an absolute path inside the worktree as its worktree file", () => {
    seedWorkspace({ id: "w1", cwd: "/Users/dev/wt/BUG-1/" });
    open("/Users/dev/wt/BUG-1/src/a.ts", 12);
    expect(request()).toMatchObject({ path: "src/a.ts", line: 12, column: null });
  });

  it("uses the machine directory for a remote workspace", () => {
    seedWorkspace({
      id: "w1",
      cwd: "",
      location: { kind: "machine", machineId: "m1", cwd: "/home/dev/wt", handle: "h" },
    } as Partial<Workspace> & { id: string });
    open("/home/dev/wt/lib/b.rs");
    expect(request()?.path).toBe("lib/b.rs");
  });

  it("drops a line or column that isn't a positive number", () => {
    open("a.ts", 0, Number.NaN);
    expect(request()).toMatchObject({ line: null, column: null });
    open("a.ts", 3.7, 2);
    expect(request()).toMatchObject({ line: 3, column: 2 });
  });

  it("a consumed request is dropped, but not one that replaced it", () => {
    open("a.ts", 1);
    const first = request()!;
    open("b.ts", 2);
    useWorktreeEditorRevealStore.getState().consume("w1", first.nonce);
    expect(request()?.path).toBe("b.ts");
    useWorktreeEditorRevealStore.getState().consume("w1", request()!.nonce);
    expect(request()).toBeNull();
  });
});
