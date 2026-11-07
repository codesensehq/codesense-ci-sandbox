// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { renderHook, act } from "@testing-library/react";

/**
 * SW-VE-7: OS file drop onto the context nav. The webview `onDragDropEvent` is
 * mocked exactly like the terminal-drag test — every subscriber fires on one
 * dispatched event, so the hit-test (position ∈ nav bounds, then row rects) is
 * what routes a drop to the right nav / directory.
 */

type DragDropHandler = (event: { payload: unknown }) => void;
const handlers: DragDropHandler[] = [];

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (handler: DragDropHandler) => {
      handlers.push(handler);
      return Promise.resolve(() => {});
    },
  }),
}));

import { useNavFileDrop } from "./use-nav-file-drop";

function dispatch(payload: unknown) {
  act(() => {
    for (const h of handlers) h({ payload });
  });
}

/** Real containment, with deterministic browser layout. */
const navs: HTMLElement[] = [];
function fakeNav(
  bounds: { left: number; top: number; right: number; bottom: number },
  dirs: { dir: string; rect: { left: number; top: number; right: number; bottom: number } }[],
): HTMLElement {
  const el = document.createElement("div");
  el.getBoundingClientRect = () => bounds as DOMRect;
  for (const d of dirs) {
    const row = document.createElement("div");
    row.dataset.navDropDir = d.dir;
    row.getBoundingClientRect = () => d.rect as DOMRect;
    el.append(row);
  }
  navs.push(el);
  return el;
}

const navBounds = { left: 0, top: 0, right: 300, bottom: 600 };

describe("useNavFileDrop", () => {
  beforeEach(() => {
    handlers.length = 0;
    navs.length = 0;
    Object.defineProperty(document, "elementFromPoint", {
      configurable: true,
      value: (x: number, y: number) =>
        navs.find((el) => {
          const r = el.getBoundingClientRect();
          return x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        }) ?? null,
    });
  });

  it("routes a drop over a directory row to that directory (AC-1/AC-2)", () => {
    const onDrop = vi.fn();
    const el = fakeNav(navBounds, [
      { dir: "/c/_config", rect: { left: 0, top: 20, right: 300, bottom: 44 } },
    ]);
    renderHook(() => useNavFileDrop({ getScrollElement: () => el, onDrop }));

    dispatch({ type: "drop", paths: ["/Users/me/a.md"], position: { x: 10, y: 30 } });

    expect(onDrop).toHaveBeenCalledWith({ dir: "/c/_config" }, ["/Users/me/a.md"]);
  });

  it("routes a drop over empty nav space to the root zone (AC-2)", () => {
    const onDrop = vi.fn();
    const el = fakeNav(navBounds, [
      { dir: "/c/_config", rect: { left: 0, top: 20, right: 300, bottom: 44 } },
    ]);
    renderHook(() => useNavFileDrop({ getScrollElement: () => el, onDrop }));

    dispatch({ type: "drop", paths: ["/Users/me/a.md"], position: { x: 10, y: 400 } });

    expect(onDrop).toHaveBeenCalledWith({ root: true }, ["/Users/me/a.md"]);
  });

  it("ignores a drop outside this nav's bounds (no cross-fire — AC-6)", () => {
    const onDrop = vi.fn();
    const el = fakeNav(navBounds, []);
    renderHook(() => useNavFileDrop({ getScrollElement: () => el, onDrop }));

    // x=900 is past the nav's right edge (e.g. over a terminal tile).
    dispatch({ type: "drop", paths: ["/Users/me/a.md"], position: { x: 900, y: 100 } });

    expect(onDrop).not.toHaveBeenCalled();
  });

  it("ignores an empty-paths drop", () => {
    const onDrop = vi.fn();
    const el = fakeNav(navBounds, []);
    renderHook(() => useNavFileDrop({ getScrollElement: () => el, onDrop }));

    dispatch({ type: "drop", paths: [], position: { x: 10, y: 100 } });

    expect(onDrop).not.toHaveBeenCalled();
  });

  it("highlights the hovered directory on over and clears on leave (AC-2)", () => {
    const el = fakeNav(navBounds, [
      { dir: "/c/scripts", rect: { left: 0, top: 20, right: 300, bottom: 44 } },
    ]);
    const { result } = renderHook(() =>
      useNavFileDrop({ getScrollElement: () => el, onDrop: vi.fn() }),
    );

    dispatch({ type: "over", position: { x: 10, y: 30 } });
    expect(result.current).toEqual({ dir: "/c/scripts" });

    dispatch({ type: "over", position: { x: 10, y: 400 } }); // empty space
    expect(result.current).toEqual({ root: true });

    dispatch({ type: "leave" });
    expect(result.current).toBeNull();
  });

  it("suppresses ingest while disabled (a collision dialog is open)", () => {
    const onDrop = vi.fn();
    const el = fakeNav(navBounds, [
      { dir: "/c/_config", rect: { left: 0, top: 20, right: 300, bottom: 44 } },
    ]);
    const disabledRef: React.RefObject<boolean> = { current: true };
    renderHook(() => useNavFileDrop({ getScrollElement: () => el, disabledRef, onDrop }));

    dispatch({ type: "drop", paths: ["/Users/me/a.md"], position: { x: 10, y: 30 } });

    expect(onDrop).not.toHaveBeenCalled();
  });

  it("does nothing when the nav is unmounted (getScrollElement → null)", () => {
    const onDrop = vi.fn();
    renderHook(() => useNavFileDrop({ getScrollElement: () => null, onDrop }));

    dispatch({ type: "drop", paths: ["/Users/me/a.md"], position: { x: 10, y: 30 } });

    expect(onDrop).not.toHaveBeenCalled();
  });
});
