// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, render } from "@testing-library/react";
import { useRef } from "react";

import { useScrollSnapshot } from "./use-scroll-snapshot";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { navViewKey } from "@/shared/lib/view-snapshot";
import { OwningScopeProvider, warmScopes } from "@/shared/lib/warm-scopes";
import { expectFlatAcross } from "@/shared/lib/warm-scopes/__test-utils__/expect-flat-across";

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

const KEY = navViewKey("acme/web:default");

function Harness({ viewKey }: { viewKey: string | null }) {
  const ref = useRef<HTMLDivElement>(null);
  useScrollSnapshot({ viewKey, getEl: () => ref.current, ready: true });
  return <div ref={ref} data-testid="scroller" style={{ height: 100, overflow: "auto" }} />;
}

beforeEach(() => {
  useViewerStore.setState({ viewStateByKey: {} });
});

describe("useScrollSnapshot (RF-perf-12)", () => {
  it("captures the live scrollTop into the slice on unmount", () => {
    const { getByTestId, unmount } = render(<Harness viewKey={KEY} />);
    const el = getByTestId("scroller");
    el.scrollTop = 240;
    el.dispatchEvent(new Event("scroll"));

    unmount();

    expect(useViewerStore.getState().readViewState(KEY)).toEqual({ scrollTop: 240 });
  });

  it("restores the saved scrollTop on a fresh mount (before paint)", () => {
    useViewerStore.getState().captureViewState(KEY, { scrollTop: 175 });
    const { getByTestId } = render(<Harness viewKey={KEY} />);
    expect(getByTestId("scroller").scrollTop).toBe(175);
  });

  it("no-ops when viewKey is null", () => {
    const { getByTestId, unmount } = render(<Harness viewKey={null} />);
    const el = getByTestId("scroller");
    el.scrollTop = 50;
    el.dispatchEvent(new Event("scroll"));
    unmount();
    expect(useViewerStore.getState().viewStateByKey).toEqual({});
  });

  it("captures the OLD key's scroll when viewKey changes IN PLACE (no remount)", () => {
    // Regression: a shared live-ref written by the listener but read in a
    // separate cleanup would capture the OLD key with the NEW element's scroll.
    // Capturing inside the listener effect's own cleanup keeps key+value paired.
    const KEY_A = navViewKey("acme/web:default");
    const KEY_B = navViewKey("acme/api:default");
    const { getByTestId, rerender } = render(<Harness viewKey={KEY_A} />);
    const el = getByTestId("scroller");
    el.scrollTop = 275;
    el.dispatchEvent(new Event("scroll"));

    // Same component reused for a different key (React reconciles in place).
    rerender(<Harness viewKey={KEY_B} />);

    // A's snapshot must hold A's scroll (275), not B's current scroll.
    expect(useViewerStore.getState().readViewState(KEY_A)).toEqual({ scrollTop: 275 });
  });
});

describe("useScrollSnapshot in a warm team", () => {
  const TEAM = "acme/web:default";
  const OTHER = "acme/web:other";

  it("puts the scroll back when the team is shown again after its box was hidden", () => {
    warmScopes.activate(TEAM);
    const { getByTestId } = render(
      <OwningScopeProvider scope={TEAM}>
        <Harness viewKey={navViewKey(TEAM)} />
      </OwningScopeProvider>,
    );
    const el = getByTestId("scroller");
    el.scrollTop = 310;
    el.dispatchEvent(new Event("scroll"));
    act(() => warmScopes.activate(OTHER));
    // A hidden box loses its scroll offset in the engine.
    el.scrollTop = 0;
    act(() => warmScopes.activate(TEAM));
    expect(el.scrollTop).toBe(310);
    warmScopes.forget(TEAM);
    warmScopes.forget(OTHER);
    warmScopes.activate(null);
  });

  it("50 hide/show round trips never reset the scroll", async () => {
    warmScopes.activate(TEAM);
    const { getByTestId } = render(
      <OwningScopeProvider scope={TEAM}>
        <Harness viewKey={navViewKey(TEAM)} />
      </OwningScopeProvider>,
    );
    const el = getByTestId("scroller");
    el.scrollTop = 310;
    el.dispatchEvent(new Event("scroll"));
    let scrollResets = 0;
    await expectFlatAcross(
      async () => {
        act(() => warmScopes.activate(OTHER));
        el.scrollTop = 0;
        act(() => warmScopes.activate(TEAM));
        if (el.scrollTop !== 310) scrollResets += 1;
        el.scrollTop = 310;
        el.dispatchEvent(new Event("scroll"));
      },
      () => ({ scrollResets }),
    );
    warmScopes.forget(TEAM);
    warmScopes.forget(OTHER);
    warmScopes.activate(null);
  });
});
