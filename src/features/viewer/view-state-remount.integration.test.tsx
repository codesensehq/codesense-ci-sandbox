// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, act } from "@testing-library/react";
import { useEffect } from "react";

// RF-perf-12 (T6): end-to-end proof that a team's view-position survives the
// RF-perf-10 render gate. We drive the REAL `useTeamRenderGate` and the warm-scope owner
// (visiting enough other teams suspends a team) so its subtree actually unmounts, and assert a
// scroller wired with the REAL `useScrollSnapshot` + store slice restores on
// remount. Also proves the OQ-4 guarantee: a DIRTY team stays mounted, so its
// capture path never fires.

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "S", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi
    .fn()
    .mockResolvedValue({ id: "n2", title: "U", pinned: false, lastOpenedAt: 0, content: "" }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "S", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));

import { useTeamRenderGate } from "@/app/hooks/use-team-render-gate";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { useScrollSnapshot } from "@/shared/hooks/use-scroll-snapshot";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { navViewKey } from "@/shared/lib/view-snapshot";

const TEAM_A = "acme/web:default";
const TEAM_B = "acme/api:default";
const TEAM_X = "acme/x:default";
const TEAM_Y = "acme/y:default";
const OPENED = [TEAM_A, TEAM_B, TEAM_X, TEAM_Y];

let mountCounts: Record<string, number> = {};

function TeamScroller({ teamKey }: { teamKey: string }) {
  useEffect(() => {
    mountCounts[teamKey] = (mountCounts[teamKey] ?? 0) + 1;
  }, [teamKey]);
  const ref = (el: HTMLDivElement | null) => {
    scrollEls[teamKey] = el;
  };
  useScrollSnapshot({ viewKey: navViewKey(teamKey), getEl: () => scrollEls[teamKey], ready: true });
  return (
    <div data-testid={`scroller:${teamKey}`} ref={ref} style={{ height: 100, overflow: "auto" }} />
  );
}

const scrollEls: Record<string, HTMLDivElement | null> = {};

const EMPTY_DIRTY: ReadonlySet<string> = new Set();

function Harness({
  activeTeamKey,
  dirty = EMPTY_DIRTY,
}: {
  activeTeamKey: string;
  dirty?: ReadonlySet<string>;
}) {
  warmScopes.activate(activeTeamKey);
  const rendered = useTeamRenderGate({
    openedTeams: OPENED,
    dirtyTeamKeys: dirty,
  });
  return (
    <>{OPENED.map((tk) => (rendered.has(tk) ? <TeamScroller key={tk} teamKey={tk} /> : null))}</>
  );
}

beforeEach(() => {
  vi.useFakeTimers();
  useViewerStore.setState({ viewStateByKey: {} });
  mountCounts = {};
});

afterEach(() => {
  for (const k of OPENED) warmScopes.forget(k);
  vi.runOnlyPendingTimers();
  vi.useRealTimers();
});

describe("RF-perf-12 view-state across the RF-perf-10 render gate", () => {
  it("restores nav scroll after a team unmounts (out of the warm set) and remounts", () => {
    const { getByTestId, queryByTestId, rerender } = render(<Harness activeTeamKey={TEAM_A} />);

    // Team A is active + mounted; scroll it.
    const elA = getByTestId(`scroller:${TEAM_A}`) as HTMLDivElement;
    elA.scrollTop = 275;
    elA.dispatchEvent(new Event("scroll"));

    // Visit three other teams: A falls out of the warm set and unmounts.
    for (const team of [TEAM_B, TEAM_X, TEAM_Y]) {
      act(() => {
        rerender(<Harness activeTeamKey={team} />);
      });
    }
    expect(queryByTestId(`scroller:${TEAM_A}`)).toBeNull(); // A really unmounted
    expect(useViewerStore.getState().readViewState(navViewKey(TEAM_A))).toEqual({ scrollTop: 275 });

    // Switch back to A → it remounts and restores the saved scroll (pre-paint).
    act(() => {
      rerender(<Harness activeTeamKey={TEAM_A} />);
    });
    const elA2 = getByTestId(`scroller:${TEAM_A}`) as HTMLDivElement;
    expect(mountCounts[TEAM_A]).toBe(2); // proved a real unmount→remount cycle
    expect(elA2.scrollTop).toBe(275);
  });

  it("a DIRTY team never unmounts, so its capture path does not fire (OQ-4)", () => {
    // Mark A dirty (unsaved edit) — RF-perf-10 pins it mounted.
    const dirty = new Set([TEAM_A]);
    const { getByTestId, queryByTestId, rerender } = render(
      <Harness activeTeamKey={TEAM_A} dirty={dirty} />,
    );
    const elA = getByTestId(`scroller:${TEAM_A}`) as HTMLDivElement;
    elA.scrollTop = 120;
    elA.dispatchEvent(new Event("scroll"));

    // Visit three other teams: A stays mounted because it is dirty (pinned).
    for (const team of [TEAM_B, TEAM_X, TEAM_Y]) {
      act(() => {
        rerender(<Harness activeTeamKey={team} dirty={dirty} />);
      });
    }
    expect(queryByTestId(`scroller:${TEAM_A}`)).not.toBeNull(); // still mounted
    expect(mountCounts[TEAM_A]).toBe(1); // never torn down
    // No unmount → no capture written for A.
    expect(useViewerStore.getState().readViewState(navViewKey(TEAM_A))).toBeUndefined();
  });
});
