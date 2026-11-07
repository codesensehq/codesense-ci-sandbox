// SPDX-License-Identifier: MIT
/**
 * The Routines wrappers and the launch bind send what their Rust handlers take:
 * camelCase argument names Tauri maps onto the handlers' snake_case parameters,
 * with absent options as null.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockInvoke } = vi.hoisted(() => ({ mockInvoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mockInvoke }));

import {
  dashboardLaunchBind,
  routineAcknowledgeMissed,
  routineArchive,
  routineArmHere,
  routineArmOn,
  routineHandoverRelease,
  routineRefreshOnMachine,
  routineCopyToMine,
  routineGet,
  routineList,
  routineNeedsYouCount,
  routineOpenAuthoringAgent,
  routinePreviewSchedule,
  routineRestoreRevision,
  routineRunDetail,
  routineRunNow,
  routineRuns,
  routineSave,
  routineSchedulerStatus,
  routineSetEnabled,
  routineStopRun,
} from "./commands";

const KEY = "_personal/1/personal/digest";

describe("routine command wrappers", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockInvoke.mockResolvedValue(undefined);
  });

  it.each([
    ["routine_list", () => routineList(), undefined],
    ["routine_get", () => routineGet(KEY), { key: KEY }],
    ["routine_set_enabled", () => routineSetEnabled(KEY, false), { key: KEY, enabled: false }],
    [
      "routine_run_now",
      () => routineRunNow(KEY),
      { key: KEY, variables: null, frameId: null, pagePath: null },
    ],
    [
      "routine_run_now",
      () => routineRunNow(KEY, { repo: "api" }),
      { key: KEY, variables: { repo: "api" }, frameId: null, pagePath: null },
    ],
    ["routine_runs", () => routineRuns(KEY), { key: KEY, limit: null, before: null }],
    ["routine_runs", () => routineRuns(KEY, 20, "r-9"), { key: KEY, limit: 20, before: "r-9" }],
    ["routine_stop_run", () => routineStopRun("r-1"), { runId: "r-1" }],
    ["routine_archive", () => routineArchive(KEY), { key: KEY }],
    ["routine_copy_to_mine", () => routineCopyToMine(KEY), { key: KEY }],
    ["routine_arm_here", () => routineArmHere(KEY), { key: KEY }],
    ["routine_arm_on", () => routineArmOn(KEY), { key: KEY, machine: null }],
    ["routine_arm_on", () => routineArmOn(KEY, "m-1"), { key: KEY, machine: "m-1" }],
    [
      "routine_handover_release",
      () => routineHandoverRelease(KEY, "m-1"),
      { key: KEY, machine: "m-1" },
    ],
    ["routine_refresh_on_machine", () => routineRefreshOnMachine(KEY), { key: KEY }],
    ["routine_run_detail", () => routineRunDetail("r-1"), { runId: "r-1" }],
    ["routine_needs_you_count", () => routineNeedsYouCount(), undefined],
    ["routine_scheduler_status", () => routineSchedulerStatus(), undefined],
    ["routine_acknowledge_missed", () => routineAcknowledgeMissed(), undefined],
    [
      "routine_save",
      () => routineSave({ name: "n", task: "t", when: "every 5m" }, undefined, false),
      {
        key: null,
        baseRevision: null,
        form: { name: "n", task: "t", when: "every 5m" },
        enabled: false,
        team: null,
      },
    ],
    [
      "routine_save",
      () =>
        routineSave({ name: "n", task: "t", when: "every 5m" }, undefined, true, {
          org: "acme/ctx",
          team: "default",
        }),
      {
        key: null,
        baseRevision: null,
        form: { name: "n", task: "t", when: "every 5m" },
        enabled: true,
        team: { org: "acme/ctx", team: "default" },
      },
    ],
    [
      "routine_save",
      () => routineSave({ task: "t2" }, { key: KEY, baseRevision: 3 }),
      { key: KEY, baseRevision: 3, form: { task: "t2" }, enabled: null, team: null },
    ],
    [
      "routine_preview_schedule",
      () => routinePreviewSchedule("every 5m"),
      { when: "every 5m", tz: null },
    ],
    [
      "routine_restore_revision",
      () => routineRestoreRevision(KEY, 1, 4),
      { key: KEY, revision: 1, baseRevision: 4 },
    ],
    ["routine_open_authoring_agent", () => routineOpenAuthoringAgent(), undefined],
  ] as const)("%s", async (command, call, args) => {
    await call();
    if (args === undefined) expect(mockInvoke).toHaveBeenLastCalledWith(command);
    else expect(mockInvoke).toHaveBeenLastCalledWith(command, args);
  });

  it("binds a launch with where it ran and its agent revision, or nulls", async () => {
    await dashboardLaunchBind("SW-1", "sess-1", "doc.md", {
      teamKey: "acme:backend",
      agentRevision: "acme/backend/dev@2",
    });
    expect(mockInvoke).toHaveBeenLastCalledWith("dashboard_launch_bind", {
      handle: "SW-1",
      sessionId: "sess-1",
      launcher: "doc.md",
      teamKey: "acme:backend",
      agentRevision: "acme/backend/dev@2",
    });
    await dashboardLaunchBind("SW-1", "sess-1");
    expect(mockInvoke).toHaveBeenLastCalledWith("dashboard_launch_bind", {
      handle: "SW-1",
      sessionId: "sess-1",
      launcher: null,
      teamKey: null,
      agentRevision: null,
    });
  });
});
