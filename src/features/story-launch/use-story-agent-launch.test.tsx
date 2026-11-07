// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";

import type { AgentRow } from "@/tauri/commands";
import { getDependencyStatus, type DependencyStatus } from "@/tauri/commands";
import { openStoryLaunchSheet } from "./story-agent-launch";
import { useStoryAgentLaunch } from "./use-story-agent-launch";

const agentRows = vi.hoisted(() => ({ current: null as unknown[] | null }));
vi.mock("@/features/agents/lib/agent-names", () => ({
  useAgentRows: () => ({ rows: agentRows.current, error: null }),
}));
vi.mock("@/tauri/commands", async (orig) => ({
  ...(await orig<typeof import("@/tauri/commands")>()),
  getDependencyStatus: vi.fn(),
}));

/** A dependency cache whose CLIs are installed unless `missing` or still `resolving`. */
function deps(missing: string[] = [], resolving: string[] = []): DependencyStatus {
  const entry = (binary: string) => ({
    available: !missing.includes(binary) && !resolving.includes(binary),
    path: missing.includes(binary) || resolving.includes(binary) ? null : `/opt/bin/${binary}`,
    resolving: resolving.includes(binary),
  });
  return {
    agents: { claude: entry("claude"), codex: entry("codex") },
  } as unknown as DependencyStatus;
}
const eventHandlers = new Map<string, Set<() => void>>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (event: string, handler: () => void) => {
    const set = eventHandlers.get(event) ?? new Set();
    set.add(handler);
    eventHandlers.set(event, set);
    return () => set.delete(handler);
  }),
}));
function emit(event: string) {
  for (const handler of eventHandlers.get(event) ?? []) handler();
}
vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: () => ({ github_user_id: 7 }),
}));
vi.mock("./story-agent-launch", async (orig) => ({
  ...(await orig<typeof import("./story-agent-launch")>()),
  openStoryLaunchSheet: vi.fn(async () => {}),
}));

function row(key: string, runtime: "claude" | "codex"): AgentRow {
  return {
    key,
    name: key.split("/").pop()!,
    description: "",
    runtime,
    launch: "foreground",
    scope: "team",
    org: "org",
    team: "team",
    created_by: "you",
    created_by_you: true,
    archived: false,
    revision: 1,
    default_prompt: "",
    avatar: null,
    account: {
      source: "runtime_default",
      bound_account_id: null,
      resolved_source: "system_default",
      account_id: null,
      provider: runtime,
      label: null,
      reason: "optional-system-default",
      availability: "available",
      spawn_eligible: true,
      capture_raw_terminal: false,
    },
    used_by: [],
    routines: [],
    archived_routines: [],
    model: null,
    running_now: 0,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(getDependencyStatus).mockResolvedValue(deps());
  eventHandlers.clear();
  agentRows.current = [row("org/team/reviewer", "codex"), row("org/team/builder", "claude")];
});

describe("useStoryAgentLaunch", () => {
  it("lists the agents by name and marks a CLI the dependency cache lacks as missing", async () => {
    vi.mocked(getDependencyStatus).mockResolvedValue(deps(["codex"]));
    const { result } = renderHook(() => useStoryAgentLaunch("org:team"));

    await waitFor(() =>
      expect(result.current?.agents.map((a) => [a.agentKey, a.availability])).toEqual([
        ["org/team/builder", "available"],
        ["org/team/reviewer", "missing"],
      ]),
    );
    expect(result.current?.agents[0].isDefault).toBe(true);
    expect(result.current?.shortcutLabel).toBeTruthy();
  });

  it("keeps checking a CLI whose PATH probe is still running", async () => {
    vi.mocked(getDependencyStatus)
      .mockResolvedValueOnce(deps([], ["codex"]))
      .mockResolvedValueOnce(deps([], ["codex"]))
      .mockResolvedValue(deps());
    const { result } = renderHook(() => useStoryAgentLaunch("org:team"));
    await waitFor(() =>
      expect(result.current?.agents.find((a) => a.runtime === "codex")?.availability).toBe(
        "checking",
      ),
    );
    await waitFor(
      () =>
        expect(result.current?.agents.find((a) => a.runtime === "codex")?.availability).toBe(
          "available",
        ),
      { timeout: 4000 },
    );
  });

  it("a probe that fails is left for the launch to surface, never hiding the agent", async () => {
    vi.mocked(getDependencyStatus).mockRejectedValue(new Error("deps unreadable"));
    const { result } = renderHook(() => useStoryAgentLaunch("org:team"));
    await waitFor(() =>
      expect(result.current?.agents.every((a) => a.availability === "available")).toBe(true),
    );
  });

  it("re-probes on dep-status-changed so a CLI installed after boot becomes launchable without a remount", async () => {
    let codexInstalled = false;
    vi.mocked(getDependencyStatus).mockImplementation(async () =>
      deps(codexInstalled ? [] : ["codex"]),
    );
    const { result } = renderHook(() => useStoryAgentLaunch("org:team"));
    await waitFor(() =>
      expect(result.current?.agents.find((a) => a.runtime === "codex")?.availability).toBe(
        "missing",
      ),
    );
    await waitFor(() => expect(eventHandlers.get("dep-status-changed")?.size).toBe(1));

    codexInstalled = true;
    act(() => emit("dep-status-changed"));

    await waitFor(() =>
      expect(result.current?.agents.find((a) => a.runtime === "codex")?.availability).toBe(
        "available",
      ),
    );
  });

  it("a pick opens the launch sheet on the story for this team", async () => {
    const { result } = renderHook(() => useStoryAgentLaunch("org:team"));
    await waitFor(() => expect(result.current).not.toBeNull());

    const chosen = result.current!.agents[1];
    result.current!.launch({ storyFilePath: "/ctx/S-1.md", storyId: "S-1" }, chosen);
    expect(openStoryLaunchSheet).toHaveBeenCalledWith(
      "org:team",
      { storyFilePath: "/ctx/S-1.md", storyId: "S-1" },
      chosen,
    );
  });

  it("with no agents of your own, offers the two starters, the default on Claude", async () => {
    const starter = (runtime: "claude" | "codex"): AgentRow => ({
      ...row(`builtin/${runtime}`, runtime),
      name: runtime === "claude" ? "Claude" : "Codex",
      scope: "builtin",
      org: "builtin",
      team: "",
    });
    agentRows.current = [starter("codex"), starter("claude")];
    const { result } = renderHook(() => useStoryAgentLaunch("org:team"));
    await waitFor(() =>
      expect(result.current?.agents.map((a) => [a.agentKey, a.place, a.availability])).toEqual([
        ["builtin/claude", "built-in", "available"],
        ["builtin/codex", "built-in", "available"],
      ]),
    );
    expect(result.current?.agents[0].isDefault).toBe(true);
  });

  it("is null (no menu) until the agent list has loaded, and while there is no agent", () => {
    agentRows.current = null;
    const { result, rerender } = renderHook(() => useStoryAgentLaunch("org:team"));
    expect(result.current).toBeNull();
    agentRows.current = [];
    rerender();
    expect(result.current).toBeNull();
  });
});
