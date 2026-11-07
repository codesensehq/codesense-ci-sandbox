// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

vi.mock("@/tauri/commands", () => ({ sessionHasLiveAgent: vi.fn(async () => true) }));

import {
  listWorkspaceAgentTabs,
  buildAgentTabOptions,
  useWorkspaceAgentTabs,
} from "@/shared/hooks/use-workspace-agent-tabs";
import type { TerminalTab } from "@/shared/stores/viewer-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";

installTerminalLayoutHarness();

function tab(id: string, role?: string): TerminalTab {
  return { id, name: id.toUpperCase(), sessionId: id, role };
}

describe("listWorkspaceAgentTabs (AC-4/4a — scope to the resolved workspace instance)", () => {
  const map: Record<string, Record<string, TerminalTab[]>> = {
    "acme:default": {
      "test-agent": [tab("shell-1", "test-agent"), tab("shell-2")],
      "story:A": [tab("shell-3", "story:A")],
    },
    "other:team": { "test-agent": [tab("shell-9", "test-agent")] },
  };

  it("returns only the tabs of the (teamKey, targetWorkspace) bucket", () => {
    const tabs = listWorkspaceAgentTabs(map, "acme:default", "test-agent");
    expect(tabs.map((t) => t.sessionId)).toEqual(["shell-1", "shell-2"]);
  });

  it("scopes per instance — a different workspace id in the same team is separate", () => {
    expect(listWorkspaceAgentTabs(map, "acme:default", "story:A").map((t) => t.sessionId)).toEqual([
      "shell-3",
    ]);
  });

  it("is empty for an unknown team or workspace (no throw)", () => {
    expect(listWorkspaceAgentTabs(map, "nope:team", "test-agent")).toEqual([]);
    expect(listWorkspaceAgentTabs(map, "acme:default", "nope")).toEqual([]);
  });
});

describe("buildAgentTabOptions (AC-4 — live-filter + state hint)", () => {
  const tabs: TerminalTab[] = [tab("shell-1", "test-agent"), tab("shell-2"), tab("shell-3")];

  it("annotates each candidate with live ● / not-responding ◐, role-tagged tabs always candidates", () => {
    const opts = buildAgentTabOptions(tabs, { "shell-1": false, "shell-2": true, "shell-3": null });
    // shell-1: role-tagged but probe says dead → candidate, ◐ (not-responding)
    // shell-2: roleless but proven live → candidate, ●
    // shell-3: roleless, still probing (null) → NOT yet a candidate (excluded)
    expect(opts.map((o) => [o.sessionId, o.live])).toEqual([
      ["shell-1", false],
      ["shell-2", true],
    ]);
  });

  it("a roleless bare shell with no live agent is excluded from the connect list (AC-4)", () => {
    const opts = buildAgentTabOptions([tab("bare")], { bare: false });
    expect(opts).toEqual([]);
  });

  it("defaults missing liveness to null (probing) and keeps role-tagged tabs visible", () => {
    const opts = buildAgentTabOptions([tab("owned", "test-agent")], {});
    expect(opts).toEqual([
      { id: "owned", name: "OWNED", sessionId: "owned", role: "test-agent", live: null },
    ]);
  });
});

describe("useWorkspaceAgentTabs — reactive team bucket (SW-AW-6 AC-2)", () => {
  beforeEach(() => {
    useViewerStore.setState({ softClosedWorkspaces: {} });
    seedTerminalTabs("acme:teamA", "test-agent", [{ session: "sess-A", role: "test-agent" }]);
    seedTerminalTabs("acme:teamB", "test-agent", [{ session: "sess-B", role: "test-agent" }]);
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "teamA" });
  });

  it("re-derives the bucket on team switch WITHOUT a remount (reactive useActiveTeamKey)", () => {
    const { result, rerender } = renderHook(() => useWorkspaceAgentTabs("test-agent", "claude"));
    // Role-tagged tabs are candidates regardless of the async liveness probe.
    expect(result.current.options.map((o) => o.sessionId)).toEqual(["sess-A"]);

    // Flip the active team — the SAME hook instance must re-read `[teamKey]` bucket.
    act(() => useOrgStore.setState({ activeTeamName: "teamB" }));
    rerender();
    expect(result.current.options.map((o) => o.sessionId)).toEqual(["sess-B"]);
  });
});
