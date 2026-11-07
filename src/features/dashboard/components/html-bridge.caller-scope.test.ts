// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";
import { frameFixture } from "../../../../tests/support/fixtures/frame";

const calls = vi.hoisted(() => ({
  query: vi.fn(),
  open: vi.fn(),
  agent: vi.fn(),
  routines: vi.fn(),
  routine: vi.fn(),
  skill: vi.fn(),
  workspace: vi.fn(),
  history: vi.fn(),
  result: vi.fn(),
  roles: vi.fn(),
  rollup: vi.fn(),
  run: vi.fn(),
  usage: vi.fn(),
  metadata: vi.fn(),
  fields: vi.fn(),
  create: vi.fn(),
  route: vi.fn(),
}));
vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  queryContext: calls.query,
  bridgeResolveOpenTarget: calls.open,
  agentReadAgent: calls.agent,
  routineReadList: calls.routines,
  routineReadGet: calls.routine,
  runSkill: calls.skill,
  readWorkspaceFile: calls.workspace,
  routineReadRuns: calls.history,
  routineResult: calls.result,
  agentReadRoles: calls.roles,
  listInboxRollup: calls.rollup,
  routineReadRun: calls.run,
  sessionReadUsage: calls.usage,
  applyMetadataEdit: calls.metadata,
  updateFrontmatter: calls.fields,
  createFile: calls.create,
  routeFileToContext: calls.route,
}));
vi.mock("@/shared/lib/open-file-in-team", () => ({ openFileInTeam: vi.fn() }));
import { HTML_BRIDGE_ALLOWLIST as bridge, type BridgeContext } from "./html-bridge";

beforeEach(() => vi.clearAllMocks());

const page = "/org/parent/panes/shared.html";
const frame = frameFixture({
  slot: "right-pane",
  documentPath: page,
  contentRoot: "/org/child/content",
});
frame.grant.team = { org: "acme/ctx", team: "child" };
const ctx: BridgeContext = { frame, frameGrant: frame.grant, docPath: page };

it("retains the host frame on query, definition, routine and skill calls", async () => {
  const query = { where: { teams: ["parent"] } };
  await bridge.queryContext(query, ctx);
  expect(calls.query).toHaveBeenCalledWith(query, page, frame.frameId);
  await bridge.agent({ key: "acme/ctx/parent/reader" }, ctx);
  expect(calls.agent).toHaveBeenCalledWith("acme/ctx/parent/reader", frame.frameId, page);
  await bridge.routines({}, ctx);
  expect(calls.routines).toHaveBeenCalledWith(frame.frameId, page);
  await bridge.routine({ key: "acme/ctx/parent/job" }, ctx);
  expect(calls.routine).toHaveBeenCalledWith("acme/ctx/parent/job", frame.frameId, page);
  await bridge.runSkill({ skill: "check", args: [] }, ctx);
  expect(calls.skill).toHaveBeenCalledWith("check", [], frame.frameId, page);
});

it("forwards openFile admission failures unchanged and retains the child frame for shared files", async () => {
  calls.open.mockRejectedValueOnce(new Error("File not found"));
  await expect(bridge.openFile({ path: "secret.md", team: "parent" }, ctx)).rejects.toThrow(
    "File not found",
  );
  calls.open.mockRejectedValueOnce(new Error("File not found"));
  await expect(bridge.openFile({ path: "missing.md", team: "parent" }, ctx)).rejects.toThrow(
    "File not found",
  );
  calls.open.mockResolvedValueOnce({
    path: "/org/parent/content/shared.md",
    org: "acme/ctx",
    team: "parent",
    contentRoot: "/org/parent/content",
  });
  await bridge.openFile({ path: "shared.md", team: "parent" }, ctx);
  expect(calls.open).toHaveBeenLastCalledWith(page, "shared.md", "parent", frame.frameId);
});

it("passes host frame identity into story workspace resolution", async () => {
  const openWorkspace = vi.fn().mockResolvedValue({});
  await bridge.openWorkspace(
    { storyId: "P-SHARED" },
    { ...ctx, controller: { openWorkspace } as never },
  );
  expect(openWorkspace).toHaveBeenCalledWith(expect.objectContaining({ frameId: frame.frameId }));
});

it("passes the host frame through the real workspace controller into Tauri", async () => {
  const { makeDashboardLaunchController } = await import("./dashboard-launch-controller");
  calls.workspace.mockResolvedValue({ content: "shared" });
  const controller = makeDashboardLaunchController(page);
  await expect(
    bridge.readWorkspace({ path: "shared.md" }, { ...ctx, controller }),
  ).resolves.toEqual({ content: "shared" });
  expect(calls.workspace).toHaveBeenCalledWith(page, "shared.md", expect.any(Array), frame.frameId);
});

it("retains caller scope on routine history and detail reads", async () => {
  await bridge.routineRuns({ key: "acme/ctx/parent/job" }, ctx);
  expect(calls.history).toHaveBeenCalledWith(
    "acme/ctx/parent/job",
    undefined,
    undefined,
    frame.frameId,
    page,
  );
  await bridge.routineRun({ runId: "parent-run" }, ctx);
  expect(calls.run).toHaveBeenCalledWith("parent-run", frame.frameId, page);
});

it("retains host authority on routine result/status definition reads", async () => {
  for (const verb of ["routineResult", "routineStatus"] as const) {
    await bridge[verb]({ key: "acme/ctx/parent/job" }, ctx);
    expect(calls.result).toHaveBeenLastCalledWith("acme/ctx/parent/job", frame.frameId, page);
  }
});

it("retains host scope on role configuration enrichment", async () => {
  await bridge.roles({}, ctx);
  expect(calls.roles).toHaveBeenCalledWith(
    expect.any(Object),
    undefined,
    undefined,
    frame.frameId,
    page,
  );
  await bridge.inboxRollup(undefined, ctx);
  expect(calls.rollup).toHaveBeenCalledWith(frame.frameId, page);
});

it("passes trusted page authority into current story enrichment in usage reports", async () => {
  await bridge.sessionUsage({}, ctx);
  expect(calls.usage).toHaveBeenCalledWith(expect.any(Object), frame.frameId, page);
});

it("keeps the child host on mutation replies, including batch status and drafts", async () => {
  const path = "/org/parent/content/shared.md";
  await bridge.setStatus({ path, status: "done" }, ctx);
  expect(calls.metadata).toHaveBeenLastCalledWith(path, expect.any(Array), frame.frameId, page);
  await bridge.setStatus({ path: [path], status: "done" }, ctx);
  expect(calls.metadata).toHaveBeenLastCalledWith(path, expect.any(Array), frame.frameId, page);
  await bridge.setField({ path, fields: { status: "done" } }, ctx);
  expect(calls.fields).toHaveBeenCalledWith(path, { status: "done" }, frame.frameId, page);
  await bridge.draft({ from: path, level: "story" }, ctx);
  expect(calls.create).toHaveBeenCalledWith(
    expect.any(String),
    expect.any(String),
    frame.frameId,
    page,
  );
  calls.route.mockResolvedValue({ outcome: "routed" });
  await bridge.routeToContext(
    { codeRepoPath: "/repo", codeRepoSlug: "acme/api", logicalName: "shared.md" },
    ctx,
  );
  expect(calls.route).toHaveBeenCalledWith(
    "/repo",
    "acme/api",
    "shared.md",
    undefined,
    frame.frameId,
    page,
  );
  for (const path of ["/org/parent/content/secret.md", "/org/parent/content/absent.md"]) {
    calls.fields.mockRejectedValueOnce(new Error("File not found"));
    await expect(bridge.setField({ path, fields: { status: "done" } }, ctx)).rejects.toThrow(
      "File not found",
    );
  }
});
