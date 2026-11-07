// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach } from "vitest";
import { renderHook } from "@testing-library/react";
import {
  useAgentSessionStore,
  selectBinding,
  DEFAULT_SURFACE_BINDING,
  resetAgentSessionStoreForTests,
  buildAgentSurfaceKey,
  useAgentBindingKey,
} from "@/shared/stores/use-agent-session-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { NO_TEAM_SENTINEL, buildTeamKey } from "@/shared/lib/team-key";

beforeEach(() => resetAgentSessionStoreForTests());

describe("use-agent-session-store (AC-9 per-surface-keyed binding)", () => {
  it("defaults an unknown surface to {agent, disconnected, null, recovery:false}", () => {
    const b = selectBinding("story:A")(useAgentSessionStore.getState());
    expect(b).toEqual(DEFAULT_SURFACE_BINDING);
    expect(b).toEqual({
      boundSessionId: null,
      owner: "agent",
      connection: "disconnected",
      recovery: false,
    });
  });

  it("upserts owner / connection / boundSessionId / recovery for a surface independently", () => {
    const s = useAgentSessionStore.getState();
    s.setOwner("story:A", "user");
    s.setConnection("story:A", "live");
    s.setBoundSessionId("story:A", "shell-1");
    s.setRecovery("story:A", true);
    expect(selectBinding("story:A")(useAgentSessionStore.getState())).toEqual({
      boundSessionId: "shell-1",
      owner: "user",
      connection: "live",
      recovery: true,
    });
  });

  it("recovery is per-surface — one surface's banner never leaks to another", () => {
    const s = useAgentSessionStore.getState();
    s.setRecovery("story:A", true);
    expect(selectBinding("story:A")(useAgentSessionStore.getState()).recovery).toBe(true);
    expect(selectBinding("story:B")(useAgentSessionStore.getState()).recovery).toBe(false);
  });

  it("a no-op write to an existing surface returns the SAME bindings ref (no re-render churn)", () => {
    const s = useAgentSessionStore.getState();
    s.setConnection("story:A", "live"); // materialize the surface
    const before = useAgentSessionStore.getState().bindings;
    s.setConnection("story:A", "live"); // no-op — same value
    s.setRecovery("story:A", false); // no-op — default value
    expect(useAgentSessionStore.getState().bindings).toBe(before); // identity preserved
    // A real change still produces a new reference.
    s.setConnection("story:A", "disconnected");
    expect(useAgentSessionStore.getState().bindings).not.toBe(before);
  });

  it("keeps two surfaces of the same KIND fully isolated (per-instance, no collision)", () => {
    const s = useAgentSessionStore.getState();
    s.setBoundSessionId("story:A", "shell-A");
    s.setConnection("story:A", "live");
    s.setBoundSessionId("story:B", "shell-B");
    s.setConnection("story:B", "reconnecting");

    const a = selectBinding("story:A")(useAgentSessionStore.getState());
    const b = selectBinding("story:B")(useAgentSessionStore.getState());
    expect(a).toEqual({
      boundSessionId: "shell-A",
      owner: "agent",
      connection: "live",
      recovery: false,
    });
    expect(b).toEqual({
      boundSessionId: "shell-B",
      owner: "agent",
      connection: "reconnecting",
      recovery: false,
    });
  });

  it("clearSurface drops one surface without touching the others", () => {
    const s = useAgentSessionStore.getState();
    s.setBoundSessionId("story:A", "shell-A");
    s.setBoundSessionId("story:B", "shell-B");
    s.clearSurface("story:A");
    expect(selectBinding("story:A")(useAgentSessionStore.getState())).toEqual(
      DEFAULT_SURFACE_BINDING,
    );
    expect(selectBinding("story:B")(useAgentSessionStore.getState()).boundSessionId).toBe(
      "shell-B",
    );
  });
});

describe("buildAgentSurfaceKey (SW-AW-6 AC-8 — the single team-scoped key source of truth)", () => {
  it("composes targetWorkspace with teamKey over a `::` boundary", () => {
    expect(buildAgentSurfaceKey("test-agent", "acme:default")).toBe("test-agent::acme:default");
    expect(buildAgentSurfaceKey("notes", "acme:default")).toBe("notes::acme:default");
  });

  it("two teams → two distinct keys for the same targetWorkspace (no cross-team collision)", () => {
    const a = buildAgentSurfaceKey("test-agent", buildTeamKey("acme", "teamA"));
    const b = buildAgentSurfaceKey("test-agent", buildTeamKey("acme", "teamB"));
    expect(a).not.toBe(b);
  });

  it("no-team window collapses to a stable sentinel-bearing key", () => {
    expect(buildAgentSurfaceKey("test-agent", NO_TEAM_SENTINEL)).toBe(
      `test-agent::${NO_TEAM_SENTINEL}`,
    );
  });
});

describe("team-scoped surfaceKey has ONE source of truth (SW-AW-6 AC-8 DRY guard)", () => {
  // Comment-stripped source of the key-builder + the binding-store reader.
  const sources = import.meta.glob(
    ["./use-agent-session-store.ts", "../hooks/use-agent-workspace-session.ts"],
    { query: "?raw", import: "default", eager: true },
  ) as Record<string, string>;

  const stripComments = (src: string): string =>
    src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");

  const codeOf = (suffix: string): string => {
    const entry = Object.entries(sources).find(([p]) => p.endsWith(suffix));
    if (!entry) throw new Error(`source not globbed: ${suffix}`);
    return stripComments(entry[1]);
  };

  it("globbed the builder + the reader (guards against a silent no-match)", () => {
    expect(Object.keys(sources)).toHaveLength(2);
  });

  it("only buildAgentSurfaceKey composes the `::` key — the reader delegates to it", () => {
    // The builder owns the one `::` composition.
    expect(codeOf("use-agent-session-store.ts")).toContain("::");
    // The reader may not re-inline it (drift = the cross-team-stale bug reborn).
    expect(codeOf("use-agent-workspace-session.ts")).not.toContain("::");
  });

  it("the reader routes through the shared key helper (useAgentBindingKey)", () => {
    expect(codeOf("use-agent-workspace-session.ts")).toContain("useAgentBindingKey");
  });
});

describe("useAgentBindingKey (SW-AW-6 AC-1/AC-8 — reactive team-scoped key)", () => {
  beforeEach(() => {
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
  });

  it("returns the sentinel-scoped key when no org is active", () => {
    const { result } = renderHook(() => useAgentBindingKey("test-agent"));
    expect(result.current).toBe(`test-agent::${NO_TEAM_SENTINEL}`);
  });

  it("scopes to the ACTIVE team and re-keys on a team switch (reactive, no remount)", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "teamA" });
    const { result, rerender } = renderHook(() => useAgentBindingKey("test-agent"));
    expect(result.current).toBe(`test-agent::${buildTeamKey("acme", "teamA")}`);

    // Flip the active team — the SAME hook instance must re-derive the key.
    useOrgStore.setState({ activeTeamName: "teamB" });
    rerender();
    expect(result.current).toBe(`test-agent::${buildTeamKey("acme", "teamB")}`);
  });

  it("teamScoped:false (user-global agent) → bare key, UNCHANGED across a team switch", () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "teamA" });
    const { result, rerender } = renderHook(() => useAgentBindingKey("notes", false));
    expect(result.current).toBe("notes"); // no `::teamKey` suffix

    useOrgStore.setState({ activeTeamName: "teamB" });
    rerender();
    expect(result.current).toBe("notes"); // still global — a switch does not re-key
  });
});
