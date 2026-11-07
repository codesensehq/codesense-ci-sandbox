// SPDX-License-Identifier: MIT
/**
 * Unit tests for representative Tauri invoke() wrapper functions in commands.ts.
 *
 * Each test verifies:
 * 1. invoke is called with the correct snake_case command name
 * 2. invoke is called with the correct parameter object
 * 3. The return value is passed through
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const { mockInvoke } = vi.hoisted(() => ({
  mockInvoke: vi.fn(),
}));

vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: mockInvoke,
}));

import {
  terminalLayoutAdmission,
  terminalLayoutMemberSnapshots,
  terminalCloseTab,
  subscribeTerminal,
} from "./commands";

describe("runtime admission transport", () => {
  beforeEach(() => mockInvoke.mockReset());

  it("preserves the exact required scoped reference across input, attach and dismiss", async () => {
    const target = {
      member: { machineId: "local" as const, slot: "sidecar" as const },
      namespace: { serverId: "shared", epoch: "sidecar-epoch", generation: "9007199254740993" },
      admissionRevision: "10",
      session: "reused-wire-id",
    };
    mockInvoke.mockResolvedValue(undefined);
    await writePty(target, "selected");
    expect(mockInvoke).toHaveBeenLastCalledWith("write_pty", {
      sessionId: target,
      data: "selected",
    });
    await subscribeTerminal(target);
    expect(mockInvoke).toHaveBeenLastCalledWith("subscribe_terminal", { ptyId: target });
    await terminalCloseTab(target, "keepIfBusy");
    expect(mockInvoke).toHaveBeenLastCalledWith("terminal_close_tab", {
      session: target,
      mode: "keepIfBusy",
    });
  });

  it("passes lossless admission rows and sealed snapshot replies through", async () => {
    const rows = {
      rows: [
        {
          member: { machineId: "local", slot: "sidecar" },
          revision: "18446744073709551615",
          state: "live",
          historical: null,
          namespace: { serverId: "real", epoch: "epoch", generation: "9007199254740993" },
        },
      ],
    };
    mockInvoke.mockResolvedValueOnce(rows);
    expect(await terminalLayoutAdmission()).toBe(rows);
    expect(mockInvoke).toHaveBeenLastCalledWith("terminal_layout_admission");
    const replies = [
      {
        mode: "unavailable",
        member: rows.rows[0].member,
        admissionRevision: rows.rows[0].revision,
      },
    ];
    mockInvoke.mockResolvedValueOnce(replies);
    expect(await terminalLayoutMemberSnapshots("boot")).toBe(replies);
    expect(mockInvoke).toHaveBeenLastCalledWith("terminal_layout_snapshot", {
      clientBootId: "boot",
    });
  });
});

import {
  listAttachmentsByRemote,
  saveAppState,
  loadAppState,
  ensureDirectory,
  writePty,
  resizePty,
  listContextFiles,
  detectTools,
  openTerminal,
  openEditor,
  checkPathExists,
  readFile,
  listDirectory,
  createFile,
  renamePath,
  deletePath,
  startWatching,
  stopWatching,
  addWatch,
  removeWatch,
  createContextRepo,
  getContextRepoPath,
  checkContextRepoRemote,
  connectContextRemote,
  revealContextRepo,
  getAuthenticatedUser,
  listUserOrgs,
  checkRepoExists,
  createGithubRepo,
  createPullRequest,
  mergePullRequest,
  getGitCredentialOverride,
  scanPipelineTree,
  findNearestValidScope,
  moveFileBetweenLayers,
  evaluateSetupProvenance,
  dispatchWeftWorker,
  wvRosterForSession,
  wvWorkerForceClose,
} from "./commands";

beforeEach(() => {
  mockInvoke.mockReset();
});

// === App state operations ===

describe("App state operations", () => {
  it("saveAppState invokes save_app_state with state", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await saveAppState('{"key":"val"}');
    expect(mockInvoke).toHaveBeenCalledWith("save_app_state", {
      state: '{"key":"val"}',
      patch: false,
      expectedPrincipal: "42",
    });
  });

  it("loadAppState invokes load_app_state with the boot principal and returns data", async () => {
    mockInvoke.mockResolvedValue('{"key":"val"}');
    const result = await loadAppState();
    expect(mockInvoke).toHaveBeenCalledWith("load_app_state", { expectedPrincipal: "42" });
    expect(result).toBe('{"key":"val"}');
  });
});

// === Utility ===

describe("Utility operations", () => {
  it("ensureDirectory invokes ensure_directory with path", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await ensureDirectory("/dir");
    expect(mockInvoke).toHaveBeenCalledWith("ensure_directory", { path: "/dir" });
  });
});

// === PTY operations ===

describe("PTY operations", () => {
  it("writePty invokes write_pty with sessionId and data", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await writePty("sess1", "ls\n");
    expect(mockInvoke).toHaveBeenCalledWith("write_pty", { sessionId: "sess1", data: "ls\n" });
  });

  it("resizePty invokes resize_pty with sessionId, cols, and rows", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await resizePty("sess1", 120, 40);
    expect(mockInvoke).toHaveBeenCalledWith("resize_pty", {
      sessionId: "sess1",
      cols: 120,
      rows: 40,
    });
  });
});

// === File index ===

describe("File index", () => {
  it("listContextFiles invokes list_context_files with rootPath and returns files", async () => {
    const files = [{ name: "a.md", relativePath: "a.md", fullPath: "/root/a.md" }];
    mockInvoke.mockResolvedValue(files);
    const result = await listContextFiles("/root");
    expect(mockInvoke).toHaveBeenCalledWith("list_context_files", { rootPath: "/root" });
    expect(result).toBe(files);
  });
});

// === Tool detection ===

describe("Tool detection", () => {
  it("detectTools invokes detect_tools with no params and returns tools", async () => {
    const tools = { terminals: [], editors: [], default_terminal: "t", default_editor: "e" };
    mockInvoke.mockResolvedValue(tools);
    const result = await detectTools();
    expect(mockInvoke).toHaveBeenCalledWith("detect_tools");
    expect(result).toBe(tools);
  });

  it("openTerminal invokes open_terminal with terminal, customCmd, repoPath, and runCommand", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await openTerminal("iterm", "custom", "/repo", "npm start");
    expect(mockInvoke).toHaveBeenCalledWith("open_terminal", {
      terminal: "iterm",
      customCmd: "custom",
      repoPath: "/repo",
      runCommand: "npm start",
    });
  });

  it("openTerminal defaults runCommand to empty string", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await openTerminal("iterm", "custom", "/repo");
    expect(mockInvoke).toHaveBeenCalledWith("open_terminal", {
      terminal: "iterm",
      customCmd: "custom",
      repoPath: "/repo",
      runCommand: "",
    });
  });

  it("openEditor invokes open_editor with editor, customCmd, and repoPath", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await openEditor("vscode", "code", "/repo");
    expect(mockInvoke).toHaveBeenCalledWith("open_editor", {
      editor: "vscode",
      customCmd: "code",
      repoPath: "/repo",
    });
  });
});

// === File operations ===

describe("File operations", () => {
  it("checkPathExists invokes check_path_exists with path and returns boolean", async () => {
    mockInvoke.mockResolvedValue(true);
    const result = await checkPathExists("/file");
    expect(mockInvoke).toHaveBeenCalledWith("check_path_exists", { path: "/file" });
    expect(result).toBe(true);
  });

  it("readFile invokes read_file with path and returns content", async () => {
    mockInvoke.mockResolvedValue("file content");
    const result = await readFile("/file.txt");
    expect(mockInvoke).toHaveBeenCalledWith("read_file", { path: "/file.txt" });
    expect(result).toBe("file content");
  });

  it("listDirectory invokes list_directory with dirPath and returns entries", async () => {
    const entries = [{ name: "a", path: "/a", is_dir: false, extension: "txt", size: 1 }];
    mockInvoke.mockResolvedValue(entries);
    const result = await listDirectory("/dir");
    expect(mockInvoke).toHaveBeenCalledWith("list_directory", { dirPath: "/dir" });
    expect(result).toBe(entries);
  });

  it("createFile invokes create_file with path and content", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await createFile("/new.txt", "hello");
    expect(mockInvoke).toHaveBeenCalledWith("create_file", { path: "/new.txt", content: "hello" });
  });

  it("renamePath invokes rename_path with oldPath and newPath", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await renamePath("/old", "/new");
    expect(mockInvoke).toHaveBeenCalledWith("rename_path", { oldPath: "/old", newPath: "/new" });
  });

  it("deletePath invokes delete_path with path", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await deletePath("/gone");
    expect(mockInvoke).toHaveBeenCalledWith("delete_path", { path: "/gone" });
  });
});

// === Pipeline scanner (story 30.7) ===

describe("Pipeline scanner", () => {
  it("scanPipelineTree invokes scan_pipeline_tree with camelCase params and returns tree", async () => {
    const tree = {
      dirPath: "",
      dirName: "",
      absoluteDirPath: "/abs",
      stories: [],
      subEpics: [],
      orderError: null,
      mainDoc: null,
    };
    mockInvoke.mockResolvedValue(tree);
    // PTS-2: scanPipelineTree takes teamContentPath; viewer_root retired.
    const result = await scanPipelineTree("/ctx/_team/default/content", "impl", "in-progress", "");
    expect(mockInvoke).toHaveBeenCalledWith("scan_pipeline_tree", {
      teamContentPath: "/ctx/_team/default/content",
      pipelineRoot: "impl",
      scopePath: "in-progress",
      mainDoc: "",
      summaryHeadings: null,
      configuredSourceRepos: null,
      validTeams: null,
    });
    expect(result).toBe(tree);
  });

  it("findNearestValidScope invokes find_nearest_valid_scope with camelCase params and returns string", async () => {
    mockInvoke.mockResolvedValue("a/b");
    // PTS-2: takes teamContentPath; viewer_root retired.
    const result = await findNearestValidScope("/ctx/_team/default/content", "impl", "a/b/ghost");
    expect(mockInvoke).toHaveBeenCalledWith("find_nearest_valid_scope", {
      teamContentPath: "/ctx/_team/default/content",
      pipelineRoot: "impl",
      scopePath: "a/b/ghost",
    });
    expect(result).toBe("a/b");
  });
});

// === Watch operations ===

describe("Watch operations", () => {
  it("startWatching invokes start_watching with watchConfigs and a lifecycle seq", async () => {
    const configs = [{ repo_path: "/r", context_repo_path: "/c", viewer_root: "_bmad-output" }];
    mockInvoke.mockResolvedValue(undefined);
    await startWatching(configs);
    // SW-WL-6: a monotonic `seq` rides along so Rust can resolve a stop-vs-restart
    // race to last-intent-wins; the exact value depends on prior calls, so match
    // the shape, not the number.
    expect(mockInvoke).toHaveBeenCalledWith(
      "start_watching",
      expect.objectContaining({
        watchConfigs: configs,
        seq: expect.any(Number),
        expectedPrincipal: "42",
      }),
    );
  });

  it("stopWatching invokes stop_watching with a lifecycle seq", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await stopWatching();
    expect(mockInvoke).toHaveBeenCalledWith(
      "stop_watching",
      expect.objectContaining({ seq: expect.any(Number), expectedPrincipal: "42" }),
    );
  });

  it("mints a strictly increasing lifecycle seq across start/stop calls", async () => {
    mockInvoke.mockResolvedValue(undefined);
    mockInvoke.mockClear();
    await startWatching([]);
    await stopWatching();
    const firstSeq = (mockInvoke.mock.calls[0][1] as { seq: number }).seq;
    const secondSeq = (mockInvoke.mock.calls[1][1] as { seq: number }).seq;
    expect(secondSeq).toBeGreaterThan(firstSeq);
  });

  it("addWatch invokes add_watch with watchConfig", async () => {
    const config = { repo_path: "/r", context_repo_path: "/c", viewer_root: "_specs" };
    mockInvoke.mockResolvedValue(undefined);
    await addWatch(config);
    expect(mockInvoke).toHaveBeenCalledWith("add_watch", {
      watchConfig: config,
      expectedPrincipal: "42",
    });
  });

  it("removeWatch invokes remove_watch with watchConfig", async () => {
    const config = { repo_path: "/r", context_repo_path: "/c", viewer_root: "_bmad-output" };
    mockInvoke.mockResolvedValue(undefined);
    await removeWatch(config);
    expect(mockInvoke).toHaveBeenCalledWith("remove_watch", {
      watchConfig: config,
      expectedPrincipal: "42",
    });
  });
});

// === Context repo operations ===

describe("Context repo operations", () => {
  it("createContextRepo rejects retired access locally without IPC", async () => {
    await expect(createContextRepo("/repo")).rejects.toThrow(/retired/);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("getContextRepoPath invokes get_context_repo_path with repoPath and returns path", async () => {
    mockInvoke.mockResolvedValue("/ctx/path");
    const result = await getContextRepoPath("/repo");
    expect(mockInvoke).toHaveBeenCalledWith("get_context_repo_path", { repoPath: "/repo" });
    expect(result).toBe("/ctx/path");
  });

  it("checkContextRepoRemote invokes check_context_repo_remote with repoPath", async () => {
    mockInvoke.mockResolvedValue("https://github.com/org/repo");
    const result = await checkContextRepoRemote("/repo");
    expect(mockInvoke).toHaveBeenCalledWith("check_context_repo_remote", { repoPath: "/repo" });
    expect(result).toBe("https://github.com/org/repo");
  });

  it("connectContextRemote rejects retired access locally without IPC", async () => {
    await expect(connectContextRemote("/repo", "https://github.com/org/repo")).rejects.toThrow(
      /retired/,
    );
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("revealContextRepo invokes reveal_context_repo with repoPath", async () => {
    mockInvoke.mockResolvedValue(undefined);
    await revealContextRepo("/repo");
    expect(mockInvoke).toHaveBeenCalledWith("reveal_context_repo", { repoPath: "/repo" });
  });
});

// === GitHub operations ===

describe("GitHub operations", () => {
  it("getAuthenticatedUser rejects retired access locally without IPC", async () => {
    await expect(getAuthenticatedUser()).rejects.toThrow(/retired/);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("listUserOrgs rejects retired access locally without IPC", async () => {
    await expect(listUserOrgs()).rejects.toThrow(/retired/);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("checkRepoExists rejects retired access locally without IPC", async () => {
    await expect(checkRepoExists("myorg", "myrepo")).rejects.toThrow(/retired/);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("createGithubRepo rejects retired access locally without IPC", async () => {
    await expect(createGithubRepo("myorg", "myrepo", true)).rejects.toThrow(/retired/);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("createPullRequest rejects retired access locally without IPC", async () => {
    await expect(
      createPullRequest("owner", "repo", "title", "body", "feature", "main"),
    ).rejects.toThrow(/retired/);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("mergePullRequest rejects retired access locally without IPC", async () => {
    await expect(mergePullRequest("owner", "repo", 42)).rejects.toThrow(/retired/);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("getGitCredentialOverride rejects retired access locally without IPC", async () => {
    await expect(getGitCredentialOverride()).rejects.toThrow(/retired/);
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});

// SW-LD-4 removed the embedded Atlassian OAuth 3LO commands (and the
// Jira token heartbeat), so their command wrappers and tests are gone.
// Atlassian returns as a BYO-token passthrough in SW-LD-5.

// === F-6 Attachments (R3-P8) ===

describe("listAttachmentsByRemote", () => {
  it("invokes list_attachments_by_remote with githubUserId and converts Record to Map", async () => {
    const record = {
      "owner/repo": {
        repo_path: "/Users/dev/owner-repo",
        remote_url: "https://github.com/owner/repo.git",
        attached_at: "2026-05-01T00:00:00Z",
        tracking_prompt_dismissed_at: null,
        credential_ref: null,
      },
      "another/proj": {
        repo_path: "/Users/dev/another-proj",
        remote_url: "git@github.com:another/proj.git",
        attached_at: null,
        tracking_prompt_dismissed_at: null,
        credential_ref: null,
      },
    };
    mockInvoke.mockResolvedValue(record);

    const result = await listAttachmentsByRemote(1234);

    expect(mockInvoke).toHaveBeenCalledWith("list_attachments_by_remote", {
      githubUserId: 1234,
    });
    expect(result).toBeInstanceOf(Map);
    expect(result.size).toBe(2);
    expect(result.get("owner/repo")).toEqual(record["owner/repo"]);
    expect(result.get("another/proj")).toEqual(record["another/proj"]);
  });

  it("returns empty Map when Rust returns empty object", async () => {
    mockInvoke.mockResolvedValue({});
    const result = await listAttachmentsByRemote(1234);
    expect(result).toBeInstanceOf(Map);
    expect(result.size).toBe(0);
  });

  it("returns empty Map when Rust returns null/undefined (defensive)", async () => {
    // R3-P9: Object.entries(null) would throw without the `?? {}` guard.
    mockInvoke.mockResolvedValue(null);
    const result = await listAttachmentsByRemote(1234);
    expect(result).toBeInstanceOf(Map);
    expect(result.size).toBe(0);

    mockInvoke.mockResolvedValue(undefined);
    const result2 = await listAttachmentsByRemote(1234);
    expect(result2.size).toBe(0);
  });
});

// === Layer move operations (SS-FU-15 / SS-FU-18) ===
describe("Layer move operations", () => {
  const baseParams = {
    fromLayer: "TeamProjection" as const,
    toLayer: "TeamProjection" as const,
    logicalName: ".claude/skills/bmad-tea/SKILL.md",
    codeRepoSlug: null,
    codesenseHome: "/home/.codesense",
    userId: 1,
    orgSlug: "alderco/codesense",
    teamName: "default",
  };

  it("moveFileBetweenLayers forwards force for a single-file Replace", async () => {
    mockInvoke.mockResolvedValue({ kind: "moved", count: 1 });
    const params = { ...baseParams, force: true };
    const res = await moveFileBetweenLayers(params);
    expect(mockInvoke).toHaveBeenCalledWith("move_file_between_layers", { params });
    expect(res).toEqual({ kind: "moved", count: 1 });
  });

  it("moveFileBetweenLayers passes subtree + conflictPolicy and returns a named skip list", async () => {
    const outcome = {
      kind: "movedPartial" as const,
      moved: 2,
      skipped: [".claude/skills/bmad-tea/steps/step-01.md"],
    };
    mockInvoke.mockResolvedValue(outcome);
    const params = {
      ...baseParams,
      logicalName: ".claude/skills/bmad-tea",
      moveAsSubtree: true,
      conflictPolicy: "skipExisting" as const,
    };
    const res = await moveFileBetweenLayers(params);
    expect(mockInvoke).toHaveBeenCalledWith("move_file_between_layers", { params });
    // `skipped` is a string[] of logical names (SS-FU-18), not a count.
    expect(res).toEqual(outcome);
    if (res.kind === "movedPartial") {
      expect(res.skipped).toEqual([".claude/skills/bmad-tea/steps/step-01.md"]);
    }
  });
});

describe("SW-WT-9 provenance gate", () => {
  it("evaluateSetupProvenance forwards repo + org_root", async () => {
    mockInvoke.mockResolvedValue({ provenance: "team_synced", config_hash: "h1" });
    const res = await evaluateSetupProvenance("/repo", "/org");
    expect(mockInvoke).toHaveBeenCalledWith("evaluate_setup_provenance", {
      repoPath: "/repo",
      orgRoot: "/org",
    });
    expect(res).toEqual({ provenance: "team_synced", config_hash: "h1" });
  });
});

describe("weft dispatch bridge", () => {
  it("dispatchWeftWorker calls invoke with wv_dispatch_worker and all params", async () => {
    mockInvoke.mockResolvedValueOnce({
      ok: true,
      dispatch_id: "d1",
      worker_handle: "wh1",
      done_when: "PR opened",
    });
    const result = await dispatchWeftWorker({
      queueId: 42,
      subclass: "impl",
      goal: "Refactor auth",
      context: "",
      constraints: "",
      doneWhen: "PR opened",
    });
    expect(mockInvoke).toHaveBeenCalledWith("wv_dispatch_worker", {
      queueId: 42,
      subclass: "impl",
      goal: "Refactor auth",
      context: "",
      constraints: "",
      doneWhen: "PR opened",
    });
    expect(result.ok).toBe(true);
    expect(result.dispatch_id).toBe("d1");
    expect(result.worker_handle).toBe("wh1");
    expect(result.done_when).toBe("PR opened");
  });

  // SW-WV-F3 (AC-6 layer 3): the fleet-view command-parity contract — each FE wrapper
  // invokes the exact snake_case name the Rust handler is registered under.
  it("wvRosterForSession calls invoke with wv_roster_for_session and sessionId", async () => {
    mockInvoke.mockResolvedValueOnce({
      subclass: "impl",
      kind: "claude",
      current_task: null,
      status: "dispatch",
    });
    const result = await wvRosterForSession("sess-1");
    expect(mockInvoke).toHaveBeenCalledWith("wv_roster_for_session", { sessionId: "sess-1" });
    expect(result?.subclass).toBe("impl");
  });

  it("wvWorkerForceClose calls invoke with wv_worker_force_close and handle", async () => {
    mockInvoke.mockResolvedValueOnce(undefined);
    await wvWorkerForceClose("wh1");
    expect(mockInvoke).toHaveBeenCalledWith("wv_worker_force_close", { handle: "wh1" });
  });
});
