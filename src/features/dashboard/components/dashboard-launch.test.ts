// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  openStoryWorkspace,
  openBranchWorkspace,
  openDirWorkspace,
  branchHandle,
  branchTargetHandle,
  REVIEW_PHASE_TIMEOUT_MS,
  RESERVATION_BIND_TIMEOUT_MS,
  titleCaseLabel,
  reserveForkLabel,
  runPrompt,
  sendToStorySession,
  closeStorySession,
  substituteStoryPath,
  splitStoryHandle,
  normalizeInitialPrompt,
  setDashboardLaunchController,
  getDashboardLaunchController,
  type DashboardLaunchDeps,
  type BranchLaunchDeps,
  type BranchResolution,
} from "./dashboard-launch";
import { useSurfaceSyncStore } from "@/features/shell/stores/surface-sync-store";
import { awaitContextForLaunch } from "@/features/viewer/lib/await-worktree-context";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

/** A bind that stamps the launch's team. */
const scoped = expect.objectContaining({ teamKey: expect.any(String) });

// A deps factory: happy-path defaults + a shared `calls` log to assert ORDER
// (the seam ordering is load-bearing — cap before spawn,
// bind after spawn).
function makeDeps(
  over: Partial<DashboardLaunchDeps> = {},
  opts: { fresh?: boolean } = {},
): {
  deps: DashboardLaunchDeps;
  calls: string[];
} {
  const calls: string[] = [];
  const deps: DashboardLaunchDeps = {
    resolveStory: vi.fn(async (id: string) => {
      calls.push("resolveStory");
      return { storyFilePath: `/managed/${id}.md`, teamKey: "org:team" };
    }),
    begin: vi.fn(async () => {
      calls.push("begin");
      return opts.fresh ?? true;
    }),
    abort: vi.fn(async () => {
      calls.push("abort");
    }),
    killSession: vi.fn(async () => {
      calls.push("killSession");
    }),
    ensureWorktree: vi.fn(async () => {
      calls.push("ensureWorktree");
      return "/wt/cwd";
    }),
    connect: vi.fn(async () => {
      calls.push("connect");
      return { sessionId: "sess-1" };
    }),
    bind: vi.fn(async () => {
      calls.push("bind");
    }),
    waitLive: vi.fn(async () => {
      calls.push("waitLive");
      return true;
    }),
    focus: vi.fn(() => {
      calls.push("focus");
    }),
    resolveSession: vi.fn(async () => {
      calls.push("resolveSession");
      return "sess-1";
    }),
    injectLine: vi.fn(async () => {
      calls.push("injectLine");
    }),
    // SW-DX-10 seams — happy-path defaults.
    sendLine: vi.fn(async () => {
      calls.push("sendLine");
      return { submitted: true };
    }),
    isSessionParked: vi.fn(() => {
      calls.push("isSessionParked");
      return false;
    }),
    echoUserTurn: vi.fn(() => {
      calls.push("echoUserTurn");
    }),
    // SW-GW-5 seams — default: a bound session is LIVE (alive, not parked).
    probeAlive: vi.fn(async () => {
      calls.push("probeAlive");
      return true;
    }),
    evictStaleTab: vi.fn(async () => {
      calls.push("evictStaleTab");
    }),
    // SW-GW-10 close-half seam — happy-path default (kill succeeds).
    closeByHandle: vi.fn(async () => {
      calls.push("closeByHandle");
    }),
    ...over,
  };
  return { deps, calls };
}

describe("@P1 dashboard-launch — pure helpers", () => {
  it("substituteStoryPath fills ONLY the {storyPath} token, verbatim otherwise (AC-6)", () => {
    // The token is replaced (every occurrence); all other text passes through unchanged.
    expect(substituteStoryPath("/bmad-code-review {storyPath}", "/managed/SW-DX-7.md")).toBe(
      "/bmad-code-review /managed/SW-DX-7.md",
    );
    // No token → returned verbatim (arbitrary prompt).
    expect(substituteStoryPath("focus on AC-3 and re-run", "/managed/x.md")).toBe(
      "focus on AC-3 and re-run",
    );
    // Multiple tokens all fill; nothing else is interpreted.
    expect(substituteStoryPath("{storyPath} vs {storyPath}", "/m/a.md")).toBe("/m/a.md vs /m/a.md");
  });

  it("splitStoryHandle parses target → {storyId, label} on the FIRST # (AC-1/AC-2/AC-7)", () => {
    // Bare id → main session (no label).
    expect(splitStoryHandle("SW-DX-7")).toEqual({ storyId: "SW-DX-7", label: undefined });
    // Compound handle → named session.
    expect(splitStoryHandle("SW-DX-7#architecture-review")).toEqual({
      storyId: "SW-DX-7",
      label: "architecture-review",
    });
    // storyId is ALWAYS the pre-# slice (never carries a #); a 2nd # falls into the
    // label half, where the slug validator rejects it.
    expect(splitStoryHandle("a#b#c")).toEqual({ storyId: "a", label: "b#c" });
    // Trailing # → empty label (treated as absent/main session downstream).
    expect(splitStoryHandle("SW-DX-7#")).toEqual({ storyId: "SW-DX-7", label: "" });
  });

  it("normalizeInitialPrompt treats empty/whitespace/non-string as absent (AC-8)", () => {
    // A real prompt passes through verbatim (whitespace preserved once non-blank).
    expect(normalizeInitialPrompt("/bmad-dev-story x")).toBe("/bmad-dev-story x");
    expect(normalizeInitialPrompt("  keep inner  ")).toBe("  keep inner  ");
    // Blank / absent / wrong-type → undefined ("no prompt asked").
    expect(normalizeInitialPrompt("")).toBeUndefined();
    expect(normalizeInitialPrompt("   ")).toBeUndefined();
    expect(normalizeInitialPrompt("\n\t")).toBeUndefined();
    expect(normalizeInitialPrompt(undefined)).toBeUndefined();
    expect(normalizeInitialPrompt(42)).toBeUndefined();
  });
});

describe("@P1 openStoryWorkspace", () => {
  it("refuses focus when the requested account differs from immutable provenance", async () => {
    const { deps } = makeDeps({
      resolveSession: vi.fn(async () => "sess-1"),
      resolveAccountProvenance: vi.fn(async () => ({
        runtime: "claude" as const,
        provider: "claude",
        source: "explicit",
        reason: "explicit-override",
        accountId: "account-a",
        label: "Account A",
      })),
    });
    await expect(
      openStoryWorkspace("SW-DX-7", deps, {
        account: { userId: 1, id: "account-b", generation: 2 },
      }),
    ).rejects.toThrow("already-running-with-different-account");
    expect(deps.begin).not.toHaveBeenCalled();
    expect(deps.connect).not.toHaveBeenCalled();
  });

  describe("a re-open that names no account, on a session started with another account", () => {
    const driftDeps = (over: Partial<DashboardLaunchDeps> = {}) =>
      makeDeps(
        {
          resolveSession: vi.fn(async () => "sess-1"),
          resolveAccountProvenance: vi.fn(async () => ({
            runtime: "claude" as const,
            provider: "claude",
            source: "explicit",
            reason: "explicit-override",
            accountId: "account-a",
            label: "Account A",
          })),
          ...over,
        },
        { fresh: false },
      );

    it("reuses and focuses the live session", async () => {
      const { deps } = driftDeps();
      const result = await openStoryWorkspace("SW-DX-7", deps);
      expect(result).toEqual({ sessionId: "sess-1" });
      expect(deps.focus).toHaveBeenCalledWith("org:team", "sess-1", "/managed/SW-DX-7.md");
      expect(deps.connect).not.toHaveBeenCalled();
    });

    it("does not steal focus for a headless re-open and reports an undelivered seed", async () => {
      const { deps } = driftDeps();
      const result = await openStoryWorkspace("SW-DX-7", deps, {
        headed: false,
        initialPrompt: "go",
      });
      expect(result).toEqual({ sessionId: "sess-1", promptDelivered: false });
      expect(deps.focus).not.toHaveBeenCalled();
      expect(deps.connect).not.toHaveBeenCalled();
    });

    it("replaces a dead session under the current account instead of handing it back", async () => {
      const { deps } = driftDeps({
        probeAlive: vi.fn(async () => false),
        connect: vi.fn(async () => ({ sessionId: "sess-2" })),
      });
      const result = await openStoryWorkspace("SW-DX-7", deps);
      expect(result.sessionId).toBe("sess-2");
      // No account named: the agent launches with the account its own binding resolves.
      expect(deps.connect).toHaveBeenCalledWith(
        expect.objectContaining({
          agent: { agent: "builtin/claude", prompt: undefined, account: undefined },
        }),
      );
      expect(deps.bind).toHaveBeenCalledWith("SW-DX-7", "sess-2", expect.anything());
    });
  });

  it("uses backend-owned committed provenance when binding", async () => {
    const { deps } = makeDeps({
      resolveSession: vi.fn(async () => null),
      connect: vi.fn(async () => ({ sessionId: "sess-1" })),
    });
    await openStoryWorkspace("SW-DX-7", deps, {
      account: { userId: 1, id: "account-a", generation: 1 },
    });
    // The named account is the agent's for this launch only; where it was launched rides
    // the bind, so the session reaches the routines in its team.
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({
        agent: {
          agent: "builtin/claude",
          prompt: undefined,
          account: { id: "account-a", generation: 1 },
        },
      }),
    );
    expect(deps.bind).toHaveBeenCalledWith("SW-DX-7", "sess-1", {
      teamKey: "org:team",
      agentRevision: undefined,
    });
  });

  it("runs the seams in order: resolve → begin → worktree → connect → bind → waitLive → focus", async () => {
    const { deps, calls } = makeDeps();
    const res = await openStoryWorkspace("SW-DX-7", deps);
    expect(res).toEqual({ sessionId: "sess-1" });
    // fresh=true so it does NOT consult resolveSession (that's the idempotent path).
    expect(calls).toEqual([
      "resolveStory",
      "begin",
      "ensureWorktree",
      "connect",
      "bind",
      "waitLive",
      "focus",
    ]);
  });

  it("waits for the worktree's in-flight establish to RESOLVE before the spawn", async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const { deps, calls } = makeDeps({
      awaitEstablished: vi.fn(async (cwd: string) => {
        calls.push(`awaitEstablished:${cwd}`);
        await gate;
        calls.push("established");
      }),
    });
    const launch = openStoryWorkspace("SW-DX-7", deps);
    await vi.waitFor(() => expect(calls).toContain("awaitEstablished:/wt/cwd"));
    // Still blocked on establish: the spawn must not have run.
    await Promise.resolve();
    expect(calls).not.toContain("connect");
    release();
    await launch;
    expect(calls.indexOf("ensureWorktree")).toBeLessThan(calls.indexOf("awaitEstablished:/wt/cwd"));
    expect(calls.indexOf("established")).toBeLessThan(calls.indexOf("connect"));
  });

  it("sends the launch line only after repository setup completes", async () => {
    // The real wait over the real stores: a freshly created story worktree is
    // latched (`needsSyncOverlay`) until its materialize wave settles.
    const cwd = "/wt/cwd";
    useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
    useViewerStore.setState({
      workspaces: {
        "org:team": [
          {
            id: "SW-DX-7",
            kind: "story",
            cwd,
            status: "ready",
            backgroundSetup: { status: "running", label: "Setup" },
          },
        ] as unknown as Workspace[],
      },
    });
    const { deps, calls } = makeDeps({ awaitContext: (dir) => awaitContextForLaunch(dir) });
    const launch = openStoryWorkspace("SW-DX-7", deps, { initialPrompt: "Read the story" });
    await vi.waitFor(() => expect(calls).toContain("ensureWorktree"));
    // The wave starts: still landing, nothing spawned, no prompt typed.
    useSurfaceSyncStore.getState().markStarted(cwd, 1);
    for (let i = 0; i < 10; i++) await Promise.resolve();
    expect(calls).not.toContain("connect");
    expect(calls).not.toContain("sendLine");
    useViewerStore.setState({
      workspaces: { "org:team": [{ id: "SW-DX-7", kind: "story", cwd, status: "ready" }] },
    });
    await launch;
    expect(calls).toContain("sendLine");
    useViewerStore.setState({ workspaces: {} });
    useSurfaceSyncStore.setState({ byPath: {} });
  });

  it.each(["awaitEstablished", "awaitContext"] as const)(
    "blocks process spawn when %s fails",
    async (boundary) => {
      const { deps, calls } = makeDeps({
        [boundary]: vi.fn(async () => {
          throw new Error("setup unavailable");
        }),
      });
      await expect(openStoryWorkspace("SW-DX-7", deps)).rejects.toThrow("setup unavailable");
      expect(calls).not.toContain("connect");
      expect(calls).not.toContain("sendLine");
    },
  );

  it("does not wait for context on a worktree that isn't landing (reopen / already there)", async () => {
    useSurfaceSyncStore.setState({ byPath: {}, assemblyFailure: {} });
    useViewerStore.setState({ workspaces: {} });
    const { deps, calls } = makeDeps({ awaitContext: (dir) => awaitContextForLaunch(dir) });
    await openStoryWorkspace("SW-DX-7", deps);
    expect(calls).toContain("connect");
  });

  it("does not wait on establish when the worktree did not open", async () => {
    const awaitEstablished = vi.fn(async () => {});
    const { deps } = makeDeps({ ensureWorktree: vi.fn(async () => ""), awaitEstablished });
    await expect(openStoryWorkspace("SW-DX-7", deps)).rejects.toThrow();
    expect(awaitEstablished).not.toHaveBeenCalled();
  });

  it("reserves the cap BEFORE any worktree/spawn work", async () => {
    const { deps, calls } = makeDeps();
    await openStoryWorkspace("SW-DX-7", deps);
    expect(calls.indexOf("begin")).toBeLessThan(calls.indexOf("ensureWorktree"));
    expect(calls.indexOf("begin")).toBeLessThan(calls.indexOf("connect"));
  });

  it("rejects (and never reserves) when the story can't be resolved to a managed path (AC-11)", async () => {
    const { deps } = makeDeps({ resolveStory: vi.fn(async () => null) });
    await expect(openStoryWorkspace("bogus", deps)).rejects.toThrow(/unavailable: cannot resolve/);
    expect(deps.resolveStory).toHaveBeenCalled();
    expect(deps.begin).not.toHaveBeenCalled();
  });

  it("propagates `busy` from a saturated cap and does not open a worktree", async () => {
    const { deps, calls } = makeDeps({
      begin: vi.fn(async () => {
        throw new Error("busy");
      }),
    });
    await expect(openStoryWorkspace("SW-DX-7", deps)).rejects.toThrow("busy");
    expect(calls).not.toContain("ensureWorktree");
    // begin threw before reserving, so there is nothing to abort.
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("aborts when the worktree does not open (empty cwd)", async () => {
    const { deps } = makeDeps({ ensureWorktree: vi.fn(async () => "") });
    await expect(openStoryWorkspace("SW-DX-7", deps)).rejects.toThrow(/worktree did not open/);
    expect(deps.abort).toHaveBeenCalledWith("SW-DX-7");
    expect(deps.connect).not.toHaveBeenCalled();
  });

  it("AC-4 idempotent focus for a LIVE session: re-focuses (after the liveness gate) and does NOT spawn", async () => {
    const { deps, calls } = makeDeps({}, { fresh: false });
    const res = await openStoryWorkspace("SW-DX-7", deps);
    expect(res).toEqual({ sessionId: "sess-1" });
    // SW-GW-5: reuse is now gated on liveness — parked-check (sync) then a real
    // alive probe — before re-focusing the still-live session.
    expect(calls).toEqual([
      "resolveStory",
      "begin",
      "resolveSession",
      "isSessionParked",
      "probeAlive",
      "focus",
    ]);
    expect(deps.connect).not.toHaveBeenCalled();
    expect(deps.evictStaleTab).not.toHaveBeenCalled();
    expect(deps.killSession).not.toHaveBeenCalled();
    expect(deps.abort).not.toHaveBeenCalled();
  });

  // ── SW-GW-5: guaranteed-live re-open (self-heal a parked/dead session) ──────
  it("AC-1 re-open after a FINISHED (parked) run heals: evict stale tab → spawn fresh → rebind → reap", async () => {
    const { deps, calls } = makeDeps(
      {
        resolveSession: vi.fn(async () => {
          calls.push("resolveSession");
          return "parked-1";
        }),
        isSessionParked: vi.fn(() => {
          calls.push("isSessionParked");
          return true; // terminal managed state (done / needs-attention)
        }),
        connect: vi.fn(async () => {
          calls.push("connect");
          return { sessionId: "fresh-2" };
        }),
      },
      { fresh: false },
    );
    const res = await openStoryWorkspace("SW-DX-7", deps);
    // A brand-new session id, not the parked corpse (AC-1).
    expect(res).toEqual({ sessionId: "fresh-2" });
    // Parked short-circuits the async probe (sync gate first — no IPC on the
    // common finished-run case).
    expect(deps.probeAlive).not.toHaveBeenCalled();
    // Exact order: evict BEFORE connect (so the launcher dedup misses), reap the
    // stale id AFTER bind (so its release can't free the live permit).
    expect(calls).toEqual([
      "resolveStory",
      "begin",
      "resolveSession",
      "isSessionParked",
      "ensureWorktree",
      "evictStaleTab",
      "connect",
      "bind",
      "killSession",
      "waitLive",
      "focus",
    ]);
    expect(deps.evictStaleTab).toHaveBeenCalledWith("parked-1");
    expect(deps.bind).toHaveBeenCalledWith("SW-DX-7", "fresh-2", scoped);
    expect(deps.killSession).toHaveBeenCalledWith("parked-1");
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("AC-2 re-open after a CRASHED run (dead PTY, not parked) heals via the alive probe", async () => {
    const { deps, calls } = makeDeps(
      {
        resolveSession: vi.fn(async () => {
          calls.push("resolveSession");
          return "dead-1";
        }),
        // Not parked, but the PTY child has exited (managed state hasn't flipped).
        probeAlive: vi.fn(async () => {
          calls.push("probeAlive");
          return false;
        }),
        connect: vi.fn(async () => {
          calls.push("connect");
          return { sessionId: "fresh-2" };
        }),
      },
      { fresh: false },
    );
    const res = await openStoryWorkspace("SW-DX-7", deps);
    expect(res).toEqual({ sessionId: "fresh-2" });
    // Both predicates ran (parked false → probe → dead) → heal. This is why both
    // predicates are required, not just isSessionParked.
    expect(calls).toEqual([
      "resolveStory",
      "begin",
      "resolveSession",
      "isSessionParked",
      "probeAlive",
      "ensureWorktree",
      "evictStaleTab",
      "connect",
      "bind",
      "killSession",
      "waitLive",
      "focus",
    ]);
    expect(deps.evictStaleTab).toHaveBeenCalledWith("dead-1");
    expect(deps.killSession).toHaveBeenCalledWith("dead-1");
  });

  it("a pre-connect failure on a heal re-open leaves the stale tab UNEVICTED and does not abort a non-owned permit", async () => {
    const { deps } = makeDeps(
      {
        resolveSession: vi.fn(async () => "parked-1"),
        isSessionParked: vi.fn(() => true),
        ensureWorktree: vi.fn(async () => {
          throw new Error("unavailable: the worktree did not open");
        }),
      },
      { fresh: false },
    );
    await expect(openStoryWorkspace("SW-DX-7", deps)).rejects.toThrow(/unavailable/);
    // Deferred eviction: a worktree failure leaves the stale session's
    // tab untouched (it self-heals on the next attempt), and never spawns.
    expect(deps.evictStaleTab).not.toHaveBeenCalled();
    expect(deps.connect).not.toHaveBeenCalled();
    // Non-fresh caller: never abort/reap a permit it doesn't own.
    expect(deps.abort).not.toHaveBeenCalled();
    expect(deps.killSession).not.toHaveBeenCalled();
  });

  it("does NOT abort a live launch it doesn't own (non-fresh caller failing)", async () => {
    // A concurrent owner binds while this caller waits; a later focus failure
    // must not abort the real owner's permit.
    const { deps } = makeDeps(
      {
        resolveSession: vi.fn().mockResolvedValueOnce(null).mockResolvedValue("sess-1"),
        focus: vi.fn(() => {
          throw new Error("spawn boom");
        }),
      },
      { fresh: false },
    );
    await expect(openStoryWorkspace("SW-DX-7", deps)).rejects.toThrow("spawn boom");
    expect(deps.abort).not.toHaveBeenCalled();
    // SW-DX-17b (AC-8): the non-owner also must not reap a session it doesn't own.
    expect(deps.killSession).not.toHaveBeenCalled();
  });

  // ── SW-DX-17b: the `headed:false` background tier ──────────────────────────
  it("AC-1 headless open runs the full launch but SUPPRESSES focus (no console swap)", async () => {
    const { deps, calls } = makeDeps();
    const res = await openStoryWorkspace("SW-DX-7", deps, { headed: false });
    // The session is still spawned + bound (first-class + adoptable via SW-DX-17a);
    // only the terminal-stealing focus step is skipped.
    expect(res).toEqual({ sessionId: "sess-1" });
    expect(calls).toEqual([
      "resolveStory",
      "begin",
      "ensureWorktree",
      "connect",
      "bind",
      "waitLive",
    ]);
    expect(deps.focus).not.toHaveBeenCalled();
  });

  it("default (headed) open still focuses — headless is opt-in", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-DX-7", deps, {}); // no headed → default true
    expect(deps.focus).toHaveBeenCalledTimes(1);
  });

  it("AC-1 headless idempotent re-open of an already-open LIVE story does NOT steal focus", async () => {
    const { deps, calls } = makeDeps({}, { fresh: false });
    const res = await openStoryWorkspace("SW-DX-7", deps, { headed: false });
    expect(res).toEqual({ sessionId: "sess-1" });
    expect(calls).toEqual([
      "resolveStory",
      "begin",
      "resolveSession",
      "isSessionParked",
      "probeAlive",
    ]);
    expect(deps.focus).not.toHaveBeenCalled();
    expect(deps.connect).not.toHaveBeenCalled();
  });

  it("AC-8 reaps the orphaned session when bind fails AFTER connect spawned it", async () => {
    // The race: connect returns a LIVE session, then bind throws (e.g. a concurrent
    // kill aborted the reserved row). `abort` alone frees the slot but leaves the
    // PTY running with no handle → invisible + unkillable. The owner must reap it.
    const { deps } = makeDeps({
      connect: vi.fn(async () => ({ sessionId: "orphan-9" })),
      bind: vi.fn(async () => {
        throw new Error("bind lost the race");
      }),
    });
    await expect(openStoryWorkspace("SW-DX-7", deps, { headed: false })).rejects.toThrow(
      "bind lost the race",
    );
    expect(deps.abort).toHaveBeenCalledTimes(1);
    expect(deps.killSession).toHaveBeenCalledWith("orphan-9");
  });

  it("AC-8 a failed reap does not mask the original launch error", async () => {
    const { deps } = makeDeps({
      connect: vi.fn(async () => ({ sessionId: "orphan-9" })),
      bind: vi.fn(async () => {
        throw new Error("bind boom");
      }),
      killSession: vi.fn(async () => {
        throw new Error("kill also failed");
      }),
    });
    // The ORIGINAL error surfaces, not the reap failure.
    await expect(openStoryWorkspace("SW-DX-7", deps, { headed: false })).rejects.toThrow(
      "bind boom",
    );
    expect(deps.killSession).toHaveBeenCalledWith("orphan-9");
  });

  it("SW-GW-5: a HEAL whose bind fails reaps the freshly-spawned orphan even though it's a !fresh re-open (code-review r1)", async () => {
    // A heal only runs on a bound handle → `begin` returns false → fresh === false.
    // The reap must NOT be gated on `fresh`: otherwise a bind failure mid-heal
    // strands a running, unbound, unseen PTY (the fresh orphan). Regression guard.
    const { deps } = makeDeps(
      {
        resolveSession: vi.fn(async () => "parked-1"),
        isSessionParked: vi.fn(() => true), // → heal path
        connect: vi.fn(async () => ({ sessionId: "fresh-orphan" })),
        bind: vi.fn(async () => {
          throw new Error("bind boom");
        }),
      },
      { fresh: false },
    );
    await expect(openStoryWorkspace("SW-DX-7", deps)).rejects.toThrow("bind boom");
    // The fresh orphan is reaped (PTY killed + tab evicted) despite !fresh…
    expect(deps.killSession).toHaveBeenCalledWith("fresh-orphan");
    expect(deps.evictStaleTab).toHaveBeenCalledWith("fresh-orphan");
    // …but a permit we don't own is never aborted.
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("does NOT reap when nothing was spawned (failure before connect)", async () => {
    const { deps } = makeDeps({
      ensureWorktree: vi.fn(async () => {
        throw new Error("unavailable: no worktree");
      }),
    });
    await expect(openStoryWorkspace("SW-DX-7", deps)).rejects.toThrow(/unavailable/);
    expect(deps.abort).toHaveBeenCalledTimes(1);
    expect(deps.killSession).not.toHaveBeenCalled();
  });

  it("AC-8 a best-effort waitLive failure AFTER bind never reaps the bound session (open still succeeds)", async () => {
    // The session is COMMITTED at bind — a later probe/IPC hiccup must not tear down
    // a live, registered agent (would kill a good session on a transient error).
    const { deps } = makeDeps({
      waitLive: vi.fn(async () => {
        throw new Error("probe IPC hiccup");
      }),
    });
    const res = await openStoryWorkspace("SW-DX-7", deps);
    expect(res).toEqual({ sessionId: "sess-1" });
    expect(deps.killSession).not.toHaveBeenCalled();
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("a focus failure AFTER bind never reaps and never fails the open", async () => {
    const { deps } = makeDeps({
      focus: vi.fn(() => {
        throw new Error("store setter boom");
      }),
    });
    const res = await openStoryWorkspace("SW-DX-7", deps);
    expect(res).toEqual({ sessionId: "sess-1" });
    expect(deps.killSession).not.toHaveBeenCalled();
    expect(deps.abort).not.toHaveBeenCalled();
  });
});

// ── SW-GW-9: initialPrompt-on-launch (seed the spawned agent in one call) ───────
describe("@P1 openStoryWorkspace — initialPrompt seed (SW-GW-9)", () => {
  it("(a) seeds via sendLine AFTER waitLive, focus BEFORE the seed, promptDelivered:true", async () => {
    const { deps, calls } = makeDeps();
    const res = await openStoryWorkspace("SW-DX-7", deps, { initialPrompt: "/bmad-dev-story x" });
    // AC-1: reuse the SHIPPED sendLine with the seed text.
    expect(deps.sendLine).toHaveBeenCalledWith("sess-1", "/bmad-dev-story x");
    // AC-2: the seed lands only AFTER the agent is proven live (never the bare shell).
    expect(calls.indexOf("waitLive")).toBeLessThan(calls.indexOf("sendLine"));
    // Gap B: a headed open paints the terminal (focus) BEFORE the seed round-trip.
    expect(calls.indexOf("focus")).toBeLessThan(calls.indexOf("sendLine"));
    // AC-5: honest delivery — the line landed.
    expect(res).toEqual({ sessionId: "sess-1", promptDelivered: true });
  });

  it("(b) headed:false ALSO seeds (RF-18 background combo) with no focus", async () => {
    const { deps, calls } = makeDeps();
    const res = await openStoryWorkspace("SW-DX-7", deps, {
      headed: false,
      initialPrompt: "/bmad-dev-story x",
    });
    // AC-7: waitLive is not gated on headed, so the seed still fires for a background
    // session; only the terminal-stealing focus is suppressed.
    expect(deps.sendLine).toHaveBeenCalledWith("sess-1", "/bmad-dev-story x");
    expect(deps.focus).not.toHaveBeenCalled();
    expect(calls.indexOf("waitLive")).toBeLessThan(calls.indexOf("sendLine"));
    expect(res).toEqual({ sessionId: "sess-1", promptDelivered: true });
  });

  it("(c) empty/whitespace initialPrompt ≡ omitted — no seed, byte-identical result", async () => {
    const { deps, calls } = makeDeps();
    const res = await openStoryWorkspace("SW-DX-7", deps, { initialPrompt: "   " });
    // AC-8: no injector call, and the result is the bare {sessionId} (promptDelivered
    // ABSENT — "no prompt asked", distinct from false = "asked, not delivered").
    expect(deps.sendLine).not.toHaveBeenCalled();
    expect(calls).not.toContain("sendLine");
    expect(res).toEqual({ sessionId: "sess-1" });
    expect("promptDelivered" in res).toBe(false);
  });

  it("does not type a role-enabled task again when the prepared CLI carries startup on argv", async () => {
    const { deps } = makeDeps({
      connect: vi.fn(async () => ({ sessionId: "sess-1", promptInArgv: true })),
    });
    const result = await openStoryWorkspace("SW-DX-7", deps, {
      initialPrompt: "/review queued work",
    });
    expect(result.promptDelivered).toBe(true);
    expect(deps.sendLine).not.toHaveBeenCalled();
    expect(deps.bind).toHaveBeenCalled();
  });

  it("(d) a held line (sendLine {submitted:false}) → promptDelivered:false, spawn NOT reaped", async () => {
    const { deps } = makeDeps({
      sendLine: vi.fn(async () => ({ submitted: false })),
    });
    const res = await openStoryWorkspace("SW-DX-7", deps, { initialPrompt: "/x" });
    // AC-5: honest degrade — the spawn succeeded, the seed did not land, and nothing
    // was reaped or thrown.
    expect(res).toEqual({ sessionId: "sess-1", promptDelivered: false });
    expect(deps.killSession).not.toHaveBeenCalled();
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("(e) a waitLive throw → promptDelivered:false, still returns {sessionId} (best-effort)", async () => {
    const { deps } = makeDeps({
      waitLive: vi.fn(async () => {
        throw new Error("liveness timeout");
      }),
    });
    const res = await openStoryWorkspace("SW-DX-7", deps, { initialPrompt: "/x" });
    // AC-5: waitLive threw before the seed could run → promptDelivered:false, spawn ok.
    expect(res).toEqual({ sessionId: "sess-1", promptDelivered: false });
    expect(deps.sendLine).not.toHaveBeenCalled();
    expect(deps.killSession).not.toHaveBeenCalled();
  });

  it("(f) idempotent re-open of a LIVE session does NOT re-seed, but honestly reports promptDelivered:false (Gap A)", async () => {
    const { deps, calls } = makeDeps({}, { fresh: false });
    const res = await openStoryWorkspace("SW-DX-7", deps, { initialPrompt: "/x" });
    // Re-injecting into an already-briefed session would be wrong — so no sendLine…
    expect(deps.sendLine).not.toHaveBeenCalled();
    expect(calls).not.toContain("sendLine");
    // …but the requested seed must not vanish silently: report false, not a bare shape.
    expect(res).toEqual({ sessionId: "sess-1", promptDelivered: false });
  });

  it("(AC-4) two concurrent opens with different prompts each seed their OWN session (parallel + neutral)", async () => {
    // Distinct sessions per story; the two launches share nothing (own deps + own PTY).
    const a = makeDeps({ connect: vi.fn(async () => ({ sessionId: "sess-A" })) });
    const b = makeDeps({ connect: vi.fn(async () => ({ sessionId: "sess-B" })) });
    const [ra, rb] = await Promise.all([
      openStoryWorkspace("STORY-A", a.deps, { initialPrompt: "/a" }),
      openStoryWorkspace("STORY-B", b.deps, { initialPrompt: "/b" }),
    ]);
    expect(ra).toEqual({ sessionId: "sess-A", promptDelivered: true });
    expect(rb).toEqual({ sessionId: "sess-B", promptDelivered: true });
    // Each seed lands in its OWN session — no cross-contamination between the launches.
    expect(a.deps.sendLine).toHaveBeenCalledWith("sess-A", "/a");
    expect(b.deps.sendLine).toHaveBeenCalledWith("sess-B", "/b");
    expect(a.deps.sendLine).toHaveBeenCalledTimes(1);
    expect(b.deps.sendLine).toHaveBeenCalledTimes(1);
  });
});

// A stateful `begin` modelling the Rust registry's atomic reserve: a handle is
// reserved (true) the FIRST time; `false` on any repeat (already live — idempotent
// focus, no new slot); throws `busy` once `cap` slots are held. Mirrors
// `dashboard_launch.rs::begin`, so the fork-probe tests exercise real semantics.
function makeProbeBegin(cap = 8): (handle: string) => Promise<boolean> {
  const reserved = new Set<string>();
  return vi.fn(async (handle: string) => {
    if (reserved.has(handle)) return false;
    if (reserved.size >= cap) throw new Error("busy: spawn cap saturated");
    reserved.add(handle);
    return true;
  });
}

describe("@P1 reserveForkLabel — SW-DX-17e begin-probe mint (AC-3/AC-4)", () => {
  it("wins on the first free label (attempt-2), reserving its handle via begin", async () => {
    const begin = vi.fn(async () => true);
    const label = await reserveForkLabel("SW-A", begin);
    expect(label).toBe("attempt-2");
    expect(begin).toHaveBeenCalledTimes(1);
    expect(begin).toHaveBeenCalledWith("SW-A#attempt-2"); // the reserved handle
  });

  it("walks past already-live forks (begin=false) to the next free N", async () => {
    // attempt-2 & attempt-3 already live → attempt-4 is the first free label.
    const live = new Set(["SW-A#attempt-2", "SW-A#attempt-3"]);
    const begin = vi.fn(async (h: string) => !live.has(h));
    const label = await reserveForkLabel("SW-A", begin);
    expect(label).toBe("attempt-4");
    expect(begin).toHaveBeenNthCalledWith(1, "SW-A#attempt-2");
    expect(begin).toHaveBeenNthCalledWith(2, "SW-A#attempt-3");
    expect(begin).toHaveBeenNthCalledWith(3, "SW-A#attempt-4");
  });

  it("propagates `busy` from a saturated cap (never a silent focus-instead-of-fork)", async () => {
    const begin = vi.fn(async () => {
      throw new Error("busy: cap saturated");
    });
    await expect(reserveForkLabel("SW-A", begin)).rejects.toThrow("busy");
  });
});

describe("@P1 openStoryWorkspace — SW-DX-17e sessionMode (focus default / new fork)", () => {
  it("AC-1 sessionMode absent → bare handle, no fork (byte-identical to today)", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-A", deps);
    expect(deps.begin).toHaveBeenCalledWith("SW-A");
    expect(deps.begin).not.toHaveBeenCalledWith("SW-A#attempt-2");
    expect(deps.bind).toHaveBeenCalledWith("SW-A", "sess-1", scoped);
  });

  it("AC-1 sessionMode:'focus' → bare handle, reuse path (explicit focus is a no-op vs default)", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-A", deps, { sessionMode: "focus" });
    expect(deps.begin).toHaveBeenCalledWith("SW-A");
    expect(deps.begin).not.toHaveBeenCalledWith("SW-A#attempt-2");
  });

  it("AC-2/AC-3 sessionMode:'new' → mints attempt-2, spawns a fork, begins ONLY the fork handle (no double-begin)", async () => {
    const { deps } = makeDeps();
    const r = await openStoryWorkspace("SW-A", deps, { sessionMode: "new" });
    expect(r).toEqual({ sessionId: "sess-1" });
    // The probe reserved the fork handle; the spine did NOT re-begin (preReserved).
    expect(deps.begin).toHaveBeenCalledTimes(1);
    expect(deps.begin).toHaveBeenCalledWith("SW-A#attempt-2");
    expect(deps.begin).not.toHaveBeenCalledWith("SW-A");
    // It SPAWNED (bound the fork handle) — never took the idempotent-focus branch.
    expect(deps.connect).toHaveBeenCalledTimes(1);
    expect(deps.bind).toHaveBeenCalledWith("SW-A#attempt-2", "sess-1", scoped);
    // The named connect carries the auto-minted label + humanized title.
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({ session: "attempt-2", title: "Attempt 2" }),
    );
  });

  it("AC-2/AC-3 two 'new' forks of the SAME story mint DISTINCT collision-free handles", async () => {
    const begin = makeProbeBegin();
    const d1 = makeDeps({ begin });
    const d2 = makeDeps({ begin });
    await openStoryWorkspace("SW-A", d1.deps, { sessionMode: "new" });
    await openStoryWorkspace("SW-A", d2.deps, { sessionMode: "new" });
    // First fork took attempt-2; the second saw it live and forked attempt-3.
    expect(d1.deps.bind).toHaveBeenCalledWith("SW-A#attempt-2", "sess-1", scoped);
    expect(d2.deps.bind).toHaveBeenCalledWith("SW-A#attempt-3", "sess-1", scoped);
  });

  it("AC-4 a 'new' fork over a saturated cap rejects `busy` and never spawns", async () => {
    const begin = makeProbeBegin(0); // no free slots
    const { deps } = makeDeps({ begin });
    await expect(openStoryWorkspace("SW-A", deps, { sessionMode: "new" })).rejects.toThrow("busy");
    expect(deps.connect).not.toHaveBeenCalled();
    expect(deps.abort).not.toHaveBeenCalled(); // nothing reserved → nothing to release
  });

  it("an explicit `session` label is authoritative — sessionMode:'new' does NOT auto-mint over it", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-A", deps, { session: "arch-review", sessionMode: "new" });
    expect(deps.begin).toHaveBeenCalledWith("SW-A#arch-review");
    expect(deps.begin).not.toHaveBeenCalledWith("SW-A#attempt-2");
    expect(deps.bind).toHaveBeenCalledWith("SW-A#arch-review", "sess-1", scoped);
  });

  it("resolves the story BEFORE reserving a fork slot — a resolve failure leaks no permit", async () => {
    const begin = makeProbeBegin();
    const { deps } = makeDeps({
      begin,
      resolveStory: vi.fn(async () => null),
    });
    await expect(openStoryWorkspace("SW-A", deps, { sessionMode: "new" })).rejects.toThrow(
      "cannot resolve story",
    );
    expect(begin).not.toHaveBeenCalled(); // never reserved anything
  });

  it("AC-5 a 'new' fork that fails AFTER reserving frees EXACTLY its own slot (no leak)", async () => {
    // reserveForkLabel reserves SW-A#attempt-2 (begin=true), then connect throws.
    // preReserved ⇒ the spine treats this call as the fresh owner and aborts the
    // fork's OWN compound handle — never the main session's, never nothing.
    const { deps } = makeDeps({
      connect: vi.fn(async () => {
        throw new Error("connect boom");
      }),
    });
    await expect(openStoryWorkspace("SW-A", deps, { sessionMode: "new" })).rejects.toThrow(
      "connect boom",
    );
    expect(deps.abort).toHaveBeenCalledTimes(1);
    expect(deps.abort).toHaveBeenCalledWith("SW-A#attempt-2");
  });
});

function makeBranchDeps(
  over: Partial<BranchLaunchDeps> = {},
  opts: { fresh?: boolean } = {},
): { deps: BranchLaunchDeps; calls: string[] } {
  const calls: string[] = [];
  const resolution: BranchResolution = {
    repoPath: "/clones/acme/api",
    teamKey: "org:team",
    codesenseHome: "/home/.codesense",
    githubUserId: 42,
    orgSlug: "acme/api",
    team: "default",
  };
  const deps: BranchLaunchDeps = {
    resolveBranch: vi.fn(async () => {
      calls.push("resolveBranch");
      return resolution;
    }),
    begin: vi.fn(async () => {
      calls.push("begin");
      return opts.fresh ?? true;
    }),
    abort: vi.fn(async () => {
      calls.push("abort");
    }),
    killSession: vi.fn(async () => {
      calls.push("killSession");
    }),
    resolveSession: vi.fn(async () => {
      calls.push("resolveSession");
      return "sess-b";
    }),
    ensureBranchWorktree: vi.fn(async () => {
      calls.push("ensureBranchWorktree");
      return "/wt/branch/cwd";
    }),
    reconcileStory: vi.fn(async () => {
      calls.push("reconcileStory");
    }),
    connect: vi.fn(async () => {
      calls.push("connect");
      return { sessionId: "sess-b" };
    }),
    bind: vi.fn(async () => {
      calls.push("bind");
    }),
    waitLive: vi.fn(async () => {
      calls.push("waitLive");
      return true;
    }),
    focus: vi.fn(() => {
      calls.push("focus");
    }),
    // SW-GW-5 seams — default: a bound session is LIVE (alive, not parked).
    isSessionParked: vi.fn(() => {
      calls.push("isSessionParked");
      return false;
    }),
    probeAlive: vi.fn(async () => {
      calls.push("probeAlive");
      return true;
    }),
    evictStaleTab: vi.fn(async () => {
      calls.push("evictStaleTab");
    }),
    ...over,
  };
  return { deps, calls };
}

describe("@P1 openBranchWorkspace (SW-DX-18)", () => {
  const target = { repo: "acme/api", branch: "feat/x" };

  it("uses the `<repo>::<branch>` handle shape (SW-DX-11/17a addressing)", () => {
    expect(branchHandle("acme/api", "feat/x")).toBe("acme/api::feat/x");
    expect(
      branchTargetHandle({
        repo: "contributor/app",
        baseRepo: "acme/app",
        pullNumber: 42,
        branch: "feat/x",
      }),
    ).toBe("acme/app::pull/42");
  });

  it("runs the seams in order: resolveBranch → begin → worktree → connect → bind → waitLive → focus", async () => {
    const { deps, calls } = makeBranchDeps();
    const res = await openBranchWorkspace(target, deps);
    expect(res).toEqual({ sessionId: "sess-b" });
    expect(calls).toEqual([
      "resolveBranch",
      "begin",
      "ensureBranchWorktree",
      "connect",
      "bind",
      "waitLive",
      "focus",
    ]);
    // The Rust registry + launcher are keyed by the branch handle.
    expect(deps.begin).toHaveBeenCalledWith("acme/api::feat/x");
    expect(deps.bind).toHaveBeenCalledWith("acme/api::feat/x", "sess-b", scoped);
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({ workspaceKey: "acme/api::feat/x" }),
    );
  });

  it("marks a reserved-but-unbound concurrent launch as non-owning", async () => {
    const onReuse = vi.fn();
    const phases: string[] = [];
    const begin = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const { deps } = makeBranchDeps({ resolveSession: vi.fn(async () => null), begin });
    await openBranchWorkspace(target, deps, {
      headed: false,
      onReuse,
      onPhase: (phase) => phases.push(phase),
    });
    expect(onReuse).toHaveBeenCalledOnce();
    expect(deps.connect).toHaveBeenCalledOnce();
    expect(phases).toEqual(["opening", "skipping-reconcile", "launching"]);
  });

  it("takes cleanup ownership when a waiting joiner acquires an aborted reservation", async () => {
    const begin = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const { deps } = makeBranchDeps({
      begin,
      resolveSession: vi.fn(async () => null),
      connect: vi.fn(async () => {
        throw new Error("spawn after transfer failed");
      }),
    });
    await expect(openBranchWorkspace(target, deps, { headed: false })).rejects.toThrow(
      "spawn after transfer failed",
    );
    expect(deps.abort).toHaveBeenCalledWith("acme/api::feat/x");
  });

  it("cancels a waiting joiner without aborting or connecting against the owner", async () => {
    vi.useFakeTimers();
    try {
      let canceled = false;
      const onReuse = vi.fn();
      const { deps } = makeBranchDeps(
        { begin: vi.fn(async () => false), resolveSession: vi.fn(async () => null) },
        { fresh: false },
      );
      const opening = openBranchWorkspace(target, deps, {
        headed: false,
        onReuse,
        isCanceled: () => canceled,
      });
      const rejection = expect(opening).rejects.toThrow(/^canceled:/);
      await vi.advanceTimersByTimeAsync(0);
      expect(onReuse).toHaveBeenCalledOnce();
      canceled = true;
      await vi.advanceTimersByTimeAsync(100);
      await rejection;
      expect(deps.abort).not.toHaveBeenCalled();
      expect(deps.connect).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds reservation joining with backoff and preserves the owner's slot", async () => {
    vi.useFakeTimers();
    try {
      const { deps } = makeBranchDeps(
        { begin: vi.fn(async () => false), resolveSession: vi.fn(async () => null) },
        { fresh: false },
      );
      const opening = openBranchWorkspace(target, deps, { headed: false });
      const rejection = expect(opening).rejects.toThrow(
        "timeout: existing review launch did not bind",
      );
      await vi.advanceTimersByTimeAsync(RESERVATION_BIND_TIMEOUT_MS);
      await rejection;
      expect(deps.abort).not.toHaveBeenCalled();
      expect(deps.connect).not.toHaveBeenCalled();
      expect(vi.mocked(deps.begin).mock.calls.length).toBeLessThanOrEqual(45);
    } finally {
      vi.useRealTimers();
    }
  });

  it("reserves the cap BEFORE any checkout/spawn work", async () => {
    const { deps, calls } = makeBranchDeps();
    await openBranchWorkspace(target, deps);
    expect(calls.indexOf("begin")).toBeLessThan(calls.indexOf("ensureBranchWorktree"));
  });

  it("reconciles a grounded story after checkout and before launch", async () => {
    const { deps, calls } = makeBranchDeps();
    const phases: string[] = [];
    await openBranchWorkspace({ ...target, linkedStoryId: "SW-OR-8" }, deps, {
      headed: false,
      onPhase: (phase) => phases.push(phase),
    });
    expect(deps.reconcileStory).toHaveBeenCalledWith("SW-OR-8", "/wt/branch/cwd", "acme/api");
    expect(calls.indexOf("ensureBranchWorktree")).toBeLessThan(calls.indexOf("reconcileStory"));
    expect(calls.indexOf("reconcileStory")).toBeLessThan(calls.indexOf("connect"));
    expect(phases).toEqual(["opening", "reconciling", "launching"]);
  });

  it("resolves a fork review from the managed base repository", async () => {
    const { deps } = makeBranchDeps();
    const forkTarget = {
      repo: "contributor/app",
      baseRepo: "acme/app",
      pullNumber: 42,
      branch: "feature/fork",
      linkedStoryId: "SW-OR-8",
    };
    await openBranchWorkspace(forkTarget, deps);
    expect(deps.resolveBranch).toHaveBeenCalledWith("acme/app");
    expect(deps.ensureBranchWorktree).toHaveBeenCalledWith(expect.anything(), forkTarget);
    expect(deps.reconcileStory).toHaveBeenCalledWith("SW-OR-8", "/wt/branch/cwd", "acme/app");
    expect(deps.begin).toHaveBeenCalledWith("acme/app::pull/42");
  });

  it("bounds a stalled checkout without launching an agent", async () => {
    vi.useFakeTimers();
    try {
      const { deps } = makeBranchDeps({
        ensureBranchWorktree: vi.fn(() => new Promise<string>(() => undefined)),
      });
      const opening = openBranchWorkspace(target, deps);
      const rejection = expect(opening).rejects.toThrow("review checkout did not complete");
      await vi.advanceTimersByTimeAsync(REVIEW_PHASE_TIMEOUT_MS);
      await rejection;
      expect(deps.connect).not.toHaveBeenCalled();
      expect(deps.abort).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });

  it("reaps a connect that completes after the bounded launch timeout", async () => {
    vi.useFakeTimers();
    try {
      let finishConnect!: (value: { sessionId: string }) => void;
      const pending = new Promise<{ sessionId: string }>((resolve) => {
        finishConnect = resolve;
      });
      const { deps } = makeBranchDeps({ connect: vi.fn(() => pending) });
      const opening = openBranchWorkspace(target, deps);
      const rejection = expect(opening).rejects.toThrow("agent launch did not complete");
      await vi.advanceTimersByTimeAsync(REVIEW_PHASE_TIMEOUT_MS);
      await rejection;
      finishConnect({ sessionId: "late-session" });
      await vi.waitFor(() => expect(deps.killSession).toHaveBeenCalledWith("late-session"));
      expect(deps.evictStaleTab).toHaveBeenCalledWith("late-session");
    } finally {
      vi.useRealTimers();
    }
  });

  it("surfaces a bounded readiness failure instead of reporting an open review", async () => {
    vi.useFakeTimers();
    try {
      const { deps } = makeBranchDeps({
        waitLive: vi.fn(() => new Promise<boolean>(() => undefined)),
      });
      const opening = openBranchWorkspace(target, deps);
      const rejection = expect(opening).rejects.toThrow("agent readiness did not complete");
      await vi.advanceTimersByTimeAsync(REVIEW_PHASE_TIMEOUT_MS);
      await rejection;
      expect(deps.bind).toHaveBeenCalledWith("acme/api::feat/x", "sess-b", scoped);
    } finally {
      vi.useRealTimers();
    }
  });

  it("preserves reconciliation truth on a deferred readiness failure", async () => {
    const { deps } = makeBranchDeps({ waitLive: vi.fn(async () => false) });
    await expect(
      openBranchWorkspace({ ...target, linkedStoryId: "SW-OR-8" }, deps),
    ).rejects.toThrow("Reconciled · unavailable: review agent readiness failed");
  });

  it("continues launch and reports skipped when a mapping is stale", async () => {
    const { deps } = makeBranchDeps({ reconcileStory: vi.fn(async () => false) });
    await expect(
      openBranchWorkspace({ ...target, linkedStoryId: "SW-OR-8" }, deps, { headed: false }),
    ).resolves.toEqual({ sessionId: "sess-b", reconcileStatus: "skipped" });
    expect(deps.connect).toHaveBeenCalledOnce();
  });

  it("preserves completed reconciliation in a later launch failure", async () => {
    const { deps } = makeBranchDeps({
      connect: vi.fn(async () => {
        throw new Error("launch failed");
      }),
    });
    await expect(
      openBranchWorkspace({ ...target, linkedStoryId: "SW-OR-8" }, deps),
    ).rejects.toThrow("Reconciled · launch failed");
  });

  it("preserves skipped reconciliation in a later launch failure", async () => {
    const { deps } = makeBranchDeps({
      reconcileStory: vi.fn(async () => false),
      connect: vi.fn(async () => {
        throw new Error("launch failed");
      }),
    });
    await expect(
      openBranchWorkspace({ ...target, linkedStoryId: "SW-OR-8" }, deps),
    ).rejects.toThrow("Skipped reconcile · launch failed");
  });

  it("keeps cancellation typed after reconciliation completes", async () => {
    let canceled = false;
    const { deps } = makeBranchDeps({
      reconcileStory: vi.fn(async () => {
        canceled = true;
        return true;
      }),
    });
    await expect(
      openBranchWorkspace({ ...target, linkedStoryId: "SW-OR-8" }, deps, {
        isCanceled: () => canceled,
      }),
    ).rejects.toThrow(/^canceled: Reconciled/);
    expect(deps.connect).not.toHaveBeenCalled();
  });

  it("honors cancellation immediately after repository resolution", async () => {
    let release!: (value: BranchResolution) => void;
    let canceled = false;
    const { deps } = makeBranchDeps({
      resolveBranch: vi.fn(
        () =>
          new Promise<BranchResolution>((resolve) => {
            release = resolve;
          }),
      ),
    });
    const opening = openBranchWorkspace(target, deps, { isCanceled: () => canceled });
    canceled = true;
    release({
      repoPath: "/clones/acme/api",
      teamKey: "org:team",
      codesenseHome: "/home/.codesense",
      githubUserId: 42,
      orgSlug: "acme/api",
      team: "default",
    });
    await expect(opening).rejects.toThrow(/^canceled:/);
    expect(deps.begin).not.toHaveBeenCalled();
    expect(deps.ensureBranchWorktree).not.toHaveBeenCalled();
  });

  it("cancels after checkout without spawning an agent", async () => {
    let releaseCheckout!: (cwd: string) => void;
    const checkout = new Promise<string>((resolve) => {
      releaseCheckout = resolve;
    });
    let canceled = false;
    const { deps } = makeBranchDeps({
      ensureBranchWorktree: vi.fn(() => checkout),
    });

    const opening = openBranchWorkspace(target, deps, { isCanceled: () => canceled });
    await vi.waitFor(() => expect(deps.ensureBranchWorktree).toHaveBeenCalledOnce());
    canceled = true;
    releaseCheckout("/wt/branch/cwd");

    await expect(opening).rejects.toThrow("canceled: review open canceled before launch");
    expect(deps.connect).not.toHaveBeenCalled();
    expect(deps.abort).toHaveBeenCalledOnce();
  });

  it("reaps an unbound agent when cancellation races with connect", async () => {
    let releaseConnect!: (value: { sessionId: string }) => void;
    const connected = new Promise<{ sessionId: string }>((resolve) => {
      releaseConnect = resolve;
    });
    let canceled = false;
    const { deps } = makeBranchDeps({ connect: vi.fn(() => connected) });

    const opening = openBranchWorkspace(target, deps, { isCanceled: () => canceled });
    await vi.waitFor(() => expect(deps.connect).toHaveBeenCalledOnce());
    canceled = true;
    releaseConnect({ sessionId: "canceled-session" });

    await expect(opening).rejects.toThrow("canceled: review open canceled before launch");
    expect(deps.bind).not.toHaveBeenCalled();
    expect(deps.killSession).toHaveBeenCalledWith("canceled-session");
    expect(deps.abort).toHaveBeenCalledOnce();
  });

  it("applies a newly confirmed mapping when reusing an already-live branch session", async () => {
    const { deps } = makeBranchDeps({}, { fresh: false });
    await expect(
      openBranchWorkspace({ ...target, linkedStoryId: "SW-OR-8" }, deps, { headed: false }),
    ).resolves.toEqual({ sessionId: "sess-b", reconcileStatus: "reconciled" });
    expect(deps.reconcileStory).toHaveBeenCalledWith("SW-OR-8", "/wt/branch/cwd", "acme/api");
    expect(deps.connect).not.toHaveBeenCalled();
  });

  it("launches the agent the open names in the branch, else the built-in Claude agent", async () => {
    const { deps } = makeBranchDeps({ connect: vi.fn(async () => ({ sessionId: "sess-b" })) });
    await openBranchWorkspace(target, deps, {
      agent: { agent: "builtin/codex", prompt: "/review" },
    });
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({
        workspaceKey: "acme/api::feat/x",
        agent: { agent: "builtin/codex", prompt: "/review" },
      }),
    );
    const fresh = makeBranchDeps({ connect: vi.fn(async () => ({ sessionId: "sess-c" })) });
    await openBranchWorkspace(target, fresh.deps);
    expect(fresh.deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({ agent: { agent: "builtin/claude" } }),
    );
  });

  it("rejects (and never reserves) when the repo has no local clone (honest degrade)", async () => {
    const { deps } = makeBranchDeps({ resolveBranch: vi.fn(async () => null) });
    await expect(openBranchWorkspace(target, deps)).rejects.toThrow(
      /unavailable: cannot resolve a local clone/,
    );
    expect(deps.begin).not.toHaveBeenCalled();
  });

  it("propagates `busy` from a saturated cap and does not check out (AC-5)", async () => {
    const { deps } = makeBranchDeps({
      begin: vi.fn(async () => {
        throw new Error("busy");
      }),
    });
    await expect(openBranchWorkspace(target, deps)).rejects.toThrow("busy");
    expect(deps.ensureBranchWorktree).not.toHaveBeenCalled();
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("aborts the reserved slot (and never spawns) when the checkout is refused unmanaged (AC-6/R10)", async () => {
    const { deps } = makeBranchDeps({
      ensureBranchWorktree: vi.fn(async () => {
        throw new Error("unavailable: branch 'feat/x' is checked out in a non-managed location");
      }),
    });
    await expect(openBranchWorkspace(target, deps)).rejects.toThrow(/non-managed location/);
    expect(deps.abort).toHaveBeenCalledWith("acme/api::feat/x");
    expect(deps.connect).not.toHaveBeenCalled();
  });

  it("AC idempotent focus: an already-open LIVE branch focuses the existing session and does NOT spawn", async () => {
    const { deps, calls } = makeBranchDeps({}, { fresh: false });
    const res = await openBranchWorkspace(target, deps);
    expect(res).toEqual({ sessionId: "sess-b" });
    expect(calls).not.toContain("ensureBranchWorktree");
    expect(calls).not.toContain("connect");
    expect(deps.focus).toHaveBeenCalledWith("org:team", "sess-b", "acme/api::feat/x");
    expect(deps.evictStaleTab).not.toHaveBeenCalled();
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("AC-6 SW-GW-5: re-open after a parked branch run heals (evict → spawn → rebind → reap)", async () => {
    const { deps, calls } = makeBranchDeps(
      {
        resolveSession: vi.fn(async () => {
          calls.push("resolveSession");
          return "parked-b";
        }),
        isSessionParked: vi.fn(() => {
          calls.push("isSessionParked");
          return true;
        }),
        connect: vi.fn(async () => {
          calls.push("connect");
          return { sessionId: "fresh-b2" };
        }),
      },
      { fresh: false },
    );
    const res = await openBranchWorkspace(target, deps);
    expect(res).toEqual({ sessionId: "fresh-b2" });
    // Full exact order (parity with the story spine's strong oracle, code-review r1):
    // parked short-circuits the probe; evict BEFORE connect; reap AFTER bind.
    expect(deps.probeAlive).not.toHaveBeenCalled();
    expect(calls).toEqual([
      "resolveBranch",
      "begin",
      "resolveSession",
      "isSessionParked",
      "ensureBranchWorktree",
      "evictStaleTab",
      "connect",
      "bind",
      "killSession",
      "waitLive",
      "focus",
    ]);
    expect(deps.evictStaleTab).toHaveBeenCalledWith("parked-b");
    expect(deps.bind).toHaveBeenCalledWith("acme/api::feat/x", "fresh-b2", scoped);
    expect(deps.killSession).toHaveBeenCalledWith("parked-b");
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("does NOT abort a live launch it doesn't own (non-fresh caller failing)", async () => {
    const { deps } = makeBranchDeps(
      {
        resolveSession: vi.fn().mockResolvedValueOnce(null).mockResolvedValue("sess-b"),
        focus: vi.fn(() => {
          throw new Error("spawn boom");
        }),
      },
      { fresh: false },
    );
    await expect(openBranchWorkspace(target, deps)).rejects.toThrow("spawn boom");
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("reaps the orphaned session when bind fails after connect spawned it (SW-DX-17b AC-8)", async () => {
    const { deps } = makeBranchDeps({
      bind: vi.fn(async () => {
        throw new Error("bind boom");
      }),
    });
    await expect(openBranchWorkspace(target, deps)).rejects.toThrow("bind boom");
    // The fresh owner aborts the slot AND reaps the just-spawned, never-bound PTY.
    expect(deps.abort).toHaveBeenCalledWith("acme/api::feat/x");
    expect(deps.killSession).toHaveBeenCalledWith("sess-b");
  });

  it("SW-GW-5: a HEAL whose bind fails reaps the freshly-spawned orphan on the !fresh branch path (code-review r1)", async () => {
    const { deps } = makeBranchDeps(
      {
        resolveSession: vi.fn(async () => "parked-b"),
        isSessionParked: vi.fn(() => true), // → heal path (always !fresh)
        connect: vi.fn(async () => ({ sessionId: "fresh-orphan-b" })),
        bind: vi.fn(async () => {
          throw new Error("bind boom");
        }),
      },
      { fresh: false },
    );
    await expect(openBranchWorkspace(target, deps)).rejects.toThrow("bind boom");
    // The fresh orphan is reaped (PTY + tab) despite the !fresh re-open; no abort.
    expect(deps.killSession).toHaveBeenCalledWith("fresh-orphan-b");
    expect(deps.evictStaleTab).toHaveBeenCalledWith("fresh-orphan-b");
    expect(deps.abort).not.toHaveBeenCalled();
  });

  it("surfaces a readiness probe failure after bind without orphan-style reaping", async () => {
    const { deps } = makeBranchDeps({
      waitLive: vi.fn(async () => {
        throw new Error("probe hiccup");
      }),
    });
    await expect(openBranchWorkspace(target, deps)).rejects.toThrow("probe hiccup");
    // The session is already registered, so controller cleanup closes it by
    // handle; the lower-level orphan reaper must not kill it directly.
    expect(deps.killSession).not.toHaveBeenCalled();
    expect(deps.abort).not.toHaveBeenCalled();
  });

  // ── SW-GW-7: branch inherits the `headed:false` background tier ─────────────
  it("SW-GW-7 headless branch open runs the full launch but SUPPRESSES focus", async () => {
    const { deps, calls } = makeBranchDeps();
    const res = await openBranchWorkspace(target, deps, { headed: false });
    // The branch-review agent is still spawned + bound (first-class + adoptable),
    // only the terminal-stealing focus is skipped — parity with a headless story.
    expect(res).toEqual({ sessionId: "sess-b" });
    expect(calls).toEqual([
      "resolveBranch",
      "begin",
      "ensureBranchWorktree",
      "connect",
      "bind",
      "waitLive",
    ]);
    expect(deps.focus).not.toHaveBeenCalled();
  });

  it("SW-GW-7 default (headed) branch open still focuses — headless is opt-in", async () => {
    const { deps } = makeBranchDeps();
    await openBranchWorkspace(target, deps, {}); // no headed → default true
    expect(deps.focus).toHaveBeenCalledTimes(1);
  });

  it("SW-GW-7 headless idempotent re-open of a LIVE branch does NOT steal focus", async () => {
    const { deps, calls } = makeBranchDeps({}, { fresh: false });
    const res = await openBranchWorkspace(target, deps, { headed: false });
    expect(res).toEqual({ sessionId: "sess-b" });
    // Re-focus is suppressed on the live re-open; no fresh spawn.
    expect(calls).toEqual([
      "resolveBranch",
      "begin",
      "resolveSession",
      "isSessionParked",
      "probeAlive",
    ]);
    expect(deps.focus).not.toHaveBeenCalled();
    expect(deps.connect).not.toHaveBeenCalled();
  });
});

describe("@P1 openDirWorkspace (SW-GW-2)", () => {
  it("opens the generic workspace at the path and returns no session handle", () => {
    const opened: string[] = [];
    const res = openDirWorkspace(
      { path: "/Users/me/proj" },
      { openGenericWorkspace: (p) => opened.push(p) },
    );
    expect(opened).toEqual(["/Users/me/proj"]);
    expect(res).toEqual({});
  });

  it("trims surrounding whitespace before dispatching", () => {
    const opened: string[] = [];
    openDirWorkspace(
      { path: "  /Users/me/proj  " },
      { openGenericWorkspace: (p) => opened.push(p) },
    );
    expect(opened).toEqual(["/Users/me/proj"]);
  });

  it("throws unavailable on an empty/whitespace path WITHOUT opening anything", () => {
    const opened: string[] = [];
    expect(() =>
      openDirWorkspace({ path: "   " }, { openGenericWorkspace: (p) => opened.push(p) }),
    ).toThrow(/unavailable: dir target needs a non-empty \{ path \}/);
    expect(opened).toEqual([]);
  });

  it("is SYNCHRONOUS — the wrapper is invoked before it returns (AC-8 non-blocking)", () => {
    let calledDuring = false;
    const res = openDirWorkspace(
      { path: "/x" },
      {
        openGenericWorkspace: () => {
          calledDuring = true;
        },
      },
    );
    // The store mutation already happened by the time the launcher returns.
    expect(calledDuring).toBe(true);
    expect(res).toEqual({});
  });
});

describe("@P1 runPrompt", () => {
  it("back-compat: a bare storyId drives the MAIN session (AC-2)", async () => {
    const { deps } = makeDeps();
    await runPrompt({ target: "SW-DX-7", prompt: "focus on AC-3" }, deps);
    // resolveStory keyed off the bare id; resolveSession off the same bare handle
    // (no label → main session).
    expect(deps.resolveStory).toHaveBeenCalledWith("SW-DX-7");
    expect(deps.resolveSession).toHaveBeenCalledWith("SW-DX-7");
    expect(deps.injectLine).toHaveBeenCalledWith("sess-1", "focus on AC-3");
  });

  it("drives a NAMED session via the compound handle (AC-1)", async () => {
    // resolveStory keys off the BARE id; resolveSession off the COMPOUND handle.
    const resolveSession = vi.fn(async () => "sess-arch");
    const { deps } = makeDeps({ resolveSession });
    await runPrompt({ target: "SW-DX-7#architecture-review", prompt: "review the diff" }, deps);
    expect(deps.resolveStory).toHaveBeenCalledWith("SW-DX-7");
    expect(resolveSession).toHaveBeenCalledWith("SW-DX-7#architecture-review");
    expect(deps.injectLine).toHaveBeenCalledWith("sess-arch", "review the diff");
  });

  it("fills ONLY the {storyPath} token trusted-side (AC-6)", async () => {
    const { deps } = makeDeps();
    await runPrompt({ target: "SW-DX-7", prompt: "/bmad-code-review {storyPath}" }, deps);
    // The token is filled with the resolved managed path; the whole line injects.
    expect(deps.injectLine).toHaveBeenCalledWith("sess-1", "/bmad-code-review /managed/SW-DX-7.md");
  });

  it("rejects when the story path can't be resolved, writing nothing (AC-6)", async () => {
    const { deps } = makeDeps({ resolveStory: vi.fn(async () => null) });
    await expect(runPrompt({ target: "bogus", prompt: "hi {storyPath}" }, deps)).rejects.toThrow(
      /unavailable: cannot resolve/,
    );
    expect(deps.injectLine).not.toHaveBeenCalled();
  });

  it("rejects with NO live session — never redirects, never spawns (AC-5)", async () => {
    const { deps, calls } = makeDeps({ resolveSession: vi.fn(async () => null) });
    await expect(runPrompt({ target: "SW-DX-7", prompt: "hi" }, deps)).rejects.toThrow(
      /no live session/,
    );
    expect(deps.injectLine).not.toHaveBeenCalled();
    // No spawn seam is ever touched — runPrompt is pure drive.
    expect(calls).not.toContain("connect");
    expect(calls).not.toContain("begin");
  });

  it("sends the line as soon as it is called — one resolve, no preview (AC-3)", async () => {
    const { deps, calls } = makeDeps();
    await runPrompt({ target: "SW-DX-7#arch", prompt: "original" }, deps);
    expect(deps.resolveSession).toHaveBeenCalledTimes(1);
    expect(deps.injectLine).toHaveBeenCalledWith("sess-1", "original");
    expect(calls).toEqual(["resolveStory", "resolveSession", "injectLine"]);
  });
});

describe("@P1 sendToStorySession", () => {
  it("resolves the bound session, submits the turn, and echoes ONLY after landing (AC-1/AC-7)", async () => {
    const { deps, calls } = makeDeps();
    const res = await sendToStorySession({ target: "SW-DX-7", text: "focus on AC-3" }, deps);
    expect(res).toEqual({ delivered: true });
    // resolve → parked-guard → submit → echo, in that order.
    expect(calls).toEqual(["resolveSession", "isSessionParked", "sendLine", "echoUserTurn"]);
    expect(deps.resolveSession).toHaveBeenCalledWith("SW-DX-7");
    expect(deps.sendLine).toHaveBeenCalledWith("sess-1", "focus on AC-3");
    expect(deps.echoUserTurn).toHaveBeenCalledWith("sess-1", "focus on AC-3");
  });

  it("resolves the target TRUSTED-SIDE and never uses a doc-supplied id (AC-3)", async () => {
    // A forged `sessionId` in the args is ignored — only `target` is resolved.
    const { deps } = makeDeps();
    await sendToStorySession(
      { target: "SW-DX-7", text: "hi", ...({ sessionId: "attacker-session" } as object) },
      deps,
    );
    expect(deps.resolveSession).toHaveBeenCalledWith("SW-DX-7");
    expect(deps.sendLine).toHaveBeenCalledWith("sess-1", "hi"); // resolved id, not the forged one
  });

  it("rejects (writes nothing) when the story has no live bound session (AC-3/AC-8)", async () => {
    const { deps } = makeDeps({ resolveSession: vi.fn(async () => null) });
    await expect(sendToStorySession({ target: "SW-DX-7", text: "hi" }, deps)).rejects.toThrow(
      /unavailable: session not live/,
    );
    expect(deps.sendLine).not.toHaveBeenCalled();
    expect(deps.echoUserTurn).not.toHaveBeenCalled();
  });

  it("rejects a dead/parked session (terminal managed state) without writing (AC-8)", async () => {
    const { deps } = makeDeps({ isSessionParked: vi.fn(() => true) });
    await expect(sendToStorySession({ target: "SW-DX-7", text: "hi" }, deps)).rejects.toThrow(
      /unavailable: session not live/,
    );
    expect(deps.sendLine).not.toHaveBeenCalled();
    expect(deps.echoUserTurn).not.toHaveBeenCalled();
  });

  it("surfaces a non-landing turn as delivered:false and does NOT echo (AC-7)", async () => {
    const { deps } = makeDeps({ sendLine: vi.fn(async () => ({ submitted: false })) });
    const res = await sendToStorySession({ target: "SW-DX-7", text: "hi" }, deps);
    expect(res).toEqual({ delivered: false });
    expect(deps.echoUserTurn).not.toHaveBeenCalled(); // no phantom "sent" bubble
  });

  it("propagates a gone-session rejection from the submit primitive (AC-8)", async () => {
    const { deps } = makeDeps({
      sendLine: vi.fn(async () => {
        throw new Error("unavailable: session not live");
      }),
    });
    await expect(sendToStorySession({ target: "SW-DX-7", text: "hi" }, deps)).rejects.toThrow(
      /unavailable: session not live/,
    );
    expect(deps.echoUserTurn).not.toHaveBeenCalled();
  });

  it("rejects empty text or empty target before resolving anything", async () => {
    const { deps } = makeDeps();
    await expect(sendToStorySession({ target: "SW-DX-7", text: "   " }, deps)).rejects.toThrow(
      /\{text\} must be a non-empty string/,
    );
    await expect(sendToStorySession({ target: "", text: "hi" }, deps)).rejects.toThrow(
      /\{target\} must be a non-empty string/,
    );
    expect(deps.resolveSession).not.toHaveBeenCalled();
  });
});

describe("@P1 closeStorySession (SW-GW-10)", () => {
  it("resolve → evict-tab → kill, in order, with no confirm even for a LIVE target (AC-1)", async () => {
    // probeAlive defaults to true (live): a close still runs when called.
    const { deps, calls } = makeDeps();
    const res = await closeStorySession("SW-DX-7", deps);
    expect(res).toEqual({ closed: true });
    expect(calls).toEqual(["resolveSession", "evictStaleTab", "closeByHandle"]);
    expect(deps.evictStaleTab).toHaveBeenCalledWith("sess-1");
    expect(deps.closeByHandle).toHaveBeenCalledWith("SW-DX-7");
  });

  it("UNBOUND handle: no evict, but STILL calls the kill choke (idempotent no-op, AC-3)", async () => {
    // The null-session path must still hit closeByHandle so Rust's `abort` clears
    // a reserved-but-unbound leaked slot — a `if (sessionId)` guard would strand it.
    const { deps, calls } = makeDeps({ resolveSession: vi.fn(async () => null) });
    const res = await closeStorySession("SW-DX-7", deps);
    expect(res).toEqual({ closed: true });
    expect(calls).toEqual(["closeByHandle"]);
    expect(deps.evictStaleTab).not.toHaveBeenCalled();
    expect(deps.closeByHandle).toHaveBeenCalledWith("SW-DX-7");
  });

  it("resolves the target TRUSTED-SIDE — a forged raw session id in args is ignored (AC-2)", async () => {
    const { deps } = makeDeps();
    await closeStorySession("SW-DX-7", deps);
    // Only the handle is resolved; the kill choke is called with the HANDLE, never
    // a raw session id (the doc can't supply one that resolves to another session).
    expect(deps.resolveSession).toHaveBeenCalledWith("SW-DX-7");
    expect(deps.closeByHandle).toHaveBeenCalledWith("SW-DX-7");
  });

  it("is handle-agnostic — a branchHandle closes via the SAME path (AC-4)", async () => {
    const calls: string[] = [];
    const { deps } = makeDeps({
      resolveSession: vi.fn(async () => {
        calls.push("resolveSession");
        return "sess-branch";
      }),
      evictStaleTab: vi.fn(async () => {
        calls.push("evictStaleTab");
      }),
      closeByHandle: vi.fn(async () => {
        calls.push("closeByHandle");
      }),
    });
    const res = await closeStorySession("alderco/codesense#feat/x", deps);
    expect(res).toEqual({ closed: true });
    expect(calls).toEqual(["resolveSession", "evictStaleTab", "closeByHandle"]);
    expect(deps.evictStaleTab).toHaveBeenCalledWith("sess-branch");
    expect(deps.closeByHandle).toHaveBeenCalledWith("alderco/codesense#feat/x");
  });

  it("rejects an empty target before resolving anything", async () => {
    const { deps } = makeDeps();
    await expect(closeStorySession("", deps)).rejects.toThrow(
      /\{target\} must be a non-empty string/,
    );
    expect(deps.resolveSession).not.toHaveBeenCalled();
  });
});

describe("@P2 controller registry", () => {
  beforeEach(() => setDashboardLaunchController(null));

  it("registers and clears the active controller", () => {
    expect(getDashboardLaunchController()).toBeNull();
    const controller = {
      openWorkspace: vi.fn(),
      runPrompt: vi.fn(),
      sendToSession: vi.fn(),
      readWorkspace: vi.fn(),
      getEpicHandle: vi.fn(() => null),
      getDocDir: vi.fn(() => null),
      getWorkspaceStatus: vi.fn(),
      closeWorkspace: vi.fn(),
    };
    setDashboardLaunchController(controller);
    expect(getDashboardLaunchController()).toBe(controller);
    setDashboardLaunchController(null);
    expect(getDashboardLaunchController()).toBeNull();
  });
});

// ── SW-GW-6: parallel named story sessions ───────────────────────────────────

describe("@P1 SW-GW-6 titleCaseLabel (pure, OQ-4)", () => {
  it("humanizes a slug into a title-cased tab name", () => {
    expect(titleCaseLabel("arch-review")).toBe("Arch Review");
    expect(titleCaseLabel("ux")).toBe("Ux");
    expect(titleCaseLabel("perf-review-2")).toBe("Perf Review 2");
  });
  it("is total on odd input (empty / extra hyphens)", () => {
    expect(titleCaseLabel("")).toBe("");
    expect(titleCaseLabel("a--b")).toBe("A B"); // empty segments dropped
  });
});

describe("@P1 SW-GW-6 openStoryWorkspace — named sessions", () => {
  it("AC-3: NO session → registry keys off the BARE storyId, byte-identical", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-DX-7", deps);
    expect(deps.begin).toHaveBeenCalledWith("SW-DX-7");
    expect(deps.bind).toHaveBeenCalledWith("SW-DX-7", "sess-1", scoped);
    // main session: no label, default title = the storyId (never "Terminal N").
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({ session: undefined, title: "SW-DX-7" }),
    );
  });

  it("AC-1/AC-2: WITH session → registry keys off the COMPOUND handle storyId#label", async () => {
    const { deps } = makeDeps();
    const res = await openStoryWorkspace("SW-DX-7", deps, { session: "arch-review" });
    expect(res).toEqual({ sessionId: "sess-1" });
    expect(deps.begin).toHaveBeenCalledWith("SW-DX-7#arch-review");
    expect(deps.bind).toHaveBeenCalledWith("SW-DX-7#arch-review", "sess-1", scoped);
    // resolveStory still keyed off the BARE storyId (the file path is per-story).
    expect(deps.resolveStory).toHaveBeenCalledWith("SW-DX-7");
    // connect carries the label + a default title-cased title.
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({ session: "arch-review", title: "Arch Review" }),
    );
  });

  it("AC-4: explicit title threads through connect, beside the agent the open names", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-DX-7", deps, {
      session: "ux-review",
      title: "UX Review Pass",
      agent: " builtin/codex ",
    });
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({
        session: "ux-review",
        title: "UX Review Pass",
        agent: { agent: "builtin/codex", prompt: undefined, account: undefined },
      }),
    );
  });

  it("an open that names no agent launches the built-in Claude agent", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-DX-7", deps, { initialPrompt: "do the thing" });
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({
        agent: { agent: "builtin/claude", prompt: "do the thing", account: undefined },
      }),
    );
  });

  it("AC-2: two named sessions on one story reserve DISTINCT compound handles", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-DX-7", deps, { session: "arch-review" });
    await openStoryWorkspace("SW-DX-7", deps, { session: "ux-review" });
    expect(deps.begin).toHaveBeenNthCalledWith(1, "SW-DX-7#arch-review");
    expect(deps.begin).toHaveBeenNthCalledWith(2, "SW-DX-7#ux-review");
  });

  it("AC-4: two named sessions with distinct titles each get their own title", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-DX-7", deps, { session: "arch-review", title: "Arch" });
    await openStoryWorkspace("SW-DX-7", deps, { session: "ux-review", title: "UX" });
    expect(deps.connect).toHaveBeenNthCalledWith(
      1,
      expect.objectContaining({ session: "arch-review", title: "Arch" }),
    );
    expect(deps.connect).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ session: "ux-review", title: "UX" }),
    );
  });

  it("AC-9: an over-cap named open rejects `busy` on the COMPOUND handle before connect", async () => {
    const { deps, calls } = makeDeps({
      begin: vi.fn(async () => {
        throw new Error("busy");
      }),
    });
    await expect(openStoryWorkspace("SW-DX-7", deps, { session: "arch-review" })).rejects.toThrow(
      "busy",
    );
    expect(deps.begin).toHaveBeenCalledWith("SW-DX-7#arch-review");
    expect(calls).not.toContain("connect"); // never spawns on saturation
  });

  it("AC-9: a failed named open aborts the COMPOUND handle (frees its own permit)", async () => {
    const { deps } = makeDeps({ ensureWorktree: vi.fn(async () => "") });
    await expect(openStoryWorkspace("SW-DX-7", deps, { session: "arch-review" })).rejects.toThrow(
      /worktree did not open/,
    );
    expect(deps.abort).toHaveBeenCalledWith("SW-DX-7#arch-review");
  });

  it("AC-5: idempotent named re-open resolves the COMPOUND handle's session", async () => {
    const { deps } = makeDeps({}, { fresh: false });
    await openStoryWorkspace("SW-DX-7", deps, { session: "arch-review" });
    expect(deps.resolveSession).toHaveBeenCalledWith("SW-DX-7#arch-review");
  });

  it("a blank/whitespace session is treated as the MAIN session (no re-key)", async () => {
    const { deps } = makeDeps();
    await openStoryWorkspace("SW-DX-7", deps, { session: "   " });
    expect(deps.begin).toHaveBeenCalledWith("SW-DX-7");
    expect(deps.connect).toHaveBeenCalledWith(
      expect.objectContaining({ session: undefined, title: "SW-DX-7" }),
    );
  });
});

describe("openStoryWorkspace — a story that lives on a machine", () => {
  it("opens its agent as a tab there, so no agent named runs the ordinary built-in even for headed:false", async () => {
    const launchRemote = vi.fn(async () => ({ sessionId: "remote-1" }));
    const remote = {
      remoteWorkspace: vi.fn(() => ({
        workspaceId: "SW-9",
        location: { machineId: "m1", handle: "h", cwd: "/r/wt" },
      })),
      launchRemote,
    } as unknown as Partial<DashboardLaunchDeps>;
    const { deps } = makeDeps(remote);
    await openStoryWorkspace("SW-9", deps, { headed: false });
    await openStoryWorkspace("SW-9", deps, { headed: false, agent: "builtin/codex" });
    expect(launchRemote.mock.calls.map((c) => (c as unknown[])[0])).toEqual([
      expect.objectContaining({ agentKey: "builtin/claude", headed: false }),
      expect.objectContaining({ agentKey: "builtin/codex", headed: false }),
    ]);
  });

  it("a machineId places the workspace there first, then starts the agent on that machine", async () => {
    const calls: string[] = [];
    let placed = false;
    const remote = {
      placeWorkspace: vi.fn(async () => {
        calls.push("place");
        placed = true;
      }),
      remoteWorkspace: vi.fn(() =>
        placed
          ? { workspaceId: "SW-9", location: { machineId: "m1", handle: "h", cwd: "/r/wt" } }
          : null,
      ),
      launchRemote: vi.fn(async () => {
        calls.push("launch");
        return { sessionId: "remote-1" };
      }),
    } as unknown as Partial<DashboardLaunchDeps>;
    const { deps } = makeDeps(remote);
    await expect(openStoryWorkspace("SW-9", deps, { machineId: "m1" })).resolves.toEqual({
      sessionId: "remote-1",
    });
    expect(remote.placeWorkspace).toHaveBeenCalledWith(
      expect.any(String),
      expect.any(String),
      "m1",
    );
    expect(calls).toEqual(["place", "launch"]);
    expect(deps.connect).not.toHaveBeenCalled();
  });

  it("a machineId naming somewhere else than the workspace lives is refused before any spawn", async () => {
    const remote = {
      placeWorkspace: vi.fn(async () => {
        throw new Error("unavailable: SW-9's workspace already lives on This Mac");
      }),
      launchRemote: vi.fn(),
    } as unknown as Partial<DashboardLaunchDeps>;
    const { deps } = makeDeps(remote);
    await expect(openStoryWorkspace("SW-9", deps, { machineId: "m1" })).rejects.toThrow(
      /already lives on This Mac/,
    );
    expect(deps.begin).not.toHaveBeenCalled();
    expect(remote.launchRemote).not.toHaveBeenCalled();
  });
});
