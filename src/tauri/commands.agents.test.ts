// SPDX-License-Identifier: MIT
/**
 * The Agents tab wrappers send what their Rust handlers take: camelCase argument names
 * Tauri maps onto the handlers' snake_case parameters.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

import {
  agentArchive,
  agentRestoreRoutines,
  agentBindingClear,
  agentBindingSet,
  agentCopyToMine,
  agentDelete,
  agentDetail,
  agentOpenAuthoringAgent,
  agentPreview,
  agentRestoreRevision,
  agentSave,
  agentsList,
  agentStartupCatalog,
  agentUnarchive,
  type AgentForm,
} from "./commands";

const KEY = "acme/backend/triage";
const FORM: AgentForm = {
  key: KEY,
  name: "Triage",
  description: "",
  runtime: "claude",
  args: [],
  launch: "foreground",
  mcpServers: [],
  startup: ["CLAUDE.md"],
  permissions: null,
  defaultPrompt: "",
};
const TEAM = { org: "acme", team: "backend" };

describe("agent command wrappers", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue(undefined);
  });

  it.each([
    ["agents_list", () => agentsList(), { includeArchived: false }],
    ["agents_list", () => agentsList(true), { includeArchived: true }],
    ["agent_detail", () => agentDetail(KEY), { key: KEY }],
    ["agent_archive", () => agentArchive(KEY), { key: KEY, archiveRoutines: false }],
    ["agent_archive", () => agentArchive(KEY, true), { key: KEY, archiveRoutines: true }],
    ["agent_restore_routines", () => agentRestoreRoutines(KEY), { key: KEY }],
    ["agent_unarchive", () => agentUnarchive(KEY, 4), { key: KEY, baseRevision: 4 }],
    ["agent_delete", () => agentDelete(KEY, 2), { key: KEY, baseRevision: 2 }],
    ["agent_copy_to_mine", () => agentCopyToMine(KEY), { key: KEY }],
    [
      "agent_binding_set",
      () => agentBindingSet(KEY, { accountId: "acct-1" }),
      { key: KEY, accountId: "acct-1", captureRawTerminal: null },
    ],
    [
      "agent_binding_set",
      () => agentBindingSet(KEY, { captureRawTerminal: true }),
      { key: KEY, accountId: null, captureRawTerminal: true },
    ],
    ["agent_binding_clear", () => agentBindingClear(KEY), { key: KEY }],
    ["agent_save", () => agentSave(FORM, 3), { form: FORM, baseRevision: 3 }],
    ["agent_save", () => agentSave(FORM), { form: FORM, baseRevision: null }],
    ["agent_preview", () => agentPreview(FORM, TEAM), { form: FORM, team: TEAM }],
    ["agent_preview", () => agentPreview(FORM), { form: FORM, team: null }],
    [
      "agent_restore_revision",
      () => agentRestoreRevision(KEY, 1, 4),
      { key: KEY, revision: 1, baseRevision: 4 },
    ],
    [
      "agent_startup_catalog",
      () => agentStartupCatalog(TEAM, "docs", { offset: 500, filter: "run" }),
      { team: TEAM, dir: "docs", offset: 500, filter: "run" },
    ],
    [
      "agent_startup_catalog",
      () => agentStartupCatalog(TEAM),
      { team: TEAM, dir: null, offset: null, filter: null },
    ],
  ])("%s", async (command, call, args) => {
    await call();
    expect(mockInvoke).toHaveBeenCalledWith(command, args);
  });

  it("agent_open_authoring_agent takes no arguments", async () => {
    await agentOpenAuthoringAgent();
    expect(mockInvoke).toHaveBeenCalledWith("agent_open_authoring_agent");
  });
});
