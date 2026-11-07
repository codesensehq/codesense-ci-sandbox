// SPDX-License-Identifier: MIT
import type { SessionOwner, ConnectionPhase } from "@/shared/components/supervised-agent-session";
import type { ReconnectResult } from "@/shared/lib/agent-session-launcher";
import type { SurfaceBinding } from "@/shared/stores/use-agent-session-store";

/**
 * SW-GS-2d (AC-9) — the React-FREE bind/handshake orchestration that
 * `useAgentWorkspaceSession` drives. Extracted so the load-bearing connect /
 * reconnect / attach control flow is unit-tested without mounting React (xterm
 * can't mount headlessly anyway). The hook supplies store-writer + handshake
 * deps; this owns the order and the live/not-live decision.
 */

/**
 * A surface's agent config — the per-KIND template (`agentBasename`,
 * `reorientCommand`, the connect/reconnect adapters) plus the per-INSTANCE
 * `targetWorkspace` (which is also the binding `surfaceKey`). NO Claude
 * literal lives here — the consumer fills it in.
 *
 * `connect`/`reconnect` do ONLY the PTY-level work and return the bound session
 * id; the hook owns the chat re-prime (AC-0) + ping/pong (AC-3b) uniformly, so
 * every surface gets the same handshake without re-implementing it. The re-orient
 * (`/cs-status`) is the adapter's concern — baked into `reconnect`, not a generic
 * field — so the hook stays agent-semantics-free.
 */
export interface AgentWorkspaceConfig {
  /** Workspace-instance id; also the binding `surfaceKey`. */
  targetWorkspace: string;
  /** Agent exe basename — liveness probe + picker filter. */
  agentBasename: string;
  /** Spawn (de-duped) + launch + wait-live → the bound session id. No prime. */
  connect: () => Promise<string>;
  /** State-aware engine reconnect of the bound session (re-attach or relaunch). No prime. */
  reconnect: (sessionId: string) => Promise<ReconnectResult>;
  /** Auto-bind on mount when there's an obvious target (AC-5). */
  autoConnect?: boolean;
  /** SW-AW-6 — is this agent's session team-scoped? Default (undefined/true):
   *  the binding key is `${targetWorkspace}::${teamKey}`, so each team gets its
   *  OWN warm session (git-sync, stories/epics — surfaces that operate on
   *  per-team directories). Set `false` for a USER-GLOBAL agent (notes: one
   *  session at a machine-global cwd, shared across teams) — its binding key is
   *  the bare `targetWorkspace`, so switching teams does not re-key or re-spawn. */
  teamScoped?: boolean;
}

/** How a (re)bind acquires its session id. */
export type BindMode =
  | { kind: "connect" } // new / auto: spawn (idempotent — launcher de-dupes the owned tab)
  | { kind: "reconnect"; sessionId: string } // Reconnect button: engine reconnect the bound sid
  | { kind: "attach"; sessionId: string }; // click-select a live tab / reopen re-handshake

/**
 * Pure mount-time bind decision — the automatic (re)bind branch of
 * `useAgentWorkspaceSession`'s mount effect, extracted so the load-bearing rules
 * are unit-testable without React. Returns the `BindMode` to run on mount, or
 * `null` to do nothing.
 *
 * Rules (in order):
 * 1. `owner === "user"` → null. The user took the wheel; never auto-rebind (that
 *    would yank ownership back to the agent). They re-engage via Reconnect.
 * 2. `!autoConnect || !hasActiveTeam` → null. All automatic binding is gated by
 *    the consumer's opt-in AND by a real active team (SW-AW-6): a re-key into a
 *    no-team window (`${org}:_none` / no org) must not spawn a phantom agent.
 * 3. `boundSessionId !== null` → `reconnect` (D5 reopen re-handshake: re-attach if
 *    the agent still lives, RELAUNCH if it died while the surface was closed).
 * 4. else `connection === "disconnected"` → `connect` (fresh auto-connect, AC-5).
 * 5. otherwise null (a live/reconnecting binding with no session needs nothing).
 */
export function resolveMountBind(
  binding: Pick<SurfaceBinding, "owner" | "boundSessionId" | "connection">,
  opts: { autoConnect: boolean | undefined; hasActiveTeam: boolean },
): BindMode | null {
  if (binding.owner === "user") return null;
  if (!opts.autoConnect || !opts.hasActiveTeam) return null;
  if (binding.boundSessionId !== null) {
    return { kind: "reconnect", sessionId: binding.boundSessionId };
  }
  if (binding.connection === "disconnected") return { kind: "connect" };
  return null;
}

export interface BindDeps {
  setOwner: (owner: SessionOwner) => void;
  setConnection: (connection: ConnectionPhase) => void;
  setBoundSessionId: (id: string) => void;
  /** AC-3a: surface (or clear) the "agent isn't responding — Re-bind" affordance. */
  setRecovery: (recovery: boolean) => void;
  /** AC-0 + AC-3b: re-prime with a one-shot token and await the pong; true = verified. */
  primeAndHandshake: (sessionId: string) => Promise<boolean>;
  /** Don't commit store writes after the surface unmounted. */
  isMounted: () => boolean;
  onError: (e: unknown) => void;
}

/**
 * Run one (re)bind: flip to the transitional state, acquire the session id per
 * `mode`, then re-prime + ping/pong. "Connected/live" is committed ONLY on a
 * pong (AC-3b); a timeout leaves the session bound but not-live and raises the
 * AC-3a recovery affordance — never a false "live" on a silent agent.
 */
export async function runAgentBind(
  config: AgentWorkspaceConfig,
  mode: BindMode,
  deps: BindDeps,
): Promise<void> {
  deps.setOwner("agent");
  deps.setConnection("reconnecting");
  try {
    let sessionId: string;
    if (mode.kind === "reconnect") {
      await config.reconnect(mode.sessionId);
      sessionId = mode.sessionId;
    } else if (mode.kind === "attach") {
      sessionId = mode.sessionId;
    } else {
      sessionId = await config.connect();
    }

    const verified = await deps.primeAndHandshake(sessionId);
    if (!deps.isMounted()) return;
    deps.setBoundSessionId(sessionId);
    deps.setConnection(verified ? "live" : "disconnected");
    deps.setRecovery(!verified);
  } catch (e) {
    // Both writes are mount-guarded: a surface that unmounted mid-bind should
    // neither flip store state nor raise an orphaned error toast.
    if (deps.isMounted()) {
      deps.setConnection("disconnected");
      deps.onError(e);
    }
  }
}
