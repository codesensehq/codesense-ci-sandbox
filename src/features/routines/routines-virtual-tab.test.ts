// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const { openTabs } = vi.hoisted(() => ({ openTabs: [] as string[] }));
vi.mock("@/shared/lib/open-management-tab", () => ({
  openManagementTab: (path: string) => openTabs.push(path),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

import { useAgentsStore, resetAgentsStoreForTests } from "@/features/agents/stores/agents-store";
import { AGENTS_VIRTUAL_PATH } from "@/features/agents/agents-virtual-tab";

import { openRoutineDraft, registerRoutinesTab, retiredRoutinesTabs } from "./routines-virtual-tab";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { resetRoutinesStoreForTests, useRoutinesStore } from "./stores/routines-store";

// SW-AGR-5: "Keep this…" on a timer (SW-AGR-6) opens New routine under the timer's agent,
// in the Agents tab: routines belong to agents (SPEC §10).

beforeEach(() => {
  openTabs.length = 0;
  resetAgentsStoreForTests();
  resetRoutinesStoreForTests();
});

describe("openRoutineDraft", () => {
  it("opens Agents → the agent → New routine, filled in from the seed", () => {
    openRoutineDraft({
      draft: { agent: "acme/backend/triage", name: "Inbox nudge", task: "read", when: "15m" },
      mode: "wake",
      machineId: "codesense-test-1",
    });
    expect(openTabs).toEqual([AGENTS_VIRTUAL_PATH]);
    expect(useAgentsStore.getState().selectedKey).toBe("acme/backend/triage");
    const routines = useRoutinesStore.getState();
    expect(routines.routineOwner).toBe("acme/backend/triage");
    expect(routines.creating).toBe(true);
    expect(routines.createSeed?.draft.name).toBe("Inbox nudge");
  });
});

describe("retired Routines tabs (SPEC §10)", () => {
  it("rewrites every team's persisted Routines tab to Agents, in place, once", () => {
    const tabs = {
      "acme:backend": [
        { path: "a.md", name: "a.md" },
        { path: "virtual://routines", name: "Routines" },
      ],
      "acme:infra": [
        { path: "virtual://routines?agent=acme/infra/barb", name: "Routines" },
        { path: "virtual://agents", name: "Agents" },
      ],
      "acme:docs": [{ path: "b.md", name: "b.md" }],
    };
    const active = { "acme:backend": "virtual://routines", "acme:infra": "virtual://agents" };
    const next = retiredRoutinesTabs(tabs, active);
    expect(next?.openTabs["acme:backend"]).toEqual([
      { path: "a.md", name: "a.md" },
      { path: "virtual://agents", name: "Agents" },
    ]);
    expect(next?.activeTab["acme:backend"]).toBe("virtual://agents");
    // A team that already has the Agents tab keeps one.
    expect(next?.openTabs["acme:infra"]).toEqual([{ path: "virtual://agents", name: "Agents" }]);
    expect(next?.openTabs["acme:docs"]).toBe(tabs["acme:docs"]);
    // The filter it carried comes along, for the Schedule it opens.
    expect(next?.agentKey).toBe("acme/infra/barb");
    expect(retiredRoutinesTabs(next!.openTabs, next!.activeTab)).toBeNull();
  });
});

describe("a restored Routines tab (SPEC §10, §2.1)", () => {
  it("opens Agents → Schedule, filtered to the agent it was", async () => {
    vi.useFakeTimers();
    useViewerStore.setState({
      openTabs: {
        "acme:infra": [{ path: "virtual://routines?agent=acme/infra/barb", name: "Routines" }],
      },
      activeTab: { "acme:infra": "virtual://routines?agent=acme/infra/barb" },
    } as never);
    registerRoutinesTab();
    vi.runAllTimers();
    vi.useRealTimers();
    expect(useViewerStore.getState().openTabs["acme:infra"]).toEqual([
      { path: "virtual://agents", name: "Agents" },
    ]);
    expect(useAgentsStore.getState().tabView).toBe("schedule");
    expect(useAgentsStore.getState().scheduleFilter).toEqual({
      kind: "agent",
      agentKey: "acme/infra/barb",
      name: "barb",
    });
  });
});
