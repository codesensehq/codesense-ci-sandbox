// SPDX-License-Identifier: MIT
import { currentContextNavigationRevision } from "@/shared/lib/context-navigation";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { buildTree } from "@/shared/lib/pipeline-graph/test-fixtures/derived-tree";
import { useScratchTeamUseStore } from "@/shared/stores/scratch-team-use-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";

import { useNavFileActions } from "./use-nav-file-actions";

// SW-XS-20: only FILES carry an owning team. A file from a parent team is edited,
// renamed, moved (from the menu) and deleted in that team. Folders are shared
// across the chain: anything created or dropped into a folder lands in the
// ACTIVE team's copy of it — even a folder only a parent has.

const ROOT = "/org/default/content";
const PARENT = "/org/platform/content";

it("defaults a Private source New file to Only me in the selected private subfolder", async () => {
  const privateRoot = "/home/users/42/scratch/orgs/acme/plans/content";
  const teamKey = "acme/plans:default";
  useViewerStore.setState({ navLens: { [teamKey]: { source: { kind: "private" } } as never } });
  useOrgStore.setState({ activeOrgSlug: "acme/plans" });
  const { result } = renderHook(() =>
    useNavFileActions({
      teamKey,
      repoPath: ROOT,
      rootAnchor: privateRoot,
      activeTeamName: "default",
      tree: buildTree("root", { absoluteDirPath: privateRoot }),
      onFileSelect: vi.fn(),
      contextMenu: null,
      setContextMenu: vi.fn(),
      toRel: () => null,
    }),
  );
  act(() => result.current.setNewFileDir(`${privateRoot}/docs`));
  expect(result.current.newFileAudience).toBe("only-me");
  await act(() => result.current.confirmNewFile("idea.md"));
  expect(createScratchFile).toHaveBeenCalledWith(
    "acme/plans",
    "default",
    `${privateRoot}/docs`,
    "idea.md",
  );
  expect(createContent).not.toHaveBeenCalled();
});

const {
  ingestFile,
  moveContentTo,
  deleteContent,
  createContent,
  renameContent,
  createScratchFile,
  toastError,
  toastSuccess,
} = vi.hoisted(() => ({
  createContent: vi.fn(async (dir: string, name: string) => ({
    ok: true,
    path: `${dir}/${name}`,
  })),
  renameContent: vi.fn(async (path: string, name: string) => ({
    ok: true,
    newPath: `${path.slice(0, path.lastIndexOf("/"))}/${name}`,
  })),
  createScratchFile: vi.fn(
    async (_o: string, _t: string, dir: string, name: string) => `/scratch${dir}/${name}`,
  ),
  ingestFile: vi.fn(async (_src: string, dir: string, name: string) => ({
    ok: true,
    path: `${dir}/${name}`,
  })),
  moveContentTo: vi.fn(async () => ({ ok: true })),
  deleteContent: vi.fn(async () => ({ ok: true })),
  toastError: vi.fn(),
  toastSuccess: vi.fn(),
}));

vi.mock("@/features/dashboard/lib/structural-ops", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/features/dashboard/lib/structural-ops")>();
  return { ...actual, ingestFile, moveContentTo, deleteContent, createContent, renameContent };
});
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, getScratchTeamUse: vi.fn(async () => true), createScratchFile };
});
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { success: toastSuccess, error: toastError }),
}));

const tree = buildTree("root", {
  absoluteDirPath: ROOT,
  subEpics: [
    // Only the parent has this folder; it sits at the active team's place.
    buildTree("rules", {
      dirPath: "rules",
      absoluteDirPath: `${ROOT}/rules`,
      docs: [{ fileName: "rule.md", filePath: `${PARENT}/rules/rule.md`, sourceTeam: "platform" }],
    }),
    // The team's own folder that also shows parent files.
    buildTree("skills", {
      dirPath: "skills",
      absoluteDirPath: `${ROOT}/skills`,
      docs: [
        { fileName: "mine.md", filePath: `${ROOT}/skills/mine.md`, overridesTeam: "platform" },
        { fileName: "a.md", filePath: `${PARENT}/skills/a.md`, sourceTeam: "platform" },
        { fileName: "b.md", filePath: `${PARENT}/skills/b.md`, sourceTeam: "platform" },
      ],
    }),
    // Dotted folders: one only the parent has, one the team has.
    buildTree(".shared", {
      dirPath: ".shared",
      absoluteDirPath: `${ROOT}/.shared`,
      docs: [{ fileName: "p.md", filePath: `${PARENT}/.shared/p.md`, sourceTeam: "platform" }],
    }),
    buildTree(".claude", {
      dirPath: ".claude",
      absoluteDirPath: `${ROOT}/.claude`,
      docs: [{ fileName: "c.md", filePath: `${ROOT}/.claude/c.md` }],
    }),
    buildTree("notes", {
      dirPath: "notes",
      absoluteDirPath: `${ROOT}/notes`,
      docs: [{ fileName: "own.md", filePath: `${ROOT}/notes/own.md` }],
    }),
  ],
});

const onFileSelect = vi.fn();

function renderActions(menuPath: string | null = null) {
  return renderHook(() =>
    useNavFileActions({
      repoPath: "/repo",
      teamKey: "alderco:default",
      tree,
      activeTeamName: "default",
      onFileSelect,
      contextMenu: menuPath ? ({ x: 0, y: 0, path: menuPath } as never) : null,
      setContextMenu: vi.fn(),
      rootAnchor: ROOT,
      toRel: (abs) =>
        abs === ROOT ? "" : abs.startsWith(`${ROOT}/`) ? abs.slice(ROOT.length + 1) : null,
    }),
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  useScratchTeamUseStore.setState({ byTeam: {}, saving: {} });
});

it.each(["rename", "delete", "move"])(
  "refreshes the private source immediately after %s",
  async (action) => {
    const privateRoot = "/home/users/42/scratch/orgs/acme/plans/content";
    const path = `${privateRoot}/2026/plan.md`;
    const teamKey = "acme/plans:default";
    useFileRefreshStore.setState({
      fileRefreshKeys: {},
      contentRefreshKeys: {},
      lastChangedContextFile: {},
    });
    const { result } = renderHook(() =>
      useNavFileActions({
        teamKey,
        repoPath: ROOT,
        rootAnchor: privateRoot,
        activeTeamName: "default",
        tree: buildTree("root", { absoluteDirPath: privateRoot }),
        onFileSelect: vi.fn(),
        contextMenu: null,
        setContextMenu: vi.fn(),
        toRel: (abs) => abs.slice(privateRoot.length + 1),
      }),
    );
    if (action === "rename") {
      act(() => result.current.setRenameTarget(path));
      await act(() => result.current.confirmRename("renamed.md"));
    } else if (action === "delete") {
      act(() => result.current.setDeleteTarget(path));
      await act(() => result.current.confirmDelete());
    } else {
      await act(() => result.current.performMove(path, `${privateRoot}/archive`));
    }
    expect(useFileRefreshStore.getState().fileRefreshKeys[privateRoot]).toBe(1);
    expect(useFileRefreshStore.getState().fileRefreshKeys[ROOT]).toBeUndefined();
  },
);

describe("useNavFileActions — SW-XS-20 drops and moves", () => {
  it("drops an OS file into a parent-only folder in the ACTIVE team's copy of it", async () => {
    const { result } = renderActions();
    act(() => result.current.handleOsDrop({ dir: `${ROOT}/rules` }, ["/Users/me/new.md"]));
    await vi.waitFor(() =>
      expect(ingestFile).toHaveBeenCalledWith("/Users/me/new.md", `${ROOT}/rules`, "new.md"),
    );
    expect(toastError).not.toHaveBeenCalled();
  });

  it("moves an own file into any folder straight away, in the active team", async () => {
    const { result } = renderActions();
    act(() => result.current.beginMove(`${ROOT}/notes/own.md`, `${ROOT}/rules`));
    expect(result.current.moveCrossScope).toBeNull();
    await vi.waitFor(() =>
      expect(moveContentTo).toHaveBeenCalledWith(`${ROOT}/notes/own.md`, `${ROOT}/rules`),
    );
  });

  it("moves a parent's file within the parent, after a confirm naming it", () => {
    const { result } = renderActions();
    act(() => result.current.beginMove(`${PARENT}/rules/rule.md`, `${ROOT}/notes`));
    expect(moveContentTo).not.toHaveBeenCalled();
    expect(result.current.moveCrossScope).toEqual({
      from: `${PARENT}/rules/rule.md`,
      toDir: `${PARENT}/notes`,
      team: "platform",
    });
  });
});

describe("useNavFileActions — SW-XS-20 Copy into ‹team›", () => {
  it("copies a parent's file to the same path in the active team and opens it", async () => {
    const { result } = renderActions();
    await act(() => result.current.copyIntoActiveTeam(`${PARENT}/rules/rule.md`));
    expect(ingestFile).toHaveBeenCalledWith(`${PARENT}/rules/rule.md`, `${ROOT}/rules`, "rule.md");
    expect(toastSuccess).toHaveBeenCalledWith("Copied into default");
    expect(onFileSelect).toHaveBeenCalledWith(`${ROOT}/rules/rule.md`);
  });

  it("refuses when the active team already has that path", async () => {
    ingestFile.mockResolvedValueOnce({
      ok: false,
      error: "File already exists",
      collision: true,
    } as never);
    const { result } = renderActions();
    await act(() => result.current.copyIntoActiveTeam(`${PARENT}/skills/a.md`));
    expect(toastError).toHaveBeenCalledWith(
      "default already has skills/a.md. Open that file instead.",
    );
    expect(onFileSelect).not.toHaveBeenCalled();
  });
});

describe("useNavFileActions — SW-XS-20 confirm wording inputs", () => {
  it("names the parent when deleting or renaming its file", () => {
    const { result } = renderActions();
    act(() => result.current.setDeleteTarget(`${PARENT}/rules/rule.md`));
    expect(result.current.deleteFromParent).toBe("platform");
    act(() => result.current.setRenameTarget(`${PARENT}/rules/rule.md`));
    expect(result.current.renameFromParent).toBe("platform");
  });

  it("says the parent's version comes back when deleting an own file that overrides it", () => {
    const { result } = renderActions();
    act(() => result.current.setDeleteTarget(`${ROOT}/skills/mine.md`));
    expect(result.current.deleteFromParent).toBeNull();
    expect(result.current.deleteRevealsParent).toBe("platform");
    expect(result.current.deleteMaterialized).toBe(true);
  });

  it("says a folder delete or rename leaves the parent's files in it untouched", () => {
    const { result } = renderActions();
    act(() => result.current.setDeleteTarget(`${ROOT}/skills`));
    expect(result.current.deleteKeepsInherited).toBe(
      "2 files here come from platform; they're not changed.",
    );
    act(() => result.current.setRenameTarget(`${ROOT}/notes`));
    expect(result.current.renameKeepsInherited).toBeNull();
  });
});

describe("useNavFileActions — SW-XS-20 r2 New file and folder gestures", () => {
  it("New file from an inherited row lands in the active team's copy of its folder", async () => {
    const { result } = renderActions();
    act(() => result.current.newFileFromRow(`${PARENT}/rules/rule.md`));
    expect(result.current.newFileDir).toBe(`${ROOT}/rules`);
    await act(() => result.current.confirmNewFile("new.md"));
    expect(createContent).toHaveBeenCalledWith(`${ROOT}/rules`, "new.md");
  });

  it("Only me from an inherited row gets the team's folder, not the parent's", async () => {
    const { result } = renderActions();
    act(() => result.current.newFileFromRow(`${PARENT}/rules/rule.md`));
    act(() => result.current.setNewFileAudience("only-me"));
    await act(() => result.current.confirmNewFile("idea.md"));
    expect(createScratchFile).toHaveBeenCalledWith(
      expect.any(String),
      "default",
      `${ROOT}/rules`,
      "idea.md",
    );
  });

  it("New file from an own row or a folder row stays where it is", () => {
    const { result } = renderActions();
    act(() => result.current.newFileFromRow(`${ROOT}/notes/own.md`));
    expect(result.current.newFileDir).toBe(`${ROOT}/notes`);
    act(() => result.current.newFileFromRow(`${ROOT}/.claude`));
    expect(result.current.newFileDir).toBe(`${ROOT}/.claude`);
  });

  it("reads folders from the tree, so a dotted folder only a parent has gets no Move/Rename/Delete", () => {
    expect(renderActions(`${ROOT}/.shared`).result.current.menuFileGestures).toBe(false);
    expect(renderActions(`${ROOT}/.claude`).result.current.menuFileGestures).toBe(true);
    expect(renderActions(`${ROOT}/rules`).result.current.menuFileGestures).toBe(false);
    expect(renderActions(`${PARENT}/rules/rule.md`).result.current.menuFileGestures).toBe(true);
  });
});

it.each(["delete", "move"] as const)(
  "%s marks before host work and leaves later cleanup passive",
  async (kind) => {
    const path = `${ROOT}/notes/own.md`;
    useOrgStore.setState({ activeOrgSlug: "alderco", activeTeamName: "default" });
    useViewerStore.setState({
      openTabs: { "alderco:default": [{ path, name: "own.md" }] },
      activeTab: { "alderco:default": path },
    });
    let finish!: (result: { ok: boolean }) => void;
    const held = new Promise<{ ok: boolean }>((resolve) => {
      finish = resolve;
    });
    if (kind === "delete") deleteContent.mockReturnValueOnce(held);
    else moveContentTo.mockReturnValueOnce(held);
    const hook = renderActions();
    if (kind === "delete") act(() => hook.result.current.setDeleteTarget(path));
    const before = currentContextNavigationRevision();
    let pending!: Promise<void>;
    act(() => {
      pending =
        kind === "delete"
          ? hook.result.current.confirmDelete()
          : hook.result.current.performMove(path, `${ROOT}/rules`, true);
    });
    expect(currentContextNavigationRevision()).toBeGreaterThan(before);
    // A Show started now is newer than the file operation; cleanup must not cancel it.
    const newerShowRevision = currentContextNavigationRevision();
    await act(async () => {
      finish({ ok: true });
      await pending;
    });
    expect(currentContextNavigationRevision()).toBe(newerShowRevision);
    expect(useViewerStore.getState().openTabs["alderco:default"]).toEqual([]);
    hook.unmount();
  },
);
