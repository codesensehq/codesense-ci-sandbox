// SPDX-License-Identifier: MIT
import { renderHook } from "@testing-library/react";
import { Workflow } from "lucide-react";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { beforeEach, describe, expect, it } from "vitest";

import {
  getPanel,
  getScopedPanel,
  listPanels,
  registerPanel,
  unregisterPanel,
  usePanelRegistry,
  usePanelsFor,
  useRegisterPanel,
} from "./panel-registry";

beforeEach(() => {
  usePanelRegistry.setState({
    panels: {},
    shared: {},
    scoped: {},
    activeScope: null,
    viewScope: null,
  });
});

const stub = (id: string) => ({ id, title: id, icon: Workflow, render: () => null });

describe("scoped panels", () => {
  it("shows the active scope's panels with the shared ones, and keeps another team's registered", () => {
    registerPanel(stub("shared"));
    registerPanel(stub("a-only"), "o:a");
    registerPanel(stub("b-only"), "o:b");
    usePanelRegistry.getState().setActiveScope("o:a");
    usePanelRegistry.getState().settleScope("o:a");
    expect(listPanels().map((p) => p.id)).toEqual(["shared", "a-only"]);
    usePanelRegistry.getState().setActiveScope("o:b");
    expect(listPanels().map((p) => p.id)).toEqual(["shared", "b-only"]);
    expect(getScopedPanel("o:a", "a-only")).toBeDefined();
  });

  it("a scope still listing keeps the previous view until it settles", () => {
    registerPanel(stub("a-only"), "o:a");
    usePanelRegistry.getState().setActiveScope("o:a");
    usePanelRegistry.getState().settleScope("o:a");
    usePanelRegistry.getState().setActiveScope("o:cold");
    expect(listPanels().map((p) => p.id)).toEqual(["a-only"]);
    usePanelRegistry.getState().settleScope("o:cold");
    expect(listPanels()).toEqual([]);
  });

  it("a hidden team's own pane reads its panels while another team is active", () => {
    registerPanel(stub("a-only"), "o:a");
    registerPanel(stub("b-only"), "o:b");
    usePanelRegistry.getState().setActiveScope("o:b");
    usePanelRegistry.getState().settleScope("o:b");
    const { result } = renderHook(() => usePanelsFor("o:a"));
    expect(Object.keys(result.current)).toEqual(["a-only"]);
  });

  it("drops a scope's panels when the team leaves the warm tier", () => {
    warmScopes.forget("o:a");
    warmScopes.activate("o:a");
    registerPanel(stub("a-only"), "o:a");
    warmScopes.activate("o:b");
    warmScopes.forget("o:a");
    expect(getScopedPanel("o:a", "a-only")).toBeUndefined();
    warmScopes.forget("o:b");
  });
});

describe("panel-registry", () => {
  it("registers and looks up a panel by id", () => {
    registerPanel({ id: "alpha", title: "Alpha", icon: Workflow, render: () => null });
    expect(getPanel("alpha")?.title).toBe("Alpha");
  });

  it("returns undefined for an unregistered id (no crash)", () => {
    expect(getPanel("missing")).toBeUndefined();
  });

  it("lists panels in registration order", () => {
    registerPanel({ id: "a", title: "A", icon: Workflow, render: () => null });
    registerPanel({ id: "b", title: "B", icon: Workflow, render: () => null });
    expect(listPanels().map((p) => p.id)).toEqual(["a", "b"]);
  });

  it("unregister removes a panel and is a no-op for unknown ids", () => {
    registerPanel({ id: "a", title: "A", icon: Workflow, render: () => null });
    unregisterPanel("a");
    expect(getPanel("a")).toBeUndefined();
    expect(() => unregisterPanel("nope")).not.toThrow();
  });

  it("stores an optional headerActions on the definition", () => {
    const headerActions = () => null;
    registerPanel({ id: "h", title: "H", icon: Workflow, render: () => null, headerActions });
    expect(getPanel("h")?.headerActions).toBe(headerActions);
  });
});

describe("useRegisterPanel", () => {
  it("registers on mount and unregisters on unmount", () => {
    const def = { id: "live", title: "Live", icon: Workflow, render: () => null };
    const { unmount } = renderHook(() => useRegisterPanel(def));
    expect(getPanel("live")?.title).toBe("Live");
    unmount();
    expect(getPanel("live")).toBeUndefined();
  });

  it("keeps a stable def registered across re-renders (no churn-out)", () => {
    const def = { id: "stable", title: "Stable", icon: Workflow, render: () => null };
    const { rerender } = renderHook(() => useRegisterPanel(def));
    rerender();
    rerender();
    expect(getPanel("stable")?.title).toBe("Stable");
  });

  it("replaces the definition when def identity changes", () => {
    const first = { id: "x", title: "First", icon: Workflow, render: () => null };
    const second = { id: "x", title: "Second", icon: Workflow, render: () => null };
    const { rerender } = renderHook(({ d }) => useRegisterPanel(d), {
      initialProps: { d: first },
    });
    expect(getPanel("x")?.title).toBe("First");
    rerender({ d: second });
    expect(getPanel("x")?.title).toBe("Second");
  });

  it("drops the stale id when the def's id changes (no leak)", () => {
    const a = { id: "old", title: "Old", icon: Workflow, render: () => null };
    const b = { id: "new", title: "New", icon: Workflow, render: () => null };
    const { rerender } = renderHook(({ d }) => useRegisterPanel(d), { initialProps: { d: a } });
    expect(getPanel("old")?.title).toBe("Old");
    rerender({ d: b });
    expect(getPanel("old")).toBeUndefined();
    expect(getPanel("new")?.title).toBe("New");
  });

  it("unmount does not clobber a same-id panel a later component still owns", () => {
    const first = { id: "dup", title: "First", icon: Workflow, render: () => null };
    const second = { id: "dup", title: "Second", icon: Workflow, render: () => null };
    const hookA = renderHook(() => useRegisterPanel(first));
    const hookB = renderHook(() => useRegisterPanel(second));
    // B registered last → it owns the id.
    expect(getPanel("dup")?.title).toBe("Second");
    // A unmounts first → must NOT remove B's live registration.
    hookA.unmount();
    expect(getPanel("dup")?.title).toBe("Second");
    // B unmounts → now the id is gone.
    hookB.unmount();
    expect(getPanel("dup")).toBeUndefined();
  });
});
