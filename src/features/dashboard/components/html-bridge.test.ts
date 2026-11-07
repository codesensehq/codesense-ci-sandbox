// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { readFileSync } from "fs";
import { resolve } from "path";
import { setDashboardLaunchController, type DashboardLaunchController } from "./dashboard-launch";
import { liveHub } from "../lib/live-hub";

const mockGetDependencyStatus = vi.fn();
const mockAnalyze = vi.fn();
const mockApplyMetadataEdit = vi.fn();
const mockRouteFileToContext = vi.fn();
const mockCreateFile = vi.fn();
const mockRunSkill = vi.fn();
const mockQueryContext = vi.fn();
const mockBridgeDocContext = vi.fn();
const mockCheckPathExists = vi.fn();
const mockBridgeResolveOpenTarget = vi.fn();
const mockFileSurfaceCall = vi.fn();
const mockFileSurfacePrepareView = vi.fn();
const mockUpdateFrontmatter = vi.fn();
const mockOpenFileInTeam = vi.fn();
vi.mock("@/shared/lib/open-file-in-team", () => ({
  openFileInTeam: (...args: unknown[]) => mockOpenFileInTeam(...args),
}));
const mockOpenMessageTab = vi.fn();
vi.mock("@/features/weft/components/inbox-message-dashboard", () => ({
  openMessageTab: (...args: unknown[]) => mockOpenMessageTab(...args),
}));
const mockListAgentUpdates = vi.fn();
const mockSendRoleMessage = vi.fn();
const mockAgentReadSessions = vi.fn();
const mockAgentReadAgent = vi.fn();
const mockAgentReadInbox = vi.fn();
const mockListInboxRollup = vi.fn();
const mockAgentReadRoles = vi.fn();
const mockGetHumanStatus = vi.fn();
const mockSetHumanStatus = vi.fn();
const mockReplyAsHuman = vi.fn();
const mockReactAsHuman = vi.fn();
const mockAgentDefinitionsPrepare = vi.fn();
const mockMarkInboxMessagesRead = vi.fn();
const mockMarkInboxMessagesUnread = vi.fn();
const mockMarkInboxMessagesArchived = vi.fn();
const mockSendInboxAsHuman = vi.fn();
const mockSendPromptToAgent = vi.fn();
const mockRoutineReadList = vi.fn();
const mockRoutineReadGet = vi.fn();
const mockRoutineReadRuns = vi.fn();
const mockRoutineReadRun = vi.fn();
const mockRunReadList = vi.fn();
const mockRunRead = vi.fn();
const mockRunReadEvidence = vi.fn();
const mockRunReadWorkProduct = vi.fn();
const mockListManagedWorktrees = vi.fn();
const mockListMachines = vi.fn();
const mockSessionReadUsage = vi.fn();
const mockSessionRead = vi.fn();
const mockWhyLine = vi.fn();
const mockSetupStatus = vi.fn();
const mockSessionSetModelRate = vi.fn();
const mockSessionReadModelRates = vi.fn();
vi.mock("@/tauri/commands", () => ({
  fileSurfaceCall: (...args: unknown[]) => mockFileSurfaceCall(...args),
  fileSurfacePrepareView: (...args: unknown[]) => mockFileSurfacePrepareView(...args),
  sendPromptToAgent: (...args: unknown[]) => mockSendPromptToAgent(...args),
  routineReadList: (...a: unknown[]) => mockRoutineReadList(...a),
  routineReadGet: (...a: unknown[]) => mockRoutineReadGet(...a),
  routineReadRuns: (...a: unknown[]) => mockRoutineReadRuns(...a),
  routineReadRun: (...a: unknown[]) => mockRoutineReadRun(...a),
  runReadList: (...a: unknown[]) => mockRunReadList(...a),
  runRead: (...a: unknown[]) => mockRunRead(...a),
  runReadEvidence: (...a: unknown[]) => mockRunReadEvidence(...a),
  runReadWorkProduct: (...a: unknown[]) => mockRunReadWorkProduct(...a),
  listManagedWorktrees: (...a: unknown[]) => mockListManagedWorktrees(...a),
  listMachines: (...a: unknown[]) => mockListMachines(...a),
  sessionReadUsage: (...a: unknown[]) => mockSessionReadUsage(...a),
  sessionRead: (...a: unknown[]) => mockSessionRead(...a),
  whyLine: (...a: unknown[]) => mockWhyLine(...a),
  setupStatus: (...a: unknown[]) => mockSetupStatus(...a),
  sessionSetModelRate: (...a: unknown[]) => mockSessionSetModelRate(...a),
  sessionReadModelRates: (...a: unknown[]) => mockSessionReadModelRates(...a),
  queryContext: (q: unknown, page?: string) => mockQueryContext(q, page),
  bridgeDocContext: (page: string) => mockBridgeDocContext(page),
  checkPathExists: (path: string) => mockCheckPathExists(path),
  bridgeResolveOpenTarget: (page: string, path: string, team?: string) =>
    mockBridgeResolveOpenTarget(page, path, team),
  updateFrontmatter: (path: string, fields: unknown) => mockUpdateFrontmatter(path, fields),
  getDependencyStatus: () => mockGetDependencyStatus(),
  analyze: (args: unknown) => mockAnalyze(args),
  runSkill: (skill: string, args: string[]) => mockRunSkill(skill, args),
  applyMetadataEdit: (path: string, edits: unknown) => mockApplyMetadataEdit(path, edits),
  routeFileToContext: (...args: unknown[]) => mockRouteFileToContext(...args),
  createFile: (path: string, content: string) => mockCreateFile(path, content),
  listAgentUpdates: (...a: unknown[]) => mockListAgentUpdates(...a),
  sendRoleMessage: (...a: unknown[]) => mockSendRoleMessage(...a),
  agentReadSessions: (...a: unknown[]) => mockAgentReadSessions(...a),
  agentReadAgent: (...a: unknown[]) => mockAgentReadAgent(...a),
  agentReadInbox: (...a: unknown[]) => mockAgentReadInbox(...a),
  listInboxRollup: () => mockListInboxRollup(),
  agentReadRoles: (...a: unknown[]) => mockAgentReadRoles(...a),
  getHumanStatus: (...a: unknown[]) => mockGetHumanStatus(...a),
  setHumanStatus: (...a: unknown[]) => mockSetHumanStatus(...a),
  replyAsHuman: (...a: unknown[]) => mockReplyAsHuman(...a),
  reactAsHuman: (...a: unknown[]) => mockReactAsHuman(...a),
  agentDefinitionsPrepare: (...a: unknown[]) => mockAgentDefinitionsPrepare(...a),
  markInboxMessagesRead: (...a: unknown[]) => mockMarkInboxMessagesRead(...a),
  markInboxMessagesUnread: (...a: unknown[]) => mockMarkInboxMessagesUnread(...a),
  markInboxMessagesArchived: (...a: unknown[]) => mockMarkInboxMessagesArchived(...a),
  sendInboxAsHuman: (...a: unknown[]) => mockSendInboxAsHuman(...a),
}));
vi.mock("@/shared/stores/org-store", () => ({
  useOrgStore: {
    getState: () => ({
      activeOrgSlug: "acme",
      activeTeamName: "default",
      orgs: [
        { slug: "acme", org_root: "/org/acme", teams: [{ name: "default" }, { name: "empty" }] },
      ],
    }),
  },
}));
vi.mock("@/features/weft/lib/locate-agent", () => ({
  locateAgentByAgentId: vi.fn(() => ({ located: true })),
}));

import {
  HTML_BRIDGE_ALLOWLIST,
  BridgeCommandNotAllowedError,
  runBridgeCommand,
  ACTION_VERBS,
  READ_VERBS,
  SEND_ACTION_VERBS,
  METADATA_ACTION_VERBS,
} from "./html-bridge";

beforeEach(() => {
  liveHub.dispose();
  vi.clearAllMocks();
  mockCheckPathExists.mockReset().mockResolvedValue(true);
});

describe("sendPrompt", () => {
  const outcome = {
    status: "submitted",
    submitted: true,
    machine: null,
    code: null,
    message: null,
  };

  it("submits by handle without a launch controller", async () => {
    setDashboardLaunchController(null);
    mockSendPromptToAgent.mockResolvedValue(outcome);
    await expect(
      runBridgeCommand("sendPrompt", { handle: "agent#1", text: "hello" }),
    ).resolves.toEqual(outcome);
    expect(mockSendPromptToAgent).toHaveBeenCalledWith("agent#1", null, "hello", undefined);
    expect(SEND_ACTION_VERBS).toContain("sendPrompt");
  });

  it("forwards false to type without Enter and preserves the machine", async () => {
    const typed = { ...outcome, status: "typed", submitted: false, machine: "remote" };
    mockSendPromptToAgent.mockResolvedValue(typed);
    await expect(
      runBridgeCommand("sendPrompt", { handle: "agent#1", text: "hello", submit: false }),
    ).resolves.toEqual(typed);
    expect(mockSendPromptToAgent).toHaveBeenCalledWith("agent#1", null, "hello", false);
  });

  it("returns invalid when the command rejects an unknown or ended handle", async () => {
    mockSendPromptToAgent.mockRejectedValue(new Error("the agent has no running session"));
    await expect(
      runBridgeCommand("sendPrompt", { handle: "ended", text: "hello" }),
    ).resolves.toEqual({
      status: "invalid",
      submitted: false,
      machine: null,
      code: null,
      message: "the agent has no running session",
    });
    expect(mockSendPromptToAgent).toHaveBeenCalledTimes(1);
  });

  it.each([
    { text: "hello" },
    { handle: " ", text: "hello" },
    { handle: "agent#1", text: " " },
    { handle: "agent#1", text: "hello", submit: "false" },
  ])("validates arguments before sending: %j", async (args) => {
    await expect(runBridgeCommand("sendPrompt", args)).rejects.toThrow(/sendPrompt/);
    expect(mockSendPromptToAgent).not.toHaveBeenCalled();
  });

  it("preserves uncertain delivery so a page does not retry it", async () => {
    const unknown = {
      ...outcome,
      status: "unknown",
      submitted: null,
      machine: "remote",
      code: "staleOrigin",
    };
    mockSendPromptToAgent.mockResolvedValue(unknown);
    await expect(
      runBridgeCommand("sendPrompt", { handle: "agent#1", text: "hello" }),
    ).resolves.toEqual(unknown);
    expect(mockSendPromptToAgent).toHaveBeenCalledTimes(1);
  });
});

describe("HTML_BRIDGE_ALLOWLIST", () => {
  it("exposes exactly the enumerated read + action verbs (never raw invoke)", () => {
    // The allowlist IS the capability surface — assert its exact membership so a
    // future addition is a conscious, reviewed change, not an accident.
    expect(Object.keys(HTML_BRIDGE_ALLOWLIST).sort()).toEqual([
      "agent",
      "agents",
      "analyze",
      "archive",
      "closeTerminalTab",
      "closeWorkspace",
      "commands.list",
      "commands.register",
      "commands.run",
      "context",
      "dismiss_agent_mark",
      "draft",
      "embedMessage",
      "files.create",
      "files.delete",
      "files.list",
      "files.move",
      "files.read",
      "focusOut",
      "focusTerminal",
      "getDependencyStatus",
      "getDocDir",
      "getEpicHandle",
      "getWorkspaceStatus",
      "gotoAgent",
      "humanStatus",
      "inbox",
      "inboxRollup",
      "launchUnder",
      "loadMedia",
      "machines",
      "markRead",
      "markUnread",
      "modelRates",
      "moveWorkspace",
      "openAgentInbox",
      "openDiff",
      "openFile",
      "openLauncher",
      "openMessage",
      "openRoutineDraft",
      "openTab",
      "openWorkspace",
      "page",
      "postMessage",
      "prefs",
      "queryContext",
      "react",
      "readWorkspace",
      "records.collections",
      "records.create",
      "records.delete",
      "records.history",
      "records.list",
      "records.read",
      "records.readAt",
      "records.restore",
      "records.search",
      "records.setFields",
      "records.write",
      "replyAsHuman",
      "roles",
      "routeToContext",
      "routine",
      "routineCancel",
      "routineRefresh",
      "routineResult",
      "routineRun",
      "routineRuns",
      "routineStatus",
      "routines",
      "run",
      "runEvidence",
      "runPrompt",
      "runSkill",
      "runWorkProduct",
      "runs",
      "sendAsHuman",
      "sendPrompt",
      "sendToSession",
      "sendToWorkspaceTab",
      "sessionRead",
      "sessionUsage",
      "sessions",
      "setField",
      "setHumanStatus",
      "setModelRate",
      "setPrefs",
      "setStatus",
      "setUiState",
      "setup",
      "showMenu",
      "showPopover",
      "stopSession",
      "storyChanges",
      "storyTotals",
      "subscribe",
      "unsubscribe",
      "updates",
      "validateYaml",
      "watchAgentActivity",
      "watchAgentInbox",
      "watchInbox",
      "whyLine",
      "worktrees",
    ]);
  });

  it("queryContext (a grant-exempt read) forwards the query to the one engine", async () => {
    const answer = { total: 1, rows: [{ id: "SW-OC-26" }] };
    mockQueryContext.mockResolvedValue(answer);
    const q = { where: { kinds: ["story"] }, groupBy: "status" };
    await expect(
      runBridgeCommand("queryContext", q, { docPath: "/acme/default/content/board.html" }),
    ).resolves.toBe(answer);
    // The host-supplied doc path is the page: private items only from its org.
    expect(mockQueryContext).toHaveBeenCalledWith(q, "/acme/default/content/board.html");
    // No doc path → an unknown page (no private items); no arg = `{}`.
    await runBridgeCommand("queryContext", undefined);
    expect(mockQueryContext).toHaveBeenLastCalledWith({}, "");
    await expect(runBridgeCommand("queryContext", "status: todo")).rejects.toThrow(
      /must be an object/,
    );
    expect(READ_VERBS).toContain("queryContext");
  });

  it("does NOT expose the reserved LD-2 proxyFetch yet", () => {
    expect(HTML_BRIDGE_ALLOWLIST.proxyFetch).toBeUndefined();
  });

  // SW-DX-13 AC-4 — cross-language drift tripwire. The MCP authoring contract
  // (get_live_data_contract) documents the action-verb surface in RUST
  // (live_data_authoring.rs::ACTION_VERB_DOCS), but the SOURCE OF TRUTH is this
  // file's `ACTION_VERBS`. Rust cannot import a TS const, so — exactly as
  // commands.contract.test.ts reads lib.rs — we read the Rust file and assert
  // the documented set equals ACTION_VERBS. Add a verb here (e.g. SW-DX-11's
  // runSkill) and this FAILS until the Rust contract's const table is extended,
  // so the contract can never silently fall behind what actually ships.
  it("the Rust MCP contract documents EXACTLY the shipped ACTION_VERBS (no drift)", () => {
    const rustSource = readFileSync(
      resolve(__dirname, "../../../../src-tauri/src/mcp/live_data_authoring.rs"),
      "utf-8",
    );
    // Scope to the ACTION_VERB_DOCS const block (verb: "…" appears only there).
    // Terminate on a LINE-START `];` (the array closer sits in column 0) rather
    // than a bare `];` — so an arg string that ever contains `];` (e.g. a doc'd
    // `Array<[k,v]>;`) can't truncate the block and silently drop trailing verbs.
    const blockStart = rustSource.indexOf("const ACTION_VERB_DOCS");
    expect(blockStart, "ACTION_VERB_DOCS const block found in Rust").toBeGreaterThan(-1);
    const blockEnd = rustSource.indexOf("\n];", blockStart);
    expect(blockEnd, "ACTION_VERB_DOCS block terminates").toBeGreaterThan(blockStart);
    const block = rustSource.slice(blockStart, blockEnd);

    const documented: string[] = [];
    const verbPattern = /\bverb:\s*"([A-Za-z][A-Za-z0-9_.]*)"/g;
    let m: RegExpExecArray | null;
    while ((m = verbPattern.exec(block)) !== null) documented.push(m[1]);

    expect(new Set(documented)).toEqual(new Set(ACTION_VERBS));
  });

  // The contract's `sdk.verbs` (and the published SDK reference generated from the
  // same tables) lists the audit-exempt reads from Rust `READ_VERBS`. Same
  // tripwire as above: a read verb added here fails until Rust lists it, and the
  // two tables together must be the whole allowlist.
  it("the Rust MCP contract lists EXACTLY the shipped READ_VERBS, and reads + actions = the allowlist", () => {
    const rustSource = readFileSync(
      resolve(__dirname, "../../../../src-tauri/src/mcp/live_data_authoring.rs"),
      "utf-8",
    );
    const strConst = (name: string): string => {
      const m = rustSource.match(new RegExp(`const ${name}: &str = "([^"]+)";`));
      expect(m, `Rust const ${name} found`).not.toBeNull();
      return m![1];
    };
    const blockStart = rustSource.indexOf("const READ_VERBS: &[&str]");
    expect(blockStart, "READ_VERBS const block found in Rust").toBeGreaterThan(-1);
    const blockEnd = rustSource.indexOf("\n];", blockStart);
    const body = rustSource
      .slice(rustSource.indexOf("= &[", blockStart) + 4, blockEnd)
      .replace(/\/\/.*$/gm, "");
    const rustRead = body
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean)
      .map((item) => (item.startsWith('"') ? item.slice(1, -1) : strConst(item)));

    expect(new Set(rustRead)).toEqual(new Set(READ_VERBS));
    expect(new Set([...READ_VERBS, ...ACTION_VERBS])).toEqual(
      new Set(Object.keys(HTML_BRIDGE_ALLOWLIST)),
    );
  });
});

// SW-SD-2: the analyze bridge verb dispatches to the typed command wrapper and
// coerces `data` to the string the Rust command expects.
describe("analyze bridge verb", () => {
  it("dispatches to the analyze command and returns its text", async () => {
    mockAnalyze.mockResolvedValue("risk: elevated");
    await expect(
      runBridgeCommand("analyze", { prompt: "assess risk", data: '{"open":3}' }),
    ).resolves.toBe("risk: elevated");
    expect(mockAnalyze).toHaveBeenCalledWith({ prompt: "assess risk", data: '{"open":3}' });
  });

  it("JSON-stringifies a non-string data payload", async () => {
    mockAnalyze.mockResolvedValue("ok");
    await runBridgeCommand("analyze", { prompt: "p", data: { open: 3 } });
    expect(mockAnalyze).toHaveBeenCalledWith({ prompt: "p", data: JSON.stringify({ open: 3 }) });
  });

  it("propagates a typed backend rejection (busy/timeout/unavailable)", async () => {
    mockAnalyze.mockRejectedValue(new Error("busy"));
    await expect(runBridgeCommand("analyze", { prompt: "p", data: "d" })).rejects.toThrow("busy");
  });
});

describe("runBridgeCommand", () => {
  it("runs an allowlisted command and returns its result", async () => {
    mockGetDependencyStatus.mockResolvedValue({ ok: true });
    await expect(runBridgeCommand("getDependencyStatus", null)).resolves.toEqual({ ok: true });
    expect(mockGetDependencyStatus).toHaveBeenCalledTimes(1);
  });

  it("throws BridgeCommandNotAllowedError for a non-allowlisted command", async () => {
    await expect(runBridgeCommand("rm_rf", null)).rejects.toBeInstanceOf(
      BridgeCommandNotAllowedError,
    );
    expect(mockGetDependencyStatus).not.toHaveBeenCalled();
  });

  it("rejects a non-string command (e.g. a prototype-polluting key)", async () => {
    await expect(runBridgeCommand(undefined, null)).rejects.toBeInstanceOf(
      BridgeCommandNotAllowedError,
    );
    // Object.hasOwn guards against inherited props like "toString"/"constructor".
    await expect(runBridgeCommand("constructor", null)).rejects.toBeInstanceOf(
      BridgeCommandNotAllowedError,
    );
  });

  it("propagates a handler rejection (parent serializes it into the RPC error)", async () => {
    mockGetDependencyStatus.mockRejectedValue(new Error("upstream down"));
    await expect(runBridgeCommand("getDependencyStatus", null)).rejects.toThrow("upstream down");
  });
});

// ── SW-DX-6 action verbs ────────────────────────────────────────────────────
describe("setStatus action verb", () => {
  it("flips status via applyMetadataEdit with the setField variant (single path)", async () => {
    mockApplyMetadataEdit.mockResolvedValue(undefined);
    await expect(
      runBridgeCommand("setStatus", { path: "/ctx/a.md", status: "done" }),
    ).resolves.toEqual({ ok: true });
    expect(mockApplyMetadataEdit).toHaveBeenCalledWith("/ctx/a.md", [
      { kind: "setField", field: "status", value: "done" },
    ]);
  });

  it("surfaces the Rust managed-target rejection verbatim (single path)", async () => {
    mockApplyMetadataEdit.mockRejectedValue(
      new Error("refusing to edit metadata: /x is a read-only projected/installed view"),
    );
    await expect(runBridgeCommand("setStatus", { path: "/x", status: "done" })).rejects.toThrow(
      /refusing to edit metadata/,
    );
  });

  it("batch form returns a per-item result array (best-effort)", async () => {
    mockApplyMetadataEdit
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error("refusing to edit metadata: unmanaged"))
      .mockResolvedValueOnce(undefined);
    const res = (await runBridgeCommand("setStatus", {
      path: ["/a.md", "/bad.md", "/c.md"],
      status: "done",
    })) as Array<{ path: string; ok: boolean; error?: string }>;
    expect(res).toEqual([
      { path: "/a.md", ok: true },
      { path: "/bad.md", ok: false, error: expect.stringContaining("refusing to edit metadata") },
      { path: "/c.md", ok: true },
    ]);
    expect(mockApplyMetadataEdit).toHaveBeenCalledTimes(3);
  });

  it("rejects a non-string status or empty path set", async () => {
    await expect(runBridgeCommand("setStatus", { path: "/a", status: 3 })).rejects.toThrow(
      /status must be a string/,
    );
    await expect(runBridgeCommand("setStatus", { path: [], status: "done" })).rejects.toThrow(
      /non-empty string/,
    );
  });

  // SW-WG-17: an interpolated `{{…}}` target that resolved to empty reaches the REAL
  // handler as `path: ""` — it must REJECT before any edit (never a silent no-op that
  // hid the WG-6 blind spot), and never invoke applyMetadataEdit.
  it("rejects an empty-string path (an unresolved interpolated target) and never edits", async () => {
    await expect(runBridgeCommand("setStatus", { path: "", status: "reviewed" })).rejects.toThrow(
      /non-empty string/,
    );
    expect(mockApplyMetadataEdit).not.toHaveBeenCalled();
  });
});

describe("context read verb", () => {
  it("reads all-teams roles only for locally synced teams and preserves explicit refusals", async () => {
    const { useOrgStore } = await import("@/shared/stores/org-store");
    const original = useOrgStore.getState();
    const state = vi.spyOn(useOrgStore, "getState").mockReturnValue({
      ...original,
      orgs: [
        {
          slug: "acme",
          org_root: "/org/acme",
          teams: [{ name: "default" }, { name: "missing" }, { name: "old", archived: true }],
        },
        { slug: "other/org", org_root: "/org/other", teams: [{ name: "support" }] },
      ],
    } as never);
    mockBridgeDocContext.mockResolvedValue({ org: "acme", team: null, layer: "private" });
    mockCheckPathExists.mockImplementation(async (path: string) =>
      ["/org/acme/default/content", "/org/other/support/content"].includes(path),
    );
    const missing = { org: "acme", team: "missing" };
    const error = new Error("team acme:missing is not synced on this machine");
    mockAgentReadRoles.mockImplementation(async (team: { org: string; team: string }) => {
      if (team.team === "missing") throw error;
      return { roles: [{ role: `${team.team}-reviewer` }] };
    });
    try {
      const context = (await runBridgeCommand("context", undefined)) as {
        inboxTeams: Array<{ org: string; team: string }>;
      };
      const roles = await Promise.all(
        context.inboxTeams.map((team) => runBridgeCommand("roles", team)),
      );
      expect(context.inboxTeams).toEqual([
        { org: "acme", team: "default" },
        { org: "other/org", team: "support" },
      ]);
      expect(roles).toEqual([
        { roles: [{ role: "default-reviewer" }] },
        { roles: [{ role: "support-reviewer" }] },
      ]);
      expect(mockCheckPathExists.mock.calls).toEqual([
        ["/org/acme/default/content"],
        ["/org/acme/missing/content"],
        ["/org/other/support/content"],
      ]);
      expect(mockAgentReadRoles).toHaveBeenCalledTimes(2);
      await expect(runBridgeCommand("roles", missing)).rejects.toBe(error);
    } finally {
      state.mockRestore();
      mockAgentReadRoles.mockReset();
    }
  });

  it("answers where the HOST says the page lives, never where the page claims", async () => {
    const where = {
      org: "acme",
      team: "backend",
      layer: "team",
      contentRoot: "/h/acme/backend/content",
    };
    mockBridgeDocContext.mockResolvedValue(where);
    await expect(
      runBridgeCommand(
        "context",
        { docPath: "/elsewhere/x.html" },
        { docPath: "/h/acme/backend/content/board.html" },
      ),
    ).resolves.toMatchObject(where);
    expect(mockBridgeDocContext).toHaveBeenCalledWith("/h/acme/backend/content/board.html");
    mockBridgeDocContext.mockResolvedValue(null);
    await expect(runBridgeCommand("context", undefined)).resolves.toBeNull();
    expect(mockBridgeDocContext).toHaveBeenLastCalledWith("");
  });
  it("exposes location and team metadata without a human-inbox permission flag", async () => {
    mockBridgeDocContext.mockResolvedValue({ org: "acme", team: null, layer: "private" });
    const context = await runBridgeCommand("context", undefined);
    expect(context).toMatchObject({
      org: "acme",
      team: null,
      layer: "private",
    });
    expect(context).not.toHaveProperty("humanInbox");
    expect(context).toMatchObject({
      activeTeam: { org: "acme", team: "default" },
      inboxTeams: [
        { org: "acme", team: "default" },
        { org: "acme", team: "empty" },
      ],
    });
  });
});

describe("setField action verb", () => {
  it("writes one field through update_frontmatter; null removes the key", async () => {
    mockUpdateFrontmatter.mockResolvedValue(undefined);
    await expect(
      runBridgeCommand("setField", { path: "/c/SW-1.md", field: "points", value: 5 }),
    ).resolves.toEqual({ ok: true });
    expect(mockUpdateFrontmatter).toHaveBeenCalledWith("/c/SW-1.md", { points: 5 });
    await runBridgeCommand("setField", { path: "/c/SW-1.md", field: "sprint", value: null });
    expect(mockUpdateFrontmatter).toHaveBeenLastCalledWith("/c/SW-1.md", { sprint: null });
    await runBridgeCommand("setField", {
      path: "/c/SW-1.md",
      fields: { assigned_to: "alice", rank: 3, depends_on: ["SW-0"] },
    });
    expect(mockUpdateFrontmatter).toHaveBeenLastCalledWith("/c/SW-1.md", {
      assigned_to: "alice",
      rank: 3,
      depends_on: ["SW-0"],
    });
  });

  it("rejects a malformed call before writing, and surfaces a Rust refusal verbatim", async () => {
    for (const bad of [
      { field: "points", value: 1 },
      { path: "", field: "points", value: 1 },
      { path: "/c/a.md", value: 1 },
      { path: "/c/a.md", field: "points" },
      { path: "/c/a.md", fields: [1] },
      { path: "/c/a.md", field: "x", value: 1, fields: { y: 1 } },
    ]) {
      await expect(runBridgeCommand("setField", bad)).rejects.toThrow(/^setField:/);
    }
    expect(mockUpdateFrontmatter).not.toHaveBeenCalled();
    mockUpdateFrontmatter.mockRejectedValue(
      new Error(
        "refusing to edit metadata: /etc/x.md is not inside a team or private content root",
      ),
    );
    await expect(
      runBridgeCommand("setField", { path: "/etc/x.md", field: "points", value: 1 }),
    ).rejects.toThrow(/^refusing to edit metadata/);
  });
});

describe("openFile action verb", () => {
  for (const [kind, path] of [
    ["private", "/private/notes.md"],
    ["inherited", "/parent/content/shared.md"],
  ]) {
    it(`opens admitted ${kind} files in the requesting pane team, with preview and pin`, async () => {
      const { frameFixture } = await import("../../../../tests/support/fixtures/frame");
      const frame = frameFixture({
        documentPath: "builtin:files-tree",
        contentRoot: "/child/content",
        slot: "file-nav",
        builtinTeam: { org: "acme", team: "child" },
      });
      const target = { path, org: "acme", team: "child" };
      mockFileSurfaceCall.mockResolvedValue(target);
      mockBridgeResolveOpenTarget.mockRejectedValue(
        new Error("general opener refuses private/parent"),
      );
      for (const preview of [true, false]) {
        await expect(runBridgeCommand("openFile", { path, preview }, { frame })).resolves.toEqual(
          target,
        );
        expect(mockFileSurfaceCall).toHaveBeenLastCalledWith(
          "resolve-open",
          { path },
          frame.frameId,
        );
        expect(mockOpenFileInTeam).toHaveBeenLastCalledWith(
          target,
          expect.objectContaining({ newTab: !preview }),
        );
      }
      expect(mockBridgeResolveOpenTarget).not.toHaveBeenCalled();
    });
  }

  it("opens an installed listing row from a custom file-nav pane in its requesting viewer team", async () => {
    const { frameFixture } = await import("../../../../tests/support/fixtures/frame");
    const frame = frameFixture({
      documentPath: "/private/panes/custom-files.html",
      contentRoot: "/private/content",
      slot: "file-nav",
    });
    frame.frameId = "custom-file-nav";
    frame.grant.frameId = frame.frameId;
    frame.grant.team = { org: "acme", team: "child" };
    expect(frame.kind).not.toBe("builtin");
    const row = {
      id: "/source-repo/content/guide.md",
      path: "/source-repo/content/guide.md",
      name: "guide.md",
      kind: "file",
      installedSourceRepo: "owner/repo",
    };
    const target = { path: row.path, ...frame.grant.team };
    mockFileSurfaceCall.mockImplementation(async (operation, args, frameId) => {
      expect(frameId).toBe(frame.frameId);
      expect(operation).toBe("resolve-open");
      expect(args).toEqual({ path: row.path });
      return target;
    });
    mockBridgeResolveOpenTarget.mockRejectedValue(
      new Error("refused: installed path is outside Team content roots"),
    );
    try {
      const args = {
        topic: "files",
        scope: "team",
        dir: "installed://owner/repo/docs",
        window: [0, 200],
      };
      const viewKey = "team:acme/child|files:installed-docs";
      mockFileSurfacePrepareView.mockResolvedValueOnce(viewKey);
      const subscribe = vi.fn(async () => ({ topic: "files", viewKey, rev: 1, snapshot: [row] }));
      const ctx = { frame, liveFrame: async () => ({ subscribe }) as never };
      const listing = (await runBridgeCommand("subscribe", args, ctx)) as {
        snapshot: Array<{ path: string }>;
      };
      const opened = await runBridgeCommand("openFile", { path: listing.snapshot[0].path }, ctx);
      expect(opened).toEqual({ path: row.path, org: "acme", team: "child" });
      expect(mockFileSurfacePrepareView).toHaveBeenCalledWith(frame.frameId, args);
      expect(subscribe).toHaveBeenCalledWith("files", undefined, { viewKey, window: [0, 200] });
      expect(mockFileSurfaceCall).toHaveBeenCalledExactlyOnceWith(
        "resolve-open",
        { path: row.path },
        frame.frameId,
      );
      expect(mockOpenFileInTeam).toHaveBeenCalledWith(
        target,
        expect.objectContaining({ keepLayout: true, newTab: false }),
      );
      expect(mockBridgeResolveOpenTarget).not.toHaveBeenCalled();
    } finally {
      mockFileSurfaceCall.mockReset();
      mockFileSurfacePrepareView.mockReset();
      mockBridgeResolveOpenTarget.mockReset();
    }
  });

  it.each([
    { slot: "right-pane", team: undefined },
    { slot: "file-nav", team: "default" },
  ])("keeps the general opener for other targets: %j", async ({ slot, team }) => {
    const { frameFixture } = await import("../../../../tests/support/fixtures/frame");
    const frame = frameFixture({
      documentPath: "/private/panes/custom.html",
      contentRoot: "/private/content",
      slot,
    });
    frame.grant.team = { org: "acme", team: "child" };
    const target = { path: "/org/acme/default/content/other.md", org: "acme", team: "default" };
    mockBridgeResolveOpenTarget.mockResolvedValue(target);
    await expect(
      runBridgeCommand(
        "openFile",
        { path: target.path, ...(team ? { team } : {}) },
        { frame, docPath: frame.documentPath },
      ),
    ).resolves.toEqual(target);
    expect(mockBridgeResolveOpenTarget).toHaveBeenCalledWith(frame.documentPath, target.path, team);
    expect(mockFileSurfaceCall).not.toHaveBeenCalled();
  });

  const target = {
    path: "/h/acme/web/content/e/WEB-1.md",
    org: "acme",
    team: "web",
    contentRoot: "/h/acme/web/content",
  };

  it("resolves through Rust against the host's page path, then opens the resolved file", async () => {
    mockBridgeResolveOpenTarget.mockResolvedValue(target);
    const navigate = vi.fn();
    await expect(
      runBridgeCommand(
        "openFile",
        { path: "e/WEB-1.md", team: "web", newTab: true },
        { docPath: "/h/acme/backend/content/board.html", navigate },
      ),
    ).resolves.toEqual({ path: target.path, org: "acme", team: "web" });
    expect(mockBridgeResolveOpenTarget).toHaveBeenCalledWith(
      "/h/acme/backend/content/board.html",
      "e/WEB-1.md",
      "web",
    );
    // A page's open is not the user's: it keeps full-screen CLI on.
    expect(mockOpenFileInTeam).toHaveBeenCalledWith(target, {
      newTab: true,
      navigate,
      switchTeam: expect.any(Function),
      keepLayout: true,
    });
  });

  it("maps preview false to a pinned native tab, and preview true to preview", async () => {
    mockBridgeResolveOpenTarget.mockResolvedValue(target);
    for (const preview of [true, false]) {
      await runBridgeCommand("openFile", { path: "e/WEB-1.md", preview });
      expect(mockOpenFileInTeam).toHaveBeenLastCalledWith(
        target,
        expect.objectContaining({ newTab: !preview }),
      );
    }
  });

  it("opens nothing when Rust refuses a path outside every team content root", async () => {
    mockBridgeResolveOpenTarget.mockRejectedValue(
      new Error("refused: /etc/hosts is not inside a known content root"),
    );
    await expect(runBridgeCommand("openFile", { path: "/etc/hosts" })).rejects.toThrow(/^refused:/);
    await expect(runBridgeCommand("openFile", { path: " " })).rejects.toThrow(/^openFile:/);
    await expect(runBridgeCommand("openFile", { path: "a.md", team: "" })).rejects.toThrow(
      /^openFile:/,
    );
    expect(mockOpenFileInTeam).not.toHaveBeenCalled();
  });
});

describe("routeToContext action verb", () => {
  const args = {
    codeRepoPath: "/repo",
    codeRepoSlug: "org/repo",
    logicalName: "doc.md",
    layer: "team",
  };

  it("maps onto route_file_to_context with the same args and resolves an affirmative outcome", async () => {
    mockRouteFileToContext.mockResolvedValue({ outcome: "routed" });
    await expect(runBridgeCommand("routeToContext", args)).resolves.toEqual({ outcome: "routed" });
    expect(mockRouteFileToContext).toHaveBeenCalledWith(
      "/repo",
      "org/repo",
      "doc.md",
      "team",
      undefined,
      undefined,
    );
  });

  it("treats already-routed / already-tracked as success (idempotent)", async () => {
    mockRouteFileToContext.mockResolvedValue({ outcome: "alreadyTracked" });
    await expect(runBridgeCommand("routeToContext", args)).resolves.toEqual({
      outcome: "alreadyTracked",
    });
  });

  it("REJECTS denied: a non-affirmative outcome (unmanaged/refused → no silent success)", async () => {
    // The Rust command RESOLVES with a no-route outcome for a non-managed
    // source (SLW-FX-13 skip); the bridge must translate that to a deny so the
    // dashboard button doesn't falsely look successful.
    mockRouteFileToContext.mockResolvedValue({ outcome: "refused" });
    await expect(runBridgeCommand("routeToContext", args)).rejects.toThrow(
      "denied: route not applied (refused)",
    );
    mockRouteFileToContext.mockResolvedValue({ outcome: "notProjectable" });
    await expect(runBridgeCommand("routeToContext", args)).rejects.toThrow(
      "denied: route not applied (notProjectable)",
    );
  });

  it("rejects a malformed args shape", async () => {
    await expect(runBridgeCommand("routeToContext", { codeRepoPath: 1 })).rejects.toThrow(
      /must be strings/,
    );
  });
});

describe("draft action verb", () => {
  it("creates a seeded managed doc derived from `from` and returns its path", async () => {
    mockCreateFile.mockResolvedValue(undefined);
    const res = (await runBridgeCommand("draft", { level: "story", from: "/ctx/epics/e.md" })) as {
      path: string;
    };
    expect(res.path).toMatch(/^\/ctx\/epics\/story-draft-[a-z0-9]+\.md$/);
    expect(mockCreateFile).toHaveBeenCalledTimes(1);
    const [path, content] = mockCreateFile.mock.calls[0];
    expect(path).toBe(res.path);
    expect(content).toContain("type: story");
    expect(content).toContain("status: todo");
    expect(content).toContain("e.md"); // backlink to `from`
  });

  it("surfaces the Rust context-path rejection verbatim (unmanaged from)", async () => {
    mockCreateFile.mockRejectedValue(new Error("path is not within the context repo"));
    await expect(runBridgeCommand("draft", { level: "story", from: "/etc/x.md" })).rejects.toThrow(
      /not within the context repo/,
    );
  });

  it("rejects a malformed args shape", async () => {
    await expect(runBridgeCommand("draft", { level: "", from: "/a" })).rejects.toThrow(
      /non-empty strings/,
    );
  });
});

describe("spawn-class verbs (SW-DX-7)", () => {
  afterEach(() => setDashboardLaunchController(null));

  it("openWorkspace forwards to the registered dashboard-launch controller", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1" }));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(runBridgeCommand("openWorkspace", { storyId: "SW-DX-7" })).resolves.toEqual({
      sessionId: "s1",
    });
    // SW-DX-15: the legacy `{ storyId }` wire form normalizes to a story target.
    expect(openWorkspace).toHaveBeenCalledWith({ target: { kind: "story", storyId: "SW-DX-7" } });
  });

  it("openWorkspace accepts a native story path target", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await runBridgeCommand("openWorkspace", { target: { kind: "story", path: "/team/story.md" } });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "story", storyPath: "/team/story.md" },
      headed: true,
    });
  });

  it("openWorkspace forwards the named agent and refuses a launch profile or pack", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1" }));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await runBridgeCommand("openWorkspace", {
      target: { kind: "story", storyId: "SW-DX-7", agent: "builtin/codex" },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "story", storyId: "SW-DX-7", agent: "builtin/codex" },
      headed: true,
    });
    // A blank agent is rejected loudly (never a silently-dropped default).
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-DX-7", agent: " " },
      }),
    ).rejects.toThrow("agent, if present, must be a non-empty string");
    openWorkspace.mockClear();
    for (const target of [
      { kind: "story", storyId: "SW-DX-7", profile: "codex-default" },
      { kind: "story", storyId: "SW-DX-7", packId: "codex" },
      { kind: "dir", path: "/d", profile: "dev" },
      { kind: "branch", repo: "o/r", branch: "b", packId: "codex" },
      { kind: "branch", repo: "o/r", branch: "b", profile: "dev" },
    ]) {
      await expect(runBridgeCommand("openWorkspace", { target, headed: false })).rejects.toThrow(
        /^invalid: openWorkspace launches an agent; pass \{ agent \}/,
      );
    }
    expect(openWorkspace).not.toHaveBeenCalled();
  });

  it("openWorkspace forwards an epic target and defaults headed", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await runBridgeCommand("openWorkspace", { target: { kind: "epic" } });
    expect(openWorkspace).toHaveBeenCalledWith({ target: { kind: "epic" }, headed: true });
  });

  it("SW-DX-16: openWorkspace forwards an EXPLICIT {team, epic} epic target", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await runBridgeCommand("openWorkspace", {
      target: { kind: "epic", team: "acme:default", epic: "epics/x" },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "epic", team: "acme:default", epic: "epics/x" },
      headed: true,
    });
  });

  it("openWorkspace forwards a branch target and defaults headed (SW-DX-18)", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s2" }));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await runBridgeCommand("openWorkspace", {
      target: { kind: "branch", repo: "acme/api", branch: "feat/x" },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "branch", repo: "acme/api", branch: "feat/x" },
      headed: true,
    });
  });

  it("openWorkspace forwards an agent-named review branch contract", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s2", promptDelivered: true }));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await runBridgeCommand("openWorkspace", {
      target: {
        kind: "branch",
        repo: "acme/api",
        baseRepo: "acme/api",
        pullNumber: "42",
        branch: "feat/x",
        agent: "reviewer",
        linkedStoryId: "SW-OR-8",
      },
      headed: false,
      reviewPrompt: "/bmad-code-review",
      operationId: "review-op-1",
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: {
        kind: "branch",
        repo: "acme/api",
        baseRepo: "acme/api",
        pullNumber: 42,
        branch: "feat/x",
        agent: "reviewer",
        linkedStoryId: "SW-OR-8",
      },
      headed: false,
      reviewPrompt: "/bmad-code-review",
      operationId: "review-op-1",
    });
  });

  it("reviewPrompt requires an explicitly headless branch target", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "branch", repo: "acme/api", branch: "feat/x" },
        reviewPrompt: "/bmad-code-review",
      }),
    ).rejects.toThrow("requires headed:false");
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-OR-8" },
        headed: false,
        reviewPrompt: "/bmad-code-review",
      }),
    ).rejects.toThrow("only supported for branch targets");
    expect(openWorkspace).not.toHaveBeenCalled();
  });

  it("openWorkspace forwards a dir target and defaults headed (SW-GW-2)", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await runBridgeCommand("openWorkspace", {
      target: { kind: "dir", path: "/Users/me/proj" },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "dir", path: "/Users/me/proj" },
      headed: true,
    });
  });

  // SW-DX-17e: sessionMode threading + the story-only + enum guards. Reuses the
  // `stubController` helper defined in the SW-GW-9 block below (deferred `it`
  // callbacks resolve it after the describe body evaluates).
  it("SW-DX-17e: forwards sessionMode:'new' on a story target", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1" }));
    stubController(openWorkspace);
    await runBridgeCommand("openWorkspace", {
      target: { kind: "story", storyId: "SW-DX-7" },
      sessionMode: "new",
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "story", storyId: "SW-DX-7" },
      headed: true,
      sessionMode: "new",
    });
  });

  it("SW-DX-17e: forwards sessionMode via the legacy { storyId } wire form", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1" }));
    stubController(openWorkspace);
    await runBridgeCommand("openWorkspace", { storyId: "SW-DX-7", sessionMode: "new" });
    expect(openWorkspace).toHaveBeenCalledWith(
      expect.objectContaining({
        target: { kind: "story", storyId: "SW-DX-7" },
        sessionMode: "new",
      }),
    );
  });

  it("SW-DX-17e: rejects sessionMode:'new' on a NON-story target before the controller (story-only)", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    stubController(openWorkspace);
    await expect(
      runBridgeCommand("openWorkspace", { target: { kind: "epic" }, sessionMode: "new" }),
    ).rejects.toThrow(/sessionMode:'new' is only supported for story targets/);
    expect(openWorkspace).not.toHaveBeenCalled();
  });

  it("forwards a story target's machineId to the controller", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1" }));
    stubController(openWorkspace);
    await runBridgeCommand("openWorkspace", {
      target: { kind: "story", storyId: "SW-9", machineId: "m1" },
    });
    expect(openWorkspace).toHaveBeenCalledWith(
      expect.objectContaining({ target: { kind: "story", storyId: "SW-9", machineId: "m1" } }),
    );
  });

  it("rejects machineId on a non-story target and a blank machineId", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    stubController(openWorkspace);
    await expect(
      runBridgeCommand("openWorkspace", { target: { kind: "epic", machineId: "m1" } }),
    ).rejects.toThrow(/machineId is only supported for story targets/);
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "S", machineId: " " },
      }),
    ).rejects.toThrow(/machineId, if present, must be a non-empty string/);
    expect(openWorkspace).not.toHaveBeenCalled();
  });

  it("SW-DX-17e: allows sessionMode:'focus' on any kind (the universal default, no fork)", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    stubController(openWorkspace);
    await runBridgeCommand("openWorkspace", { target: { kind: "epic" }, sessionMode: "focus" });
    // focus is a no-op for non-story kinds — it forwards without a sessionMode fork.
    expect(openWorkspace).toHaveBeenCalledWith({ target: { kind: "epic" }, headed: true });
  });

  it("SW-DX-17e: rejects an invalid sessionMode value before the controller", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    stubController(openWorkspace);
    await expect(
      runBridgeCommand("openWorkspace", { storyId: "SW-DX-7", sessionMode: "clone" }),
    ).rejects.toThrow(/sessionMode, if present, must be 'new' or 'focus'/);
    expect(openWorkspace).not.toHaveBeenCalled();
  });

  it("SW-DX-17e (r1): the legacy { storyId } form rejects a '#'-bearing id (guard symmetry)", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    stubController(openWorkspace);
    await expect(
      runBridgeCommand("openWorkspace", { storyId: "SW-A#attempt-2", sessionMode: "new" }),
    ).rejects.toThrow(/must not contain '#'/);
    expect(openWorkspace).not.toHaveBeenCalled();
  });

  it("SW-GW-2: a dir target with no { path } rejects a shape error before the controller", async () => {
    const openWorkspace = vi.fn(async () => ({}));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(runBridgeCommand("openWorkspace", { target: { kind: "dir" } })).rejects.toThrow(
      /dir target needs a non-empty \{ path \}/,
    );
    expect(openWorkspace).not.toHaveBeenCalled();
  });

  it("SW-DX-16: a half-specified epic target ({team} only) rejects, no own-epic fallback", async () => {
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(
      runBridgeCommand("openWorkspace", { target: { kind: "epic", team: "acme:default" } }),
    ).rejects.toThrow(/both \{ team, epic \} or neither/);
    await expect(
      runBridgeCommand("openWorkspace", { target: { kind: "epic", epic: "epics/x" } }),
    ).rejects.toThrow(/both \{ team, epic \} or neither/);
  });

  it("openWorkspace rejects a branch target missing repo/branch (shape guard)", async () => {
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(
      runBridgeCommand("openWorkspace", { target: { kind: "branch", branch: "feat/x" } }),
    ).rejects.toThrow(/branch target needs a non-empty \{ repo \}/);
    await expect(
      runBridgeCommand("openWorkspace", { target: { kind: "branch", repo: "acme/api" } }),
    ).rejects.toThrow(/branch target needs a non-empty \{ branch \}/);
  });

  // ── SW-GW-9: initialPrompt threading + story-only reject ────────────────────
  // A tiny controller stub so the seed-threading assertions stay focused on the arg
  // the bridge builds (not the 7-field controller boilerplate).
  const stubController = (openWorkspace: DashboardLaunchController["openWorkspace"]) =>
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });

  it("SW-GW-9: threads initialPrompt into an explicit story target", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1", promptDelivered: true }));
    stubController(openWorkspace);
    await runBridgeCommand("openWorkspace", {
      target: { kind: "story", storyId: "GW-9" },
      initialPrompt: "/bmad-dev-story x",
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "story", storyId: "GW-9" },
      headed: true,
      initialPrompt: "/bmad-dev-story x",
    });
  });

  it("SW-GW-9: threads initialPrompt into the legacy { storyId } form too", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1", promptDelivered: true }));
    stubController(openWorkspace);
    await runBridgeCommand("openWorkspace", { storyId: "GW-9", initialPrompt: "/x" });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "story", storyId: "GW-9" },
      initialPrompt: "/x",
    });
  });

  it("SW-GW-9 (AC-8): an empty/whitespace initialPrompt normalizes to undefined at the boundary", async () => {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1" }));
    stubController(openWorkspace);
    await runBridgeCommand("openWorkspace", {
      target: { kind: "story", storyId: "GW-9" },
      initialPrompt: "   ",
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "story", storyId: "GW-9" },
      headed: true,
      initialPrompt: undefined,
    });
  });

  it.each(["epic", "branch", "dir"] as const)(
    "SW-GW-9 (AC-2): initialPrompt on a %s target rejects before the controller (never a silent drop)",
    async (kind) => {
      const openWorkspace = vi.fn(async () => ({}));
      stubController(openWorkspace);
      const target =
        kind === "branch"
          ? { kind, repo: "acme/api", branch: "feat/x" }
          : kind === "dir"
            ? { kind, path: "/Users/me/proj" }
            : { kind };
      await expect(
        runBridgeCommand("openWorkspace", { target, initialPrompt: "/x" }),
      ).rejects.toThrow(
        new RegExp(`initialPrompt is only supported for story targets, not ${kind}`),
      );
      expect(openWorkspace).not.toHaveBeenCalled();
    },
  );

  // SW-GW-3: the OBSERVE read verb validates {storyId} then forwards to the
  // controller (which owns the trusted viewer-store + liveness probe).
  it("getWorkspaceStatus validates {storyId} and forwards to the controller", async () => {
    const getWorkspaceStatus = vi.fn(async () => ({ exists: true, live: true, sessionId: "s1" }));
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus,
      closeWorkspace: vi.fn(),
    });
    // Shape guard: a missing/empty storyId rejects before the controller.
    await expect(runBridgeCommand("getWorkspaceStatus", {})).rejects.toThrow(
      /\{storyId\} must be a non-empty string/,
    );
    expect(getWorkspaceStatus).not.toHaveBeenCalled();
    // Valid: forwards the storyId and returns the controller's {exists,live,sessionId}.
    await expect(runBridgeCommand("getWorkspaceStatus", { storyId: "GW-3" })).resolves.toEqual({
      exists: true,
      live: true,
      sessionId: "s1",
    });
    expect(getWorkspaceStatus).toHaveBeenCalledWith("GW-3");
  });

  it("getWorkspaceStatus rejects `unavailable` when no dashboard controller is mounted", async () => {
    setDashboardLaunchController(null);
    await expect(runBridgeCommand("getWorkspaceStatus", { storyId: "GW-3" })).rejects.toThrow(
      /unavailable: no active dashboard workspace context/,
    );
  });

  it("runPrompt forwards {target, prompt}, rebuilding the validated compound handle", async () => {
    const runPrompt = vi.fn(async () => undefined);
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt,
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    // A valid named-session handle round-trips through the shared slug validator.
    await runBridgeCommand("runPrompt", {
      target: "SW-DX-7#architecture-review",
      prompt: "/bmad-code-review {storyPath}",
    });
    expect(runPrompt).toHaveBeenCalledWith({
      target: "SW-DX-7#architecture-review",
      prompt: "/bmad-code-review {storyPath}",
    });
  });

  it("runPrompt rejects a blank/whitespace-only prompt (no stray Enter to the PTY)", async () => {
    const runPrompt = vi.fn(async () => undefined);
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt,
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(
      runBridgeCommand("runPrompt", { target: "SW-DX-7", prompt: "   \n\t" }),
    ).rejects.toThrow(/prompt.*non-empty/);
    expect(runPrompt).not.toHaveBeenCalled();
  });

  it("runPrompt rejects a target with no story id before the '#' (symmetric w/ openWorkspace)", async () => {
    const runPrompt = vi.fn(async () => undefined);
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt,
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(
      runBridgeCommand("runPrompt", { target: "#architecture-review", prompt: "hi" }),
    ).rejects.toThrow(/must include a story id/);
    await expect(runBridgeCommand("runPrompt", { target: "#", prompt: "hi" })).rejects.toThrow(
      /must include a story id/,
    );
    expect(runPrompt).not.toHaveBeenCalled();
  });

  // SW-DX-10: the send-half verb forwards the bounded {target, text} to the
  // controller (which resolves the session trusted-side); a raw session id in
  // the doc args never reaches the controller.
  it("sendToSession forwards the bounded {target, text} to the controller", async () => {
    const sendToSession = vi.fn(async () => ({ delivered: true }));
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession,
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(
      runBridgeCommand("sendToSession", { target: "SW-DX-7", text: "focus on AC-3" }),
    ).resolves.toEqual({ delivered: true });
    expect(sendToSession).toHaveBeenCalledWith({ target: "SW-DX-7", text: "focus on AC-3" });
  });

  it("sendToSession rejects when no dashboard controller is mounted", async () => {
    setDashboardLaunchController(null);
    await expect(
      runBridgeCommand("sendToSession", { target: "SW-DX-7", text: "hi" }),
    ).rejects.toThrow(/unavailable: no active dashboard/);
  });

  // SW-DX-11 (AC-1): readWorkspace forwards ONLY the caller's `path` to the
  // controller — the doc never supplies the doc path or the extension whitelist
  // (those are the controller's trusted scope).
  it("readWorkspace forwards the caller path to the controller", async () => {
    const readWorkspace = vi.fn(async () => ({ content: "hi" }));
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace,
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(runBridgeCommand("readWorkspace", { path: "./DEMO-1.md" })).resolves.toEqual({
      content: "hi",
    });
    expect(readWorkspace).toHaveBeenCalledWith("./DEMO-1.md", undefined);
  });

  it("readWorkspace rejects a non-string path before forwarding", async () => {
    const readWorkspace = vi.fn();
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace,
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(runBridgeCommand("readWorkspace", {})).rejects.toThrow(/path/);
    expect(readWorkspace).not.toHaveBeenCalled();
  });

  it("readWorkspace rejects when no dashboard controller is mounted", async () => {
    setDashboardLaunchController(null);
    await expect(runBridgeCommand("readWorkspace", { path: "./x.md" })).rejects.toThrow(
      /unavailable: no active dashboard/,
    );
  });

  // SW-DX-11 (AC-5): runSkill forwards {skill, args} to the run_skill command
  // (a direct headless call like analyze — NOT the launch controller).
  it("runSkill forwards {skill, args} to the run_skill command", async () => {
    mockRunSkill.mockResolvedValueOnce("done");
    await expect(
      runBridgeCommand("runSkill", { skill: "sync-story", args: ["DEMO-1893"] }),
    ).resolves.toBe("done");
    expect(mockRunSkill).toHaveBeenCalledWith("sync-story", ["DEMO-1893"]);
  });

  it("runSkill coerces a missing/invalid args list to [] and rejects a non-string skill", async () => {
    mockRunSkill.mockResolvedValueOnce("ok");
    await runBridgeCommand("runSkill", { skill: "x" });
    expect(mockRunSkill).toHaveBeenCalledWith("x", []);
    await expect(runBridgeCommand("runSkill", { args: ["a"] })).rejects.toThrow(/skill/);
  });

  // SW-DX-11 §5: getEpicHandle returns the controller's containing-epic handle
  // (read-only doc discovery), or null when no dashboard controller is mounted.
  it("getEpicHandle returns the controller's epic handle, null when unmounted", async () => {
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => "/ctx/epic/README.md"),
      getDocDir: vi.fn(() => "/ctx/epic"),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(runBridgeCommand("getEpicHandle", null)).resolves.toBe("/ctx/epic/README.md");

    setDashboardLaunchController(null);
    await expect(runBridgeCommand("getEpicHandle", null)).resolves.toBeNull();
  });

  // SW-DX-14: getDocDir returns the controller's own directory (read-only doc
  // discovery for absolute sibling paths), or null when no controller is mounted.
  it("getDocDir returns the controller's own dir, null when unmounted", async () => {
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => "/ctx/epic"),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(runBridgeCommand("getDocDir", null)).resolves.toBe("/ctx/epic");

    setDashboardLaunchController(null);
    await expect(runBridgeCommand("getDocDir", null)).resolves.toBeNull();
  });

  it("sendToSession validates {target, text} before forwarding", async () => {
    setDashboardLaunchController({
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    await expect(runBridgeCommand("sendToSession", { text: "hi" })).rejects.toThrow(/target/);
    await expect(runBridgeCommand("sendToSession", { target: "x" })).rejects.toThrow(/text/);
    await expect(runBridgeCommand("sendToSession", { target: "x", text: "" })).rejects.toThrow(
      /text/,
    );
  });

  it("rejects `unavailable` when no dashboard controller is mounted", async () => {
    setDashboardLaunchController(null);
    await expect(runBridgeCommand("openWorkspace", { storyId: "SW-DX-7" })).rejects.toThrow(
      /unavailable: no active dashboard/,
    );
    await expect(
      runBridgeCommand("runPrompt", { target: "SW-DX-7", prompt: "hi" }),
    ).rejects.toThrow(/unavailable: no active dashboard/);
  });

  it("validates args shape before forwarding", async () => {
    await expect(runBridgeCommand("openWorkspace", {})).rejects.toThrow(/storyId/);
    await expect(runBridgeCommand("runPrompt", { target: "x" })).rejects.toThrow(/prompt/);
    await expect(runBridgeCommand("runPrompt", { prompt: "x" })).rejects.toThrow(/target/);
    // A `#`-bearing storyId part and an ill-formed label are rejected via the shared validator.
    await expect(runBridgeCommand("runPrompt", { target: "a#b#c", prompt: "x" })).rejects.toThrow(
      /session must be a lowercase slug/,
    );
  });
});

// ── SW-GW-6: openWorkspace story session/title/agent validation ──────────────
describe("SW-GW-6 openWorkspace story named-session fields", () => {
  afterEach(() => setDashboardLaunchController(null));

  function mountSpy() {
    const openWorkspace = vi.fn(async () => ({ sessionId: "s1" }));
    setDashboardLaunchController({
      openWorkspace,
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    });
    return openWorkspace;
  }

  it("forwards a valid slug session + title + agent on the story target", async () => {
    const openWorkspace = mountSpy();
    await runBridgeCommand("openWorkspace", {
      target: {
        kind: "story",
        storyId: "SW-GW-6",
        session: "arch-review",
        title: "Architecture Review",
        agent: "big-model",
      },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: {
        kind: "story",
        storyId: "SW-GW-6",
        session: "arch-review",
        title: "Architecture Review",
        agent: "big-model",
      },
      headed: true,
    });
  });

  it("omits a blank/whitespace session (treated as the main session)", async () => {
    const openWorkspace = mountSpy();
    await runBridgeCommand("openWorkspace", {
      target: { kind: "story", storyId: "SW-GW-6", session: "   " },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "story", storyId: "SW-GW-6" },
      headed: true,
    });
  });

  it("rejects a non-slug session (uppercase / spaces)", async () => {
    mountSpy();
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-GW-6", session: "Arch Review" },
      }),
    ).rejects.toThrow(/lowercase slug/);
  });

  it("rejects a session that contains the reserved '#' separator", async () => {
    mountSpy();
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-GW-6", session: "arch#review" },
      }),
    ).rejects.toThrow(/lowercase slug/);
  });

  it("rejects a present-but-empty title / agent", async () => {
    mountSpy();
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-GW-6", title: "" },
      }),
    ).rejects.toThrow(/title/);
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-GW-6", agent: "  " },
      }),
    ).rejects.toThrow(/agent/);
  });

  // ── the agent an open launches: epic headless-only; dir and branch always ─────
  it("forwards an agent on a HEADLESS epic target (headed:false)", async () => {
    const openWorkspace = mountSpy();
    await runBridgeCommand("openWorkspace", {
      target: { kind: "epic", agent: "builtin/codex" },
      headed: false,
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "epic", agent: "builtin/codex" },
      headed: false,
    });
  });

  it("rejects an agent on a HEADED epic (a bare terminal launches no agent)", async () => {
    const openWorkspace = mountSpy();
    await expect(
      runBridgeCommand("openWorkspace", { target: { kind: "epic", agent: "builtin/codex" } }),
    ).rejects.toThrow(/agent on an epic open requires headed:false/);
    expect(openWorkspace).not.toHaveBeenCalled();
  });

  it("forwards an agent on a BRANCH target (headed or background — always an agent)", async () => {
    const openWorkspace = mountSpy();
    await runBridgeCommand("openWorkspace", {
      target: { kind: "branch", repo: "o/r", branch: "b", agent: "reviewer" },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "branch", repo: "o/r", branch: "b", agent: "reviewer" },
      headed: true,
    });
  });

  it("forwards an agent on a dir target, headed or headless", async () => {
    const openWorkspace = mountSpy();
    await runBridgeCommand("openWorkspace", {
      target: { kind: "dir", path: "/some/dir", agent: "triage" },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: { kind: "dir", path: "/some/dir", agent: "triage" },
      headed: true,
    });
  });

  // ── review-loop hardening: slug hygiene + storyId '#' guard + trimming ──────
  it("rejects a slug with a leading/trailing/doubled hyphen (handle/title divergence)", async () => {
    mountSpy();
    for (const bad of ["arch-", "-arch", "a--b"]) {
      await expect(
        runBridgeCommand("openWorkspace", {
          target: { kind: "story", storyId: "SW-GW-6", session: bad },
        }),
      ).rejects.toThrow(/lowercase slug/);
    }
  });

  it("rejects an over-long session slug (bounded key)", async () => {
    mountSpy();
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-GW-6", session: "a".repeat(65) },
      }),
    ).rejects.toThrow(/lowercase slug/);
  });

  it("rejects a storyId containing the reserved '#' separator", async () => {
    mountSpy();
    await expect(
      runBridgeCommand("openWorkspace", {
        target: { kind: "story", storyId: "SW-GW-6#arch-review" },
      }),
    ).rejects.toThrow(/must not contain '#'/);
  });

  it("trims surrounding whitespace off title / agent before forwarding", async () => {
    const openWorkspace = mountSpy();
    await runBridgeCommand("openWorkspace", {
      target: {
        kind: "story",
        storyId: "SW-GW-6",
        session: "arch-review",
        title: "  Architecture Review  ",
        agent: "  big-model  ",
      },
    });
    expect(openWorkspace).toHaveBeenCalledWith({
      target: {
        kind: "story",
        storyId: "SW-GW-6",
        session: "arch-review",
        title: "Architecture Review",
        agent: "big-model",
      },
      headed: true,
    });
  });
});

// The agents read model: each bridge call hands back exactly what its Rust command
// (the same read model the MCP tool uses) returns, acting on the doc's own team.
describe("agents read model verbs", () => {
  const DOC = { docPath: "/acme/default/content/console.html" };
  const TEAM = { org: "acme", team: "default" };
  beforeEach(() => {
    mockBridgeDocContext.mockResolvedValue({ org: "acme", team: "default" });
  });

  it("react passes the human's intent to the engine and validates its shape", async () => {
    const reacted = { status: "reacted", messageId: 9, emoji: "👍🏽", outcome: "added" };
    mockReactAsHuman.mockResolvedValue(reacted);
    await expect(
      runBridgeCommand("react", { messageId: 9, emoji: "👍🏽", remove: false }, DOC),
    ).resolves.toEqual(reacted);
    expect(mockReactAsHuman).toHaveBeenLastCalledWith(TEAM, 9, "👍🏽", false);
    await runBridgeCommand("react", { messageId: 9, emoji: "🏳️‍🌈" }, DOC);
    expect(mockReactAsHuman).toHaveBeenLastCalledWith(TEAM, 9, "🏳️‍🌈", null);
    await expect(runBridgeCommand("react", { messageId: 9, emoji: " " }, DOC)).rejects.toThrow(
      /react: \{emoji\}/,
    );
    await expect(
      runBridgeCommand("react", { messageId: 9, emoji: "👍", remove: "yes" }, DOC),
    ).rejects.toThrow(/react: remove/);
    await expect(runBridgeCommand("react", { messageId: "9", emoji: "👍" }, DOC)).rejects.toThrow(
      /messageId/,
    );
    expect(METADATA_ACTION_VERBS).toContain("react");
  });

  it("agents carries each definition's avatar for faces of ended sessions", async () => {
    mockAgentDefinitionsPrepare.mockResolvedValue({
      topic: "agentDefinitions",
      viewKey: "all",
      rev: 1,
      snapshot: [
        {
          id: "a",
          rowType: "definition",
          row: { key: "scout", name: "Scout", avatar: "builtin-funny:coffee-bot" },
        },
        { id: "b", rowType: "definition", row: { key: "plain", name: "Plain" } },
      ],
    });
    const rows = (await runBridgeCommand("agents", {}, DOC)) as Array<Record<string, unknown>>;
    expect(rows.map((r) => [r.key, r.avatar])).toEqual([
      ["scout", "builtin-funny:coffee-bot"],
      ["plain", null],
    ]);
  });

  it("markUnread uses the scoped human-mail batch without an extra grant", async () => {
    mockMarkInboxMessagesUnread.mockResolvedValue(2);
    await expect(runBridgeCommand("markUnread", { messageIds: [3, 4] }, DOC)).resolves.toEqual({
      marked: 2,
    });
    expect(mockMarkInboxMessagesUnread).toHaveBeenCalledWith(TEAM, [3, 4]);
  });

  it("sessions is the list_sessions row, unchanged", async () => {
    const rows = [
      {
        handle: "SW-1",
        displayName: "SW-1 dev",
        state: "idle",
        launchedBy: null,
        unread: 2,
        oldestUnreadAt: "2026-10-04T00:00:00.000Z",
        machine: "This Mac",
        agent: "builtin/claude",
      },
    ];
    mockAgentReadSessions.mockResolvedValue(rows);
    // The roster the host watches for changes is already loaded.
    const { useAgentListProjectionStore } =
      await import("@/features/viewer/lib/agent-list-projection");
    useAgentListProjectionStore.setState({ sessions: [] });
    await expect(
      runBridgeCommand("sessions", undefined, {
        ...DOC,
        liveFrame: async () => ({ subscribe: async () => ({ snapshot: rows }) }) as never,
      }),
    ).resolves.toEqual(rows);
  });

  it("lets a page write the dock's folds but never its selected row", async () => {
    const writeUi = vi.fn(async () => ({}));
    const ctx = { liveFrame: async () => ({ writeUi }) as never };
    await runBridgeCommand("setUiState", { topic: "ui:dock", patch: { folds: { a: true } } }, ctx);
    expect(writeUi).toHaveBeenCalledWith("ui:dock", { folds: { a: true } }, undefined);
    await expect(
      runBridgeCommand("setUiState", { topic: "ui:dock", patch: { selected: "sid" } }, ctx),
    ).rejects.toThrow("call gotoAgent");
    expect(writeUi).toHaveBeenCalledTimes(1);
  });

  it("uses the projection's team metadata without a roster join", async () => {
    const rows = [
      { id: "Remote", handle: "Remote", state: "idle", team: { org: "acme", team: "remote" } },
    ];
    const subscribe = vi.fn(async () => ({ snapshot: rows }));
    mockAgentReadSessions.mockResolvedValue(rows);
    await expect(
      runBridgeCommand("sessions", undefined, {
        liveFrame: async () => ({ subscribe }) as never,
      }),
    ).resolves.toEqual(rows);
    expect(subscribe).toHaveBeenCalledTimes(1);
  });

  it("joins cached inbox fields without reading or moving the frame's inbox window", async () => {
    const row = { id: "a", handle: "a", team: TEAM, state: "idle" };
    const subscribe = vi.fn(async (topic: string) => ({
      snapshot: topic === "sessions" ? [row] : [{ id: "message:older", rowType: "message" }],
    }));
    mockAgentReadSessions.mockResolvedValue([
      { ...row, unread: 3, groupUnread: 2 },
      { id: "other", handle: "other", team: { org: "other", team: "other" }, unread: 7 },
    ]);
    await expect(
      runBridgeCommand("sessions", undefined, {
        ...DOC,
        liveFrame: async () => ({ subscribe }) as never,
      }),
    ).resolves.toEqual([{ ...row, unread: 3, groupUnread: 2 }]);
    expect(subscribe).toHaveBeenCalledOnce();
    expect(subscribe).toHaveBeenCalledWith("sessions");
  });

  it("agent({key}) is the agent_get data with its instances", async () => {
    const got = { id: "acme/default/reviewer", instances: [{ handle: "H1" }] };
    mockAgentReadAgent.mockResolvedValue(got);
    await expect(runBridgeCommand("agent", { key: " acme/default/reviewer " })).resolves.toBe(got);
    expect(mockAgentReadAgent).toHaveBeenCalledWith("acme/default/reviewer", undefined, undefined);
    await expect(runBridgeCommand("agent", {})).rejects.toThrow(/\{key\}/);
  });

  it("inbox passes only the filters given, in the doc's team", async () => {
    const view = { messages: [{ messageId: 1 }], humanUnread: 1 };
    mockAgentReadInbox.mockResolvedValue(view);
    await expect(runBridgeCommand("inbox", undefined, DOC)).resolves.toBe(view);
    expect(mockAgentReadInbox).toHaveBeenLastCalledWith(TEAM, {});
    await runBridgeCommand(
      "inbox",
      { scope: "all", agentId: "a", sessionId: "S1", role: "foreman", limit: 5.7 },
      DOC,
    );
    expect(mockAgentReadInbox).toHaveBeenLastCalledWith(TEAM, {
      scope: "all",
      agentId: "a",
      sessionId: "S1",
      role: "foreman",
      limit: 5,
    });
    await expect(runBridgeCommand("inbox", { scope: "mine" }, DOC)).rejects.toThrow(/scope/);
    await expect(runBridgeCommand("inbox", { limit: -1 }, DOC)).rejects.toThrow(/limit/);
    // SW-AGR-18: what waits on the human, or what agents report.
    await runBridgeCommand("inbox", { kind: "needs-you" }, DOC);
    expect(mockAgentReadInbox).toHaveBeenLastCalledWith(TEAM, { kind: "needs-you" });
    await expect(runBridgeCommand("inbox", { kind: "urgent" }, DOC)).rejects.toThrow(
      /kind, if present, must be 'needs-you' or 'update'/,
    );
    expect(READ_VERBS).toContain("inbox");
  });

  it("validates conversation paging arguments using the inbox error style", async () => {
    await runBridgeCommand("inbox", { conversation: " Mia ", before: 51, limit: 50 }, DOC);
    expect(mockAgentReadInbox).toHaveBeenLastCalledWith(TEAM, {
      conversation: "Mia",
      before: 51,
      limit: 50,
    });
    for (const conversation of ["", "  ", 4, false, null])
      await expect(runBridgeCommand("inbox", { conversation }, DOC)).rejects.toThrow(
        /inbox: conversation must be a non-empty string/,
      );
    for (const before of [0, -1, 1.5, "2", false, null])
      await expect(runBridgeCommand("inbox", { before }, DOC)).rejects.toThrow(
        /inbox: before must be a positive integer/,
      );
  });

  it("roles, updates and humanStatus are the role_state, list_agent_updates and get_human_status shapes", async () => {
    const roles = { roles: [{ role: "foreman", deliveries: [] }] };
    mockAgentReadRoles.mockResolvedValue(roles);
    await expect(runBridgeCommand("roles", { role: "foreman" }, DOC)).resolves.toBe(roles);
    expect(mockAgentReadRoles).toHaveBeenCalledWith(
      TEAM,
      "foreman",
      undefined,
      undefined,
      DOC.docPath,
    );

    const updates = [{ id: 1, text: "halfway" }];
    mockListAgentUpdates.mockResolvedValue(updates);
    await expect(runBridgeCommand("updates", { limit: 3 }, DOC)).resolves.toEqual({ updates });
    expect(mockListAgentUpdates).toHaveBeenCalledWith(TEAM, 3);

    const status = { status: "away", updatedAtMs: 5 };
    mockGetHumanStatus.mockResolvedValue(status);
    await expect(runBridgeCommand("humanStatus", undefined, DOC)).resolves.toBe(status);
    expect(mockGetHumanStatus).toHaveBeenCalledWith(TEAM);
  });

  it("the human's writes go through the action classes and the inbox commands", async () => {
    expect(SEND_ACTION_VERBS).toEqual(expect.arrayContaining(["sendAsHuman", "replyAsHuman"]));
    expect(METADATA_ACTION_VERBS).toEqual(expect.arrayContaining(["markRead", "setHumanStatus"]));

    mockSendInboxAsHuman.mockResolvedValue({ status: "sent", messageId: 9, deliveryId: 4 });
    await expect(
      runBridgeCommand(
        "sendAsHuman",
        {
          to: " acme/default/reviewer ",
          text: "please look",
          contentType: "html",
          onClick: { type: "show-message" },
          attachFile: "docs/plan.md",
          inReplyTo: 7,
        },
        DOC,
      ),
    ).resolves.toEqual({ status: "sent", messageId: 9, deliveryId: 4 });
    expect(mockSendInboxAsHuman).toHaveBeenCalledWith(
      TEAM,
      "acme/default/reviewer",
      "please look",
      "html",
      { type: "show-message" },
      "docs/plan.md",
      null,
      null,
      7,
    );
    await expect(runBridgeCommand("sendAsHuman", { to: "H1", text: " " }, DOC)).rejects.toThrow(
      /text/,
    );
    // replyTo: where replies go instead of to the human (email Reply-To).
    await runBridgeCommand(
      "sendAsHuman",
      { to: "acme/default/worker", text: "brief", replyTo: " role:manager " },
      DOC,
    );
    expect(mockSendInboxAsHuman).toHaveBeenLastCalledWith(
      TEAM,
      "acme/default/worker",
      "brief",
      null,
      null,
      null,
      "role:manager",
      null,
      null,
    );
    // summary: the one-line preview lists show for the message (an html body's
    // markup never stands in for it).
    await runBridgeCommand(
      "sendAsHuman",
      {
        to: "H1",
        text: "<html><body>…</body></html>",
        contentType: "html",
        summary: "Deploy: green",
      },
      DOC,
    );
    expect(mockSendInboxAsHuman).toHaveBeenLastCalledWith(
      TEAM,
      "H1",
      "<html><body>…</body></html>",
      "html",
      null,
      null,
      null,
      "Deploy: green",
      null,
    );
    await expect(
      runBridgeCommand("sendAsHuman", { to: "H1", text: "hi", summary: 3 }, DOC),
    ).rejects.toThrow(/summary/);

    // postMessage carries the summary to the role send, and refuses a non-string.
    mockSendRoleMessage.mockResolvedValue({ status: "sent", messageId: 11, deliveryId: 5 });
    await runBridgeCommand(
      "postMessage",
      { to: "triage", text: "<html>…</html>", contentType: "html", summary: "Nightly: green" },
      DOC,
    );
    expect(mockSendRoleMessage).toHaveBeenLastCalledWith(
      TEAM,
      DOC.docPath,
      "triage",
      "<html>…</html>",
      expect.any(String),
      null,
      null,
      "html",
      "Nightly: green",
    );
    await expect(
      runBridgeCommand("postMessage", { to: "triage", text: "hi", summary: {} }, DOC),
    ).rejects.toThrow(/summary/);
    await expect(
      runBridgeCommand("sendAsHuman", { to: "H1", text: "hi", replyTo: 7 }, DOC),
    ).rejects.toThrow(/replyTo/);

    mockReplyAsHuman.mockResolvedValue({ status: "sent", messageId: 10 });
    await runBridgeCommand("replyAsHuman", { messageId: 9, text: "yes" }, DOC);
    expect(mockReplyAsHuman).toHaveBeenCalledWith(TEAM, 9, "yes", null, null);
    await expect(
      runBridgeCommand("replyAsHuman", { messageId: "9", text: "yes" }, DOC),
    ).rejects.toThrow(/messageId/);

    mockMarkInboxMessagesRead.mockResolvedValue(1);
    await expect(runBridgeCommand("markRead", { messageIds: [3, 4] }, DOC)).resolves.toEqual({
      marked: 1,
    });
    expect(mockMarkInboxMessagesRead.mock.calls).toEqual([[TEAM, [3, 4]]]);
    await expect(runBridgeCommand("markRead", { messageIds: [] }, DOC)).rejects.toThrow(
      /messageIds/,
    );

    mockSetHumanStatus.mockResolvedValue({ status: "busy", note: "deep work", updatedAtMs: 1 });
    await runBridgeCommand("setHumanStatus", { status: "busy", note: "deep work" }, DOC);
    expect(mockSetHumanStatus).toHaveBeenCalledWith(TEAM, "busy", "deep work");
    await expect(runBridgeCommand("setHumanStatus", { status: "gone" }, DOC)).rejects.toThrow(
      /status/,
    );
  });

  it("archives in one call, returns changed rows and propagates store errors", async () => {
    mockMarkInboxMessagesArchived.mockResolvedValueOnce(1).mockResolvedValueOnce(0);
    await expect(runBridgeCommand("archive", { messageIds: [3, 4, 3] }, DOC)).resolves.toEqual({
      archived: 1,
    });
    expect(mockMarkInboxMessagesArchived).toHaveBeenCalledExactlyOnceWith(TEAM, [3, 4, 3]);
    await expect(runBridgeCommand("archive", { messageIds: [5] }, DOC)).resolves.toEqual({
      archived: 0,
    });
    mockMarkInboxMessagesArchived.mockRejectedValueOnce("store offline");
    await expect(runBridgeCommand("archive", { messageIds: [3] }, DOC)).rejects.toBe(
      "store offline",
    );
    for (const messageIds of [[], [0], [1.5], ["3"]]) {
      await expect(runBridgeCommand("archive", { messageIds }, DOC)).rejects.toThrow(
        "archive: {messageIds} must be a non-empty array of message ids",
      );
    }
  });

  it("opens only live html mail to the human without reading it", async () => {
    const row = { messageId: 42, from: "H1", to: "human", toHuman: true, contentType: "html" };
    mockAgentReadInbox.mockResolvedValue({ messages: [row], humanUnread: 1 });
    await expect(runBridgeCommand("openMessage", { messageId: 42 }, DOC)).resolves.toEqual({
      opened: true,
      messageId: 42,
    });
    expect(mockAgentReadInbox).toHaveBeenLastCalledWith(TEAM, { scope: "to-human" });
    expect(mockOpenMessageTab).toHaveBeenCalledExactlyOnceWith(TEAM, 42, "H1", {
      keepLayout: true,
    });
    expect(mockMarkInboxMessagesRead).not.toHaveBeenCalled();
    mockOpenMessageTab.mockClear();
    for (const [change, error] of [
      [
        { contentType: "markdown" },
        "openMessage: message 42 is not an html message; render it in the page",
      ],
      [{ from: "human" }, "openMessage: message 42 is from the human; render it in the page"],
      [{ to: "H2", toHuman: false }, "openMessage: no message 42 for the human in this team"],
    ] as const) {
      mockAgentReadInbox.mockResolvedValue({ messages: [{ ...row, ...change }], humanUnread: 1 });
      await expect(runBridgeCommand("openMessage", { messageId: 42 }, DOC)).rejects.toThrow(error);
    }
    mockAgentReadInbox.mockResolvedValue({ messages: [], humanUnread: 0 });
    await expect(runBridgeCommand("openMessage", { messageId: 42 }, DOC)).rejects.toThrow(
      "openMessage: no message 42 for the human in this team",
    );
    await expect(runBridgeCommand("openMessage", { messageId: 0 }, DOC)).rejects.toThrow(
      "openMessage: {messageId}",
    );
    expect(mockOpenMessageTab).not.toHaveBeenCalled();
  });

  it("a team-scoped call with no team rejects loudly", async () => {
    mockBridgeDocContext.mockResolvedValue(null);
    const orgStore = await import("@/shared/stores/org-store");
    const spy = vi
      .spyOn(orgStore.useOrgStore, "getState")
      .mockReturnValue({ activeOrgSlug: null, activeTeamName: null } as never);
    await expect(runBridgeCommand("roles", undefined, DOC)).rejects.toThrow(
      /roles: no active team/,
    );
    spy.mockRestore();
  });
});

// Routines, runs, worktrees and machines: each bridge call hands back exactly what
// its Rust command returns — the same function its MCP twin calls — and machines
// act on the doc's own team.
describe("routine, run, worktree and machine read verbs", () => {
  const DOC = { docPath: "/acme/default/content/ops.html" };
  beforeEach(() => {
    mockBridgeDocContext.mockResolvedValue({ org: "acme", team: "default" });
  });

  it("are audit-exempt reads", () => {
    for (const verb of [
      "routines",
      "routine",
      "routineRuns",
      "routineRun",
      "runs",
      "run",
      "runEvidence",
      "runWorkProduct",
      "worktrees",
      "machines",
      "sessionRead",
      "whyLine",
      "setup",
      "storyChanges",
      "storyTotals",
      "sessionUsage",
      "modelRates",
    ]) {
      expect(READ_VERBS).toContain(verb);
      expect(ACTION_VERBS).not.toContain(verb);
    }
  });

  it("routines, routine, routineRuns and routineRun are the routine tools' data, unchanged", async () => {
    const list = [{ id: "acme/default/digest", name: "digest" }];
    mockRoutineReadList.mockResolvedValue(list);
    await expect(runBridgeCommand("routines", undefined, DOC)).resolves.toBe(list);

    const got = { id: "acme/default/digest", runs: [] };
    mockRoutineReadGet.mockResolvedValue(got);
    await expect(runBridgeCommand("routine", { key: " acme/default/digest " })).resolves.toBe(got);
    expect(mockRoutineReadGet).toHaveBeenCalledWith("acme/default/digest", undefined, undefined);
    await runBridgeCommand("routine", { id: "acme/default/digest" });
    expect(mockRoutineReadGet).toHaveBeenLastCalledWith(
      "acme/default/digest",
      undefined,
      undefined,
    );
    await expect(runBridgeCommand("routine", {})).rejects.toThrow(/\{key\}/);

    const page = { runs: [{ run_id: "r1", phase: "succeeded" }], next_before: null };
    mockRoutineReadRuns.mockResolvedValue(page);
    await expect(
      runBridgeCommand("routineRuns", { key: "acme/default/digest", limit: 5.5, before: "r9" }),
    ).resolves.toBe(page);
    expect(mockRoutineReadRuns).toHaveBeenCalledWith(
      "acme/default/digest",
      5,
      "r9",
      undefined,
      undefined,
    );
    await runBridgeCommand("routineRuns", { key: "acme/default/digest", before: null });
    expect(mockRoutineReadRuns).toHaveBeenLastCalledWith(
      "acme/default/digest",
      undefined,
      undefined,
      undefined,
      undefined,
    );
    await expect(runBridgeCommand("routineRuns", { key: "k", limit: 0 })).rejects.toThrow(/limit/);

    const run = { run_id: "r1", routine_id: "acme/default/digest", routine_name: "digest" };
    mockRoutineReadRun.mockResolvedValue(run);
    await expect(runBridgeCommand("routineRun", { runId: "r1" })).resolves.toBe(run);
    expect(mockRoutineReadRun).toHaveBeenCalledWith("r1", undefined, undefined);
    await expect(runBridgeCommand("routineRun", { runId: " " })).rejects.toThrow(/\{runId\}/);
  });

  it("runs and run are list_runs and read_run, unchanged", async () => {
    const rows = [{ outcome: { run_id: "x1" }, validated: "completed" }];
    mockRunReadList.mockResolvedValue(rows);
    await expect(runBridgeCommand("runs", undefined)).resolves.toEqual(rows);
    expect(mockRunReadList).toHaveBeenLastCalledWith(undefined);
    await runBridgeCommand("runs", { limit: 10 });
    expect(mockRunReadList).toHaveBeenLastCalledWith(10);

    const detail = { outcome: null, transcript: null, artifact: "out", artifactTruncated: false };
    mockRunRead.mockResolvedValue(detail);
    await expect(runBridgeCommand("run", { runId: "x1" })).resolves.toBe(detail);
    expect(mockRunRead).toHaveBeenLastCalledWith("x1", undefined);
    await runBridgeCommand("run", { runId: "x1", maxArtifactBytes: 1024.9 });
    expect(mockRunRead).toHaveBeenLastCalledWith("x1", 1024);
    await expect(runBridgeCommand("run", {})).rejects.toThrow(/\{runId\}/);
    await expect(runBridgeCommand("run", { runId: "x1", maxArtifactBytes: -1 })).rejects.toThrow(
      /maxArtifactBytes/,
    );
  });

  it("runEvidence and runWorkProduct are read_run_evidence and read_run_work_product, unchanged", async () => {
    const evidence = {
      header: { runId: "x1", storyId: "SW-1", trigger: { kind: "preEnvelope" } },
      run: { runId: "x1", correlationId: "x1", steps: [] },
      perNodeResults: [],
      authority: "currentStoreFallback",
    };
    mockRunReadEvidence.mockResolvedValue(evidence);
    await expect(runBridgeCommand("runEvidence", { runId: " x1 " })).resolves.toBe(evidence);
    expect(mockRunReadEvidence).toHaveBeenLastCalledWith("x1", undefined);
    await runBridgeCommand("runEvidence", { runId: "x1", storyId: " SW-1 " });
    expect(mockRunReadEvidence).toHaveBeenLastCalledWith("x1", "SW-1");
    await runBridgeCommand("runEvidence", { run_id: "x1", story_id: "SW-2" });
    expect(mockRunReadEvidence).toHaveBeenLastCalledWith("x1", "SW-2");
    await runBridgeCommand("runEvidence", { runId: "x1", storyId: null });
    expect(mockRunReadEvidence).toHaveBeenLastCalledWith("x1", undefined);
    mockRunReadEvidence.mockResolvedValue(null);
    await expect(runBridgeCommand("runEvidence", { runId: "gone" })).resolves.toBeNull();
    await expect(runBridgeCommand("runEvidence", {})).rejects.toThrow(/\{runId\}/);
    await expect(runBridgeCommand("runEvidence", { runId: "x1", storyId: " " })).rejects.toThrow(
      /storyId/,
    );

    mockRunReadWorkProduct.mockResolvedValue({ branch: "feat/x" });
    await expect(runBridgeCommand("runWorkProduct", { runId: "x1" })).resolves.toEqual({
      branch: "feat/x",
    });
    expect(mockRunReadWorkProduct).toHaveBeenLastCalledWith("x1");
    mockRunReadWorkProduct.mockResolvedValue(null);
    await expect(runBridgeCommand("runWorkProduct", { run_id: "x2" })).resolves.toBeNull();
    expect(mockRunReadWorkProduct).toHaveBeenLastCalledWith("x2");
    await expect(runBridgeCommand("runWorkProduct", undefined)).rejects.toThrow(/runWorkProduct/);
  });

  it("setup is describe_setup's attribution answer, passed through unchanged", async () => {
    const answer = { attribution: { status: { kind: "active" }, capture: { lastSweepAt: null } } };
    mockSetupStatus.mockResolvedValue(answer);
    await expect(runBridgeCommand("setup", undefined, DOC)).resolves.toBe(answer);
    expect(mockSetupStatus).toHaveBeenCalledWith();
  });

  it("worktrees is list_worktrees; machines is list_machines for the doc's own team", async () => {
    const listing = { rows: [{ kind: "story", storyId: "SW-1" }], reaped: 0 };
    mockListManagedWorktrees.mockResolvedValue(listing);
    await expect(runBridgeCommand("worktrees", undefined, DOC)).resolves.toBe(listing);

    const machines = [{ machineId: "local", label: "This Mac" }];
    mockListMachines.mockResolvedValue(machines);
    await expect(runBridgeCommand("machines", undefined, DOC)).resolves.toBe(machines);
    expect(mockBridgeDocContext).toHaveBeenCalledWith(DOC.docPath);
    expect(mockListMachines).toHaveBeenCalledWith("acme", "default", true);

    // A message doc acts in its own message's team.
    await runBridgeCommand("machines", undefined, {
      message: { org: "beta", team: "ops", messageId: 1 },
    } as never);
    expect(mockListMachines).toHaveBeenLastCalledWith("beta", "ops", true);
  });

  it("modelRates is session_read_model_rates, unchanged", async () => {
    const rates = [
      {
        modelPrefix: "gpt-6-sol",
        effectiveFrom: "2026-10-17",
        inputPerMtok: 2,
        outputPerMtok: 10,
        cacheReadPerMtok: null,
        long: null,
        source: "set by the user",
      },
    ];
    mockSessionReadModelRates.mockResolvedValue(rates);
    await expect(runBridgeCommand("modelRates", undefined)).resolves.toBe(rates);
    expect(mockSessionReadModelRates).toHaveBeenLastCalledWith();
  });

  it("whyLine forwards committed ranges and frame scope unchanged", async () => {
    const result = { ranges: [{ start: 1, end: 2, basis: "exact" }], sessions: {} };
    mockWhyLine.mockResolvedValue(result);
    const args = {
      repo: "acme/api",
      path: 'src/ ünicode "name".rs ',
      start: 1,
      end: 2,
      rev: "main",
    };
    const { frameFixture } = await import("../../../../tests/support/fixtures/frame");
    const frame = frameFixture({
      documentPath: DOC.docPath,
      contentRoot: "/acme/default/content",
      slot: "right-pane",
    });
    await expect(runBridgeCommand("whyLine", args, { ...DOC, frame })).resolves.toBe(result);
    expect(mockWhyLine).toHaveBeenCalledWith(args, frame.frameId, DOC.docPath);
    await expect(runBridgeCommand("whyLine", { ...args, end: 2001, start: 1 })).resolves.toBe(
      result,
    );
  });
  it.each([
    { path: "a.txt", start: 1, end: 2 },
    { repo: 7, path: "a.txt", start: 1, end: 2 },
    { repo: "acme/api", path: 7, start: 1, end: 2 },
    { repo: "acme/api", path: "a.txt", start: "1", end: 2 },
    { repo: "acme/api", path: "a.txt", start: 0, end: 2 },
    { repo: "acme/api", path: "a.txt", start: 3, end: 2 },
    { repo: "acme/api", path: "a.txt", start: 1, end: 2002 },
    { repo: "acme/api", path: "a.txt", start: 1.5, end: 2 },
    { repo: "acme/api", path: "a.txt", start: 1, end: 2, rev: null },
    { repo: "acme/api", path: "a.txt", start: 1, end: 2, rev: "--all" },
    { repo: "acme/api", path: "a\0.txt", start: 1, end: 2 },
    { repo: "acme/api", path: "a.txt", start: 1, end: 2, extra: true },
  ])("whyLine rejects invalid input %#", async (args) => {
    await expect(runBridgeCommand("whyLine", args)).rejects.toThrow(/whyLine/);
    expect(mockWhyLine).not.toHaveBeenCalled();
  });
  it("sessionRead forwards the identical joined result and conversation selectors", async () => {
    const result = { session: { sessionId: "tab" }, cost: { totalUsd: null } };
    mockSessionRead.mockResolvedValue(result);
    await expect(
      runBridgeCommand("sessionRead", {
        sessionId: " native ",
        include: ["conversation"],
        cursor: 2,
        limit: 5,
      }),
    ).resolves.toBe(result);
    expect(mockSessionRead).toHaveBeenLastCalledWith(
      { sessionId: "native", include: ["conversation"], cursor: 2, limit: 5 },
      undefined,
      undefined,
    );
    await runBridgeCommand("sessionRead", { sessionId: "tab", include: ["conversation"], turn: 1 });
    await runBridgeCommand("sessionRead", {
      sessionId: "tab",
      include: ["conversation"],
      toolUseId: "call-1",
    });
  });

  it("sessionRead takes include turns with the conversation selectors, and never both includes", async () => {
    const result = { session: { sessionId: "tab" }, turns: { turns: [] } };
    mockSessionRead.mockResolvedValue(result);
    await expect(
      runBridgeCommand("sessionRead", {
        sessionId: "tab",
        include: ["turns"],
        cursor: 20,
        limit: 20,
      }),
    ).resolves.toBe(result);
    expect(mockSessionRead).toHaveBeenLastCalledWith(
      { sessionId: "tab", include: ["turns"], cursor: 20, limit: 20 },
      undefined,
      undefined,
    );
    mockSessionRead.mockClear();
    await expect(
      runBridgeCommand("sessionRead", { sessionId: "tab", include: ["conversation", "turns"] }),
    ).rejects.toThrow(/not both/);
    expect(mockSessionRead).not.toHaveBeenCalled();
  });

  it.each([
    { field: "cursor", extra: { cursor: null, turn: 3 }, expected: { turn: 3 } },
    { field: "limit", extra: { limit: null }, expected: {} },
    { field: "turn", extra: { turn: null, cursor: 2 }, expected: { cursor: 2 } },
    { field: "toolUseId", extra: { toolUseId: null }, expected: {} },
  ])(
    "sessionRead omits nullable $field before validation and forwarding",
    async ({ extra, expected }) => {
      const result = { session: { sessionId: "tab" } };
      mockSessionRead.mockResolvedValue(result);
      await expect(
        runBridgeCommand("sessionRead", { sessionId: "tab", include: ["conversation"], ...extra }),
      ).resolves.toBe(result);
      expect(mockSessionRead).toHaveBeenLastCalledWith(
        { sessionId: "tab", include: ["conversation"], ...expected },
        undefined,
        undefined,
      );
    },
  );

  it.each([
    { case: "missing sessionId", args: {} },
    { case: "empty sessionId", args: { sessionId: " " } },
    { case: "unknown field", args: { sessionId: "tab", extra: true } },
    { case: "bad include", args: { sessionId: "tab", include: ["other"] } },
    { case: "missing include", args: { sessionId: "tab", cursor: 0 } },
    {
      case: "exclusive selectors",
      args: { sessionId: "tab", include: ["conversation"], turn: 1, cursor: 2 },
    },
    { case: "negative cursor", args: { sessionId: "tab", include: ["conversation"], cursor: -1 } },
    {
      case: "oversized turn",
      args: { sessionId: "tab", include: ["conversation"], turn: 2 ** 32 },
    },
    { case: "empty tool id", args: { sessionId: "tab", include: ["conversation"], toolUseId: "" } },
    { case: "zero limit", args: { sessionId: "tab", include: ["conversation"], limit: 0 } },
    { case: "oversized limit", args: { sessionId: "tab", include: ["conversation"], limit: 201 } },
    { case: "fractional limit", args: { sessionId: "tab", include: ["conversation"], limit: 1.5 } },
  ])("sessionRead rejects $case", async ({ args }) => {
    await expect(runBridgeCommand("sessionRead", args)).rejects.toThrow(/sessionRead/);
  });

  it("sessionUsage is session_read_usage with its window and filters, and rejects a bad window", async () => {
    const report = {
      sessions: [],
      totals: {
        tokens: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
        costUsd: 0,
        pricedSessions: 0,
        unpricedSessions: 0,
      },
    };
    mockSessionReadUsage.mockResolvedValue(report);
    await expect(
      runBridgeCommand("sessionUsage", {
        sinceMs: 1000,
        untilMs: 2000,
        cwd: " /work/a ",
        limit: 5,
      }),
    ).resolves.toBe(report);
    expect(mockSessionReadUsage).toHaveBeenLastCalledWith(
      {
        sinceMs: 1000,
        untilMs: 2000,
        sessionId: undefined,
        cwd: "/work/a",
        story: undefined,
        limit: 5,
      },
      undefined,
      undefined,
    );
    // A page's nextCursor continues the walk: it reaches the command with the same filter.
    mockSessionReadUsage.mockResolvedValueOnce({ ...report, nextCursor: "u:1:2:3:s1" });
    const page1 = (await runBridgeCommand("sessionUsage", { limit: 1 })) as { nextCursor: string };
    mockSessionReadUsage.mockResolvedValueOnce(report);
    await expect(
      runBridgeCommand("sessionUsage", { limit: 1, cursor: page1.nextCursor }),
    ).resolves.toBe(report);
    expect(mockSessionReadUsage).toHaveBeenLastCalledWith(
      expect.objectContaining({ limit: 1, cursor: "u:1:2:3:s1" }),
      undefined,
      undefined,
    );
    await expect(runBridgeCommand("sessionUsage", { cursor: 5 })).rejects.toThrow(/cursor/);
    await runBridgeCommand("sessionUsage", { story: " SV-5 " });
    expect(mockSessionReadUsage).toHaveBeenLastCalledWith(
      expect.objectContaining({ story: "SV-5" }),
      undefined,
      undefined,
    );
    // The limit must be a whole number the store can hold, and the window must run forwards.
    await expect(runBridgeCommand("sessionUsage", { limit: 2.5 })).rejects.toThrow(/whole number/);
    await expect(runBridgeCommand("sessionUsage", { limit: 0 })).rejects.toThrow(/whole number/);
    await expect(runBridgeCommand("sessionUsage", { limit: -1 })).rejects.toThrow(/whole number/);
    await expect(runBridgeCommand("sessionUsage", { limit: 2 ** 53 })).rejects.toThrow(/too large/);
    await expect(runBridgeCommand("sessionUsage", { untilMs: "x" })).rejects.toThrow(/untilMs/);
    await expect(
      runBridgeCommand("setModelRate", {
        modelPrefix: "claude-x",
        effectiveFrom: "2026-10-16",
        inputPerMtok: Infinity,
        outputPerMtok: 1,
      }),
    ).rejects.toThrow(/^invalid: /);
    await expect(
      runBridgeCommand("sessionUsage", { sinceMs: 2000, untilMs: 1000 }),
    ).rejects.toThrow(/before untilMs/);
    await expect(runBridgeCommand("sessionUsage", {})).resolves.toBe(report);
    await expect(runBridgeCommand("sessionUsage", { sinceMs: "today" })).rejects.toThrow(/sinceMs/);
  });

  it("setModelRate forwards its four fields in order and refuses a non-number rate", async () => {
    mockSessionSetModelRate.mockResolvedValue(3);
    await expect(
      runBridgeCommand("setModelRate", {
        modelPrefix: "claude-haiku-5-5",
        effectiveFrom: "2026-10-16",
        inputPerMtok: 2,
        outputPerMtok: 10,
      }),
    ).resolves.toBe(3);
    expect(mockSessionSetModelRate).toHaveBeenLastCalledWith({
      modelPrefix: "claude-haiku-5-5",
      effectiveFrom: "2026-10-16",
      inputPerMtok: 2,
      outputPerMtok: 10,
    });
    await expect(
      runBridgeCommand("setModelRate", {
        modelPrefix: "x",
        effectiveFrom: "2026-10-16",
        inputPerMtok: "2",
        outputPerMtok: 10,
      }),
    ).rejects.toThrow(/^invalid: /);
  });

  it("setModelRate forwards a cache-read price and a long prompt's prices, and refuses bad ones", async () => {
    mockSessionSetModelRate.mockResolvedValue(1);
    const base = {
      modelPrefix: "gpt-6-sol",
      effectiveFrom: "2026-10-17",
      inputPerMtok: 2,
      outputPerMtok: 10,
    };
    await expect(
      runBridgeCommand("setModelRate", {
        ...base,
        cacheReadPerMtok: 0.1,
        long: { fromTokens: 272000, inputPerMtok: 4, outputPerMtok: 15, cacheReadPerMtok: 0.2 },
      }),
    ).resolves.toBe(1);
    expect(mockSessionSetModelRate).toHaveBeenLastCalledWith({
      ...base,
      cacheReadPerMtok: 0.1,
      long: { fromTokens: 272000, inputPerMtok: 4, outputPerMtok: 15, cacheReadPerMtok: 0.2 },
    });
    await runBridgeCommand("setModelRate", {
      ...base,
      long: { fromTokens: 100000, inputPerMtok: 1, outputPerMtok: 5 },
    });
    expect(mockSessionSetModelRate).toHaveBeenLastCalledWith({
      ...base,
      long: { fromTokens: 100000, inputPerMtok: 1, outputPerMtok: 5 },
    });
    await expect(
      runBridgeCommand("setModelRate", { ...base, cacheReadPerMtok: "0.1" }),
    ).rejects.toThrow(/^invalid: /);
    await expect(
      runBridgeCommand("setModelRate", { ...base, cacheReadPerMtok: Infinity }),
    ).rejects.toThrow(/^invalid: /);
    await expect(
      runBridgeCommand("setModelRate", {
        ...base,
        long: { fromTokens: 2 ** 53, inputPerMtok: 1, outputPerMtok: 5 },
      }),
    ).rejects.toThrow(/^invalid: /);
    await expect(
      runBridgeCommand("setModelRate", {
        ...base,
        long: { fromTokens: 1.5, inputPerMtok: 1, outputPerMtok: 5 },
      }),
    ).rejects.toThrow(/^invalid: /);
    await expect(
      runBridgeCommand("setModelRate", { ...base, long: { fromTokens: 10 } }),
    ).rejects.toThrow(/^invalid: /);
    await expect(runBridgeCommand("setModelRate", { ...base, long: [1] })).rejects.toThrow(
      /^invalid: /,
    );
    // A threshold of zero or below is refused before it reaches the app, as an `invalid:` refusal.
    for (const fromTokens of [0, -5]) {
      await expect(
        runBridgeCommand("setModelRate", {
          ...base,
          long: { fromTokens, inputPerMtok: 1, outputPerMtok: 5 },
        }),
      ).rejects.toThrow(/^invalid: /);
    }
    // A misspelled key is refused, not dropped.
    await expect(
      runBridgeCommand("setModelRate", {
        ...base,
        long: { fromTokens: 10, inputPerMtok: 1, outputPerMtok: 5, cacheReadPerMTok: 0.2 },
      }),
    ).rejects.toThrow(/^invalid: .*cacheReadPerMTok/);
    for (const cacheReadPerMtok of [Infinity, "0.2"]) {
      await expect(
        runBridgeCommand("setModelRate", {
          ...base,
          long: { fromTokens: 10, inputPerMtok: 1, outputPerMtok: 5, cacheReadPerMtok },
        }),
      ).rejects.toThrow(/^invalid: /);
    }
    await expect(
      runBridgeCommand("setModelRate", { ...base, cacheReadPerMTok: 0.1 }),
    ).rejects.toThrow(/^invalid: setModelRate has unknown keys: cacheReadPerMTok/);
    // null is the same as leaving a price out.
    await runBridgeCommand("setModelRate", { ...base, cacheReadPerMtok: null, long: null });
    expect(mockSessionSetModelRate).toHaveBeenLastCalledWith(base);
    await runBridgeCommand("setModelRate", {
      ...base,
      long: { fromTokens: 10, inputPerMtok: 1, outputPerMtok: 5, cacheReadPerMtok: null },
    });
    expect(mockSessionSetModelRate).toHaveBeenLastCalledWith({
      ...base,
      long: { fromTokens: 10, inputPerMtok: 1, outputPerMtok: 5 },
    });
  });

  it("machines with no team rejects loudly", async () => {
    mockBridgeDocContext.mockResolvedValue(null);
    const orgStore = await import("@/shared/stores/org-store");
    const spy = vi
      .spyOn(orgStore.useOrgStore, "getState")
      .mockReturnValue({ activeOrgSlug: null, activeTeamName: null } as never);
    await expect(runBridgeCommand("machines", undefined, DOC)).rejects.toThrow(
      /machines: no active team/,
    );
    spy.mockRestore();
  });
});

// SW-AGR-3: a session timer set from the app acts as the person ("set by you"),
// so a dashboard can never reach the person's timer commands.
describe("session timers stay out of the dashboard bridge", () => {
  it("allowlists no timer verb and imports no timer command", async () => {
    const { HTML_BRIDGE_ALLOWLIST } = await import("./html-bridge");
    const verbs = Object.keys(HTML_BRIDGE_ALLOWLIST).map((verb) => verb.toLowerCase());
    for (const verb of verbs) {
      expect(verb).not.toMatch(/remind|timer|wakeup/);
    }
    const source = readFileSync(resolve(__dirname, "html-bridge.ts"), "utf8");
    for (const command of ["remindSession", "sessionTimers", "cancelSessionTimer"]) {
      expect(source).not.toContain(command);
    }
  });
});

describe("human-inbox surfaces", () => {
  const ctx = {
    message: { org: "acme", team: "default", messageId: 1, sender: "H1" },
  };
  const other = { org: "acme", team: "support" };

  it("anchors a human-inbox attachment in its selected org and team", async () => {
    const { useOrgStore } = await import("@/shared/stores/org-store");
    const original = useOrgStore.getState();
    const state = vi.spyOn(useOrgStore, "getState").mockReturnValue({
      ...original,
      orgs: [
        ...original.orgs,
        { slug: "other/org", org_root: "/org/other", teams: [{ name: "empty" }] },
      ],
    } as never);
    try {
      mockBridgeResolveOpenTarget.mockResolvedValue({
        org: "other/org",
        team: "empty",
        path: "/org/other/empty/content/notes.md",
        contentRoot: "/org/other/empty/content",
      });
      const args = { org: "other/org", team: "empty", path: "notes.md" };
      await runBridgeCommand("openFile", args, ctx);
      expect(mockBridgeResolveOpenTarget).toHaveBeenLastCalledWith(
        "/org/other/empty/content/inbox.html",
        "notes.md",
        "empty",
      );
      await expect(runBridgeCommand("openFile", args, {})).resolves.toBeDefined();
    } finally {
      state.mockRestore();
    }
  });

  it("scopes the recipient role directory through the same trusted team gate", async () => {
    mockAgentReadRoles.mockResolvedValue({ roles: [] });
    await runBridgeCommand("roles", other, ctx);
    expect(mockAgentReadRoles).toHaveBeenLastCalledWith(
      other,
      undefined,
      undefined,
      undefined,
      undefined,
    );
    await expect(runBridgeCommand("roles", other, {})).resolves.toEqual({ roles: [] });
  });

  it("keeps the cached rollup count independent of a moving inbox window", async () => {
    const teams = [{ org: "acme", team: "default" }, other];
    const rollup = {
      messages: teams.map((team, i) => ({ id: i + 1, team, text: "mail" })),
      roles: [{ role: "reviewer", holder: null, pending: 2, note: "no live holder" }],
      updates: teams.map((team) => ({ team, text: "working" })),
      humanUnread: 2,
    };
    mockListInboxRollup.mockResolvedValue(rollup);
    const subscribe = vi.fn(async () => ({
      snapshot: [{ id: "message:older", rowType: "message" }],
    }));
    const frameContext = { ...ctx, liveFrame: async () => ({ subscribe }) as never };
    await runBridgeCommand("subscribe", { topic: "inbox", window: [0, 50] }, frameContext);
    await runBridgeCommand("subscribe", { topic: "inbox", window: [50, 100] }, frameContext);
    await expect(runBridgeCommand("inboxRollup", undefined, frameContext)).resolves.toEqual(rollup);
    expect(subscribe).toHaveBeenCalledTimes(2);
    expect(subscribe).toHaveBeenLastCalledWith("inbox", undefined, { window: [50, 100] });
    expect(mockListInboxRollup).toHaveBeenCalledExactlyOnceWith();
    expect(READ_VERBS).toContain("inboxRollup");
  });

  it("routes human replies, sends and read marks to the row's team", async () => {
    await runBridgeCommand("replyAsHuman", { ...other, messageId: 2, text: "yes" }, ctx);
    expect(mockReplyAsHuman.mock.calls[0][0]).toEqual(other);
    await runBridgeCommand("sendAsHuman", { ...other, to: "reviewer", text: "hello" }, ctx);
    expect(mockSendInboxAsHuman.mock.calls[0][0]).toEqual(other);
    await runBridgeCommand("markRead", { ...other, messageIds: [2, 3] }, ctx);
    expect(mockMarkInboxMessagesRead).toHaveBeenCalledExactlyOnceWith(other, [2, 3]);
    await runBridgeCommand("archive", { ...other, messageIds: [4] }, ctx);
    expect(mockMarkInboxMessagesArchived).toHaveBeenCalledExactlyOnceWith(other, [4]);
  });

  it("opens html mail from the row's team", async () => {
    const row = { messageId: 42, from: "H2", to: "human", toHuman: true, contentType: "html" };
    mockAgentReadInbox.mockResolvedValue({ messages: [row], humanUnread: 1 });
    await expect(
      runBridgeCommand("openMessage", { ...other, messageId: 42 }, ctx),
    ).resolves.toEqual({ opened: true, messageId: 42 });
    expect(mockAgentReadInbox).toHaveBeenLastCalledWith(other, { scope: "to-human" });
    expect(mockOpenMessageTab).toHaveBeenCalledExactlyOnceWith(other, 42, "H2", {
      keepLayout: true,
    });
  });

  it("keeps writes without an override pinned to the caller's team", async () => {
    await runBridgeCommand("replyAsHuman", { messageId: 2, text: "yes" }, ctx);
    expect(mockReplyAsHuman.mock.calls[0][0]).toEqual({ org: "acme", team: "default" });
    await runBridgeCommand("markRead", { messageIds: [2] }, ctx);
    expect(mockMarkInboxMessagesRead).toHaveBeenCalledWith({ org: "acme", team: "default" }, [2]);
  });

  it("allows an unmarked team dashboard to read all-team inbox under the open policy", async () => {
    const rollup = { messages: [], roles: [], updates: [], humanUnread: 0 };
    mockListInboxRollup.mockResolvedValue(rollup);
    await expect(
      runBridgeCommand("inboxRollup", undefined, { message: ctx.message }),
    ).resolves.toEqual(rollup);
    await runBridgeCommand("markRead", { ...other, messageIds: [2] }, { message: ctx.message });
    expect(mockMarkInboxMessagesRead).toHaveBeenCalledWith(other, [2]);
  });

  it.each([
    ["replyAsHuman", mockReplyAsHuman, { messageId: 2, text: "yes" }],
    ["sendAsHuman", mockSendInboxAsHuman, { to: "H2", text: "hello" }],
    ["markRead", mockMarkInboxMessagesRead, { messageIds: [2] }],
    ["archive", mockMarkInboxMessagesArchived, { messageIds: [2] }],
  ] as const)("names %s when the inbox host refuses an unsynced team", async (verb, host, args) => {
    const message = "team acme:missing is not synced on this machine";
    for (const error of [message, new Error(message)]) {
      host.mockRejectedValueOnce(error);
      await expect(
        runBridgeCommand(verb, { ...args, org: "acme", team: "missing" }, ctx),
      ).rejects.toThrow(`${verb}: ${message}`);
    }
    const pinnedError = new Error("inbox unavailable");
    host.mockRejectedValueOnce(pinnedError);
    await expect(runBridgeCommand(verb, args, ctx)).rejects.toBe(pinnedError);
  });

  it("requires org and team together on a marked surface", async () => {
    for (const override of [{ org: "acme" }, { team: "support" }]) {
      await expect(
        runBridgeCommand("markRead", { ...override, messageIds: [2] }, ctx),
      ).rejects.toThrow(/markRead:.*org.*team.*together/);
    }
    expect(mockMarkInboxMessagesRead).not.toHaveBeenCalled();
  });
});
