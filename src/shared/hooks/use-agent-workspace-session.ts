// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useRef } from "react";
import { toast } from "sonner";

import {
  useAgentSessionOwnership,
  type AgentSessionBinding,
} from "@/shared/components/supervised-agent-session";
import {
  useAgentSessionStore,
  selectBinding,
  useAgentBindingKey,
  DEFAULT_SURFACE_BINDING,
} from "@/shared/stores/use-agent-session-store";
import { useHasActiveTeam } from "@/shared/lib/team-key";
import { sessionExists } from "@/shared/lib/agent-session-injection";
import { verifyAgentLive } from "@/shared/lib/agent-liveness";
import {
  useAgentDotFloor,
  useAgentLivenessMonitor,
} from "@/shared/hooks/use-agent-liveness-monitor";

export { LIVENESS_POLL_MS } from "@/shared/hooks/use-agent-liveness-monitor";
// SW-GS-24: the handshake budgets moved to `agent-liveness` (their new home, shared
// with the draft path); re-exported here so existing import sites stay stable.
export { HANDSHAKE_TIMEOUT_MS, ENTER_NUDGE_MS } from "@/shared/lib/agent-liveness";
import {
  useWorkspaceAgentTabs,
  type AgentTabOption,
} from "@/shared/hooks/use-workspace-agent-tabs";
import {
  runAgentBind,
  resolveMountBind,
  type AgentWorkspaceConfig,
  type BindMode,
} from "@/shared/lib/run-agent-bind";

/**
 * SW-GS-2d (AC-9) — the plug-in hook: a surface "registers" itself for a
 * workspace instance by passing an `AgentWorkspaceConfig`, and gets full agentic
 * control (chat enablement + disconnect + reconnect + an in-surface session
 * picker) back. git-sync is its first consumer; a story/epic surface drops in
 * with its own config + a few JSX lines — no re-wire.
 *
 * It composes the generic pieces (`runAgentBind`, the keyed binding store, the
 * picker primitive, `useAgentSessionOwnership`, prime + ping/pong) and OWNS the
 * agent-workspace lifecycle: auto-connect on mount (AC-5), the reopen
 * re-handshake (D5), the in-flight guard, and the unmount settle.
 * Literal-free (AC-8) — the surfaceKey is the instance `targetWorkspace` (AC-9),
 * so two surfaces of the same kind never collide.
 */

export interface AgentWorkspaceSessionApi {
  /** Feed to `<SupervisedAgentSession binding={…} />`. */
  binding: AgentSessionBinding;
  /** Feed to `<AgentChatSurface sessionId={…} />`. */
  boundSessionId: string | null;
  /** Chat send enablement (agent drives a verified-live session). */
  canSend: boolean;
  /** Shown when `!canSend` (incl. the AC-3a recovery prompt). */
  disabledReason: string | undefined;
  /** AC-3a: the agent didn't ack — show the "open its terminal, then Re-bind" affordance. */
  recovery: boolean;
  /** In-surface session picker (AC-4/AC-6). */
  picker: {
    options: AgentTabOption[];
    /** AC-6 Start new session — spawn (idempotent: de-dupes onto a live agent). */
    startNew: () => void;
    /** AC-4 Connect to an open tab — bind + re-prime + handshake an existing live tab. */
    connectToTab: (sessionId: string) => void;
    /** Re-probe tab liveness. */
    refresh: () => void;
  };
}

export function useAgentWorkspaceSession(config: AgentWorkspaceConfig): AgentWorkspaceSessionApi {
  // SW-AW-6 (AC-1): the binding surfaceKey is TEAM-SCOPED by default —
  // `${targetWorkspace}::${teamKey}` via the reactive team key — so each team
  // gets its own binding and switching teams re-keys the mount effect (AC-4).
  // A USER-GLOBAL agent (`teamScoped: false`, e.g. notes) keeps the bare
  // `targetWorkspace`, so a team switch neither re-keys nor re-spawns. The raw
  // `config.targetWorkspace` still serves as the per-team tab-bucket inner key
  // below (AC-2) — do NOT pass this composite key to the tab hook.
  const teamScoped = config.teamScoped ?? true;
  const surfaceKey = useAgentBindingKey(config.targetWorkspace, teamScoped);

  // Config can be recreated each render by the consumer; keep it in a ref so the
  // mount-once lifecycle effect doesn't churn (consumers SHOULD still memoize it).
  const configRef = useRef(config);
  configRef.current = config;

  // SW-AW-6 (R1/AC-9): ALL per-surface session state — including `recovery` — is
  // keyed in the binding store, so a single app-level hook instance driving N
  // teams can't leak one team's "not responding" banner onto another. One keyed
  // home, no parallel hook-local map.
  const { boundSessionId, owner, connection, recovery } = useAgentSessionStore(
    selectBinding(surfaceKey),
  );
  const setOwner = useAgentSessionStore((s) => s.setOwner);
  const setConnection = useAgentSessionStore((s) => s.setConnection);
  const setBoundSessionId = useAgentSessionStore((s) => s.setBoundSessionId);
  const setRecovery = useAgentSessionStore((s) => s.setRecovery);
  const clearSurface = useAgentSessionStore((s) => s.clearSurface);

  // AC-2: the tabs are the RAW `targetWorkspace`'s under the active team — never
  // the composite binding key's, which would look under an empty bucket.
  const { options, refresh } = useWorkspaceAgentTabs(config.targetWorkspace, config.agentBasename);

  // SW-AW-6: no auto-(re)bind without a REAL active team. A switch INTO a no-team
  // window (no org, or org-set/team-null → `${org}:_none`) must not auto-connect
  // against a non-navigable `_none` bucket. `hasActiveTeam` is a MOUNT-EFFECT DEP
  // (below), not just a ref: a team-scoped agent re-keys `surfaceKey` on every
  // team change so the effect re-runs anyway, but a USER-GLOBAL agent
  // (`teamScoped:false`) has a constant `surfaceKey`, so without this dep it would
  // skip auto-connect forever if it first mounted in a no-team window. Depending
  // on the boolean re-drives global agents on no-team→team (connect) WITHOUT
  // re-driving them on team→team (the boolean is unchanged → stays one session).
  const hasActiveTeam = useHasActiveTeam();

  const mountedRef = useRef(true);
  // SW-AW-6 (AC-9): the in-flight guard is PER-SURFACE, not a single boolean. The
  // app-level agent mounts once and persists across team switches (SW-AW-5), so
  // one hook instance drives every team's binding in turn; a shared boolean would
  // let team A's in-flight bind block team B's. A set keyed by `surfaceKey` keeps
  // each team's bind independent.
  const connectingRef = useRef<Set<string>>(new Set());

  // AC-0 + AC-3b: re-prime with a one-shot token and await the pong. The mechanics
  // (listener-before-prime no-lost-pong race, Enter-nudge, bounded timeout) live in
  // the shared `verifyAgentLive` (SW-GS-24) so the bind path and the headless draft
  // path share ONE liveness definition.
  const primeAndHandshake = useCallback(
    (sessionId: string): Promise<boolean> => verifyAgentLive(sessionId),
    [],
  );

  const runBind = useCallback(
    async (mode: BindMode) => {
      if (connectingRef.current.has(surfaceKey)) return; // per-surface in-flight guard
      connectingRef.current.add(surfaceKey);
      setRecovery(surfaceKey, false); // clear this surface's stale banner while we retry
      try {
        await runAgentBind(configRef.current, mode, {
          setOwner: (o) => setOwner(surfaceKey, o),
          setConnection: (c) => setConnection(surfaceKey, c),
          setBoundSessionId: (id) => setBoundSessionId(surfaceKey, id),
          setRecovery: (r) => mountedRef.current && setRecovery(surfaceKey, r),
          primeAndHandshake,
          isMounted: () => mountedRef.current,
          onError: (e) =>
            toast.error(e instanceof Error ? e.message : `Couldn't connect the agent: ${e}`),
        });
      } finally {
        connectingRef.current.delete(surfaceKey);
        // Re-probe tab liveness after any bind: a connect/relaunch/verify changes
        // agent liveness, and a de-duped Start-new makes no tab-set change to fire
        // the probe effect on its own — so refresh explicitly to avoid a stale ◐.
        if (mountedRef.current) refresh();
      }
    },
    [
      surfaceKey,
      setOwner,
      setConnection,
      setBoundSessionId,
      setRecovery,
      primeAndHandshake,
      refresh,
    ],
  );

  // Mount: reopen re-handshake an existing binding (D5), else auto-connect (AC-5).
  // Unmount: settle a transitional state so a reopen can re-drive.
  useEffect(() => {
    mountedRef.current = true;
    // The whole mount-time decision (preserve user takeover / D5 reopen reconnect /
    // fresh auto-connect / do nothing) is the pure `resolveMountBind` — unit-tested
    // in isolation; the effect just runs the chosen mode. `recovery` needs no
    // cross-team settle: it's per-surface in the store, read for THIS surfaceKey.
    const current = selectBinding(surfaceKey)(useAgentSessionStore.getState());
    const bindOpts = { autoConnect: configRef.current.autoConnect, hasActiveTeam };
    let mode = resolveMountBind(current, bindOpts);
    // SW-GS-22: reopen staleness guard. The binding survives a close, but the
    // session tab it points at may not — a soft-close shadow can be cleared while
    // the workspace is closed. Driving `config.reconnect` into that gone tab
    // relaunches into the void and throws the guarded-injection error at the user
    // (via the reconnect adapter's `/cs-status` re-orient). Detect the stale
    // target BEFORE binding: clear the dead binding and re-derive from the fresh
    // default. This branch only runs when autoConnect + active-team already hold
    // (that's why `resolveMountBind` chose `reconnect`), so the re-derive always
    // yields a fresh `connect` spawn. Scoped to the AUTOMATIC reopen path only —
    // the manual Reconnect (`ownership.onReconnect`) and the live-death recovery
    // are untouched, so the loud throw is preserved for the genuine in-session
    // "owned tab died under us" case (AC-4).
    if (mode?.kind === "reconnect" && !sessionExists(mode.sessionId)) {
      clearSurface(surfaceKey);
      mode = resolveMountBind(DEFAULT_SURFACE_BINDING, bindOpts);
    }
    if (mode) void runBind(mode);
    return () => {
      mountedRef.current = false;
      // The verified-connection state is NOT trusted across an unmount (the agent
      // may die while the surface is closed). Reset it so a reopen never shows a
      // stale "live"/"reconnecting" for a frame before re-verifying; the binding
      // (boundSessionId, owner) persists and the mount effect re-derives liveness.
      const settled = selectBinding(surfaceKey)(useAgentSessionStore.getState());
      if (settled.connection !== "disconnected") setConnection(surfaceKey, "disconnected");
    };
    // Re-runs on a surfaceKey change (team-scoped re-key / reopen) OR a
    // hasActiveTeam change (drives a USER-GLOBAL agent's first connect once a
    // team resolves — its surfaceKey never changes). runBind/setConnection are
    // intentionally excluded (they track surfaceKey, already a dep).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [surfaceKey, hasActiveTeam]);

  // Liveness monitor: while CONNECTED, detect the agent exiting in the CLI (drops
  // to a bare `zsh` — no `pty-exit`/`SessionEnd`) and drop to disconnected +
  // recovery so the chat stops thinking it's connected.
  const onAgentDead = useCallback(() => {
    setConnection(surfaceKey, "disconnected");
    setRecovery(surfaceKey, true);
    toast.error("The agent exited — reconnect to continue.");
  }, [surfaceKey, setConnection, setRecovery]);
  useAgentLivenessMonitor({
    active: connection === "live",
    sessionId: boundSessionId,
    agentBasename: config.agentBasename,
    onDead: onAgentDead,
  });

  // SLW-FX-17: reliability floor for the per-tab status dot. Reconcile the dot
  // from the SAME process probe whenever a session is bound — independent of the
  // chat "live" state — so the dot is correct even where the notify hook is
  // deliberately unwired (org_root git-sync) or a warm-pool post was dropped by
  // the endpoint liveness guard. Precise four-state still comes from the hook
  // where it is wired; this only floors running-from-empty and clears on exit.
  useAgentDotFloor({
    active: boundSessionId !== null,
    sessionId: boundSessionId,
    agentBasename: config.agentBasename,
  });

  const ownership = useAgentSessionOwnership({
    sessionId: boundSessionId,
    owner,
    connection,
    onDisconnect: () => {
      // Take the wheel; agent stays alive (AC-2). Clear recovery — the banner is
      // about chat responsiveness, which is moot once the user drives directly.
      setOwner(surfaceKey, "user");
      setRecovery(surfaceKey, false);
    },
    onReconnect: () => {
      // Reconnect / Re-bind: engine reconnect a bound session (re-attach or
      // relaunch, AC-1); spawn fresh only if nothing is bound yet.
      if (boundSessionId !== null) void runBind({ kind: "reconnect", sessionId: boundSessionId });
      else void runBind({ kind: "connect" });
    },
  });

  const binding: AgentSessionBinding = {
    sessionId: boundSessionId,
    owner,
    connection,
    onDisconnect: ownership.disconnect,
    onReconnect: ownership.reconnect,
  };

  const canSend = ownership.isDriving && boundSessionId !== null;
  const disabledReason = useMemo(() => {
    if (owner === "user") return "You have control — type directly in the terminal.";
    if (connection === "reconnecting") return "Connecting…";
    if (recovery)
      return "Agent isn't responding — open its terminal in the tray, get it to a prompt, then Re-bind.";
    if (connection !== "live") return "No live session — reconnect to continue.";
    return undefined;
  }, [owner, connection, recovery]);

  return {
    binding,
    boundSessionId,
    canSend,
    disabledReason,
    recovery,
    picker: {
      options,
      startNew: () => void runBind({ kind: "connect" }),
      connectToTab: (sessionId) => void runBind({ kind: "attach", sessionId }),
      refresh,
    },
  };
}
