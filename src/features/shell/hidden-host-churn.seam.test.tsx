// SPDX-License-Identifier: MIT
import { resetContentFileIndexForTests } from "@/shared/lib/pipeline-graph/content-scan-queue";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@tauri-apps/api/core")>()),
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
  emit: vi.fn(async () => {}),
}));
vi.mock("@/shared/lib/get-current-user", () => ({
  useCurrentUser: () => ({ github_user_id: 7 }),
}));

import { useContextMoveRowSlot } from "@/features/context-mobility/context-mobility-host";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useMergeMarks } from "@/features/dashboard/lib/use-merge-marks";
import { invalidateTeamMachines, useTeamMachines } from "@/features/machines/stores/machines-store";
import { useTeamEditsStore } from "@/features/team-edits/stores/team-edits-store";
import { useTeamEditsRefresh } from "@/features/team-edits/stores/use-team-edits-refresh";
import { expectFlatAcross } from "@/shared/lib/warm-scopes/__test-utils__/expect-flat-across";
import { OwningScopeProvider, warmScopes } from "@/shared/lib/warm-scopes";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";

// Three warm teams, each with the hooks a team view mounts: the move-target listing, the merge
// marks, the machines list and the unsaved-edits read. Team C is in another org and stays hidden
// through the whole run: none of its hooks may reach the backend while it is.
const A = "acme:a";
const B = "acme:b";
const C = "other:c";
const org = (slug: string, teams: string[]): Org =>
  ({
    slug,
    display_name: slug,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    teams: teams.map((name) => ({ name, source_repos: [] })),
    org_root: `/mock/${slug}`,
  }) as Org;

function Inner({ teamKey }: { teamKey: string }) {
  const [orgSlug, team] = teamKey.split(":");
  useMergeMarks(`/mock/${orgSlug}`, team);
  useTeamMachines(teamKey);
  useTeamEditsRefresh(teamKey);
  return null;
}

/** A team view: the move-target hook runs in its body, the rest under its owning scope. */
function TeamHost({ teamKey }: { teamKey: string }) {
  useContextMoveRowSlot(teamKey, `/repo/${teamKey}`);
  return (
    <OwningScopeProvider scope={teamKey}>
      <Inner teamKey={teamKey} />
    </OwningScopeProvider>
  );
}

const calls = { files: 0, marks: 0, machines: 0, edits: 0 };
let stopWatching = () => {};

beforeEach(() => {
  resetContentFileIndexForTests();
  Object.assign(calls, { files: 0, marks: 0, machines: 0, edits: 0 });
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    // Everything C's hooks can ask for, attributed by its own org, team or content root.
    if (cmd === "list_context_files" && String(args?.rootPath).startsWith("/mock/other")) {
      calls.files += 1;
    }
    if (cmd === "list_content_merge_marks" && args?.team === "c") calls.marks += 1;
    if (cmd === "list_machines" && args?.team === "c") calls.machines += 1;
    if (cmd === "list_context_files" || cmd === "list_content_merge_marks") return [];
    if (cmd === "list_machines") return [];
    return null;
  });
  stopWatching = useTeamEditsStore.subscribe((state, previous) => {
    if (state.byTeam[C]?.loading && !previous.byTeam[C]?.loading) calls.edits += 1;
  });
  useOrgStore.setState({
    orgs: [org("acme", ["a", "b"]), org("other", ["c", "d"])],
    activeOrgSlug: "other",
    activeTeamName: "c",
  });
  for (const key of [A, B, C]) warmScopes.forget(key);
  warmScopes.activate(C);
});

afterEach(() => {
  stopWatching();
  cleanup();
});

const settle = () => act(async () => void (await new Promise((r) => setTimeout(r, 0))));

describe("a hidden team's host", () => {
  it("reads nothing across 50 warm switches between two other teams", async () => {
    render(
      <>
        <TeamHost teamKey={A} />
        <TeamHost teamKey={B} />
        <TeamHost teamKey={C} />
      </>,
    );
    await settle();
    // C was the active team; it read once on the way in. Now it goes hidden for good.
    const shown = (key: string, org: string, team: string) =>
      act(async () => {
        useOrgStore.setState({ activeOrgSlug: org, activeTeamName: team });
        warmScopes.activate(key);
        await new Promise((r) => setTimeout(r, 0));
      });
    await shown(A, "acme", "a");
    // Everything a switch can stir up reaches the hidden host too: the file-refresh key its move
    // targets re-list on, its machines list being dropped, and the window focus its edits read on.
    const stir = () => {
      useFileRefreshStore.getState().bumpFileRefresh(`/repo/${C}`, undefined);
      invalidateTeamMachines(C);
      window.dispatchEvent(new Event("focus"));
    };
    await expectFlatAcross(
      async () => {
        await shown(B, "acme", "b");
        stir();
        await shown(A, "acme", "a");
        stir();
      },
      () => ({ ...calls }),
    );
  });
});

describe("a merge-marks hook in a hidden team", () => {
  it("loads when its team is shown, not while it is hidden", async () => {
    function Marks({ teamKey }: { teamKey: string }) {
      const [orgSlug, team] = teamKey.split(":");
      useMergeMarks(`/mock/${orgSlug}`, team);
      return null;
    }
    render(
      <OwningScopeProvider scope={C}>
        <Marks teamKey={C} />
      </OwningScopeProvider>,
    );
    await settle();
    expect(calls.marks).toBe(1);
    cleanup();
    warmScopes.activate(A);
    render(
      <OwningScopeProvider scope={C}>
        <Marks teamKey={C} />
      </OwningScopeProvider>,
    );
    await settle();
    expect(calls.marks).toBe(1);
  });
});
