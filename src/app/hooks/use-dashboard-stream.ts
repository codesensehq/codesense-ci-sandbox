// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { useDashboardStreamStore } from "@/shared/stores/dashboard-stream-store";
import { selectRunningTerminalTabIndex, useViewerStore } from "@/shared/stores/viewer-store";
import { findOpenStoryWorkspace } from "@/shared/lib/workspace-liveness";
import type { SessionStatus } from "@/shared/lib/dashboard-stream/types";
import { asPhase } from "@/shared/lib/roles";
import { createSessionProgressLifetime } from "./session-progress-lifetime";

/** Payload shapes of the agent emits (`mcp/handler.rs`). */
type AgentDonePayload = {
  sessionId: string;
  status: string;
  summary?: string | null;
  activityCompletion?: unknown;
};
type AgentMessagePayload = { sessionId: string; role: string; text: string };
/** SW-GW-4 `signal_progress` emit. Optional fields arrive as `null` when unset. */
type AgentProgressPayload = {
  sessionId: string;
  phase: string;
  step?: number | null;
  total?: number | null;
  note?: string | null;
};

const AGENT_DONE_EVENT = "mcp:agent-done";
const AGENT_MESSAGE_EVENT = "mcp:agent-message";
const AGENT_PROGRESS_EVENT = "mcp:agent-progress";

/** SW-AM-4: an untrusted `note`/`summary` → trimmed, or `undefined` when empty.
 *  Keeps an empty/whitespace signal from writing a phantom (invisible) slice
 *  entry that would only clear at teardown. */
function normalizeNote(s: string | null | undefined): string | undefined {
  const trimmed = (s ?? "").trim();
  return trimmed === "" ? undefined : trimmed;
}

function isSessionStatus(s: string): s is SessionStatus {
  return s === "success" || s === "failed" || s === "needs-attention";
}

/**
 * SW-DX-3 — the ONE app-root listener effect for the live status stream. Mounted
 * once alongside `useCompositeOverlayRefresh` / `useAgentMessageListener` in
 * `App.tsx`. It funnels the agent push seams into the hub:
 *
 * - `mcp:agent-done` / `mcp:agent-message` / `mcp:agent-progress` → session-scoped
 *   deltas (AC-5).
 * - `visibilitychange` hidden ⇒ `markStale` (bias to honesty). A `listen`
 *   registration failure ⇒ `markStale`.
 *
 * (Doc changes are not routed here: in-app doc views subscribe to the context
 * engine directly — SW-OC-26.)
 *
 * Listener lifecycle mirrors `use-composite-overlay-refresh.ts:99–146` exactly —
 * disposed-flag + tracked promise so a fast unmount never leaks a `listen` (AC-9).
 */
export function useDashboardStream(): void {
  useEffect(() => {
    let disposed = false;
    const progressLifetime = createSessionProgressLifetime();

    // SW-GW-3: wire the trusted `storyId → current sessionId` resolver so an
    // `onStatus({kind:"story", storyId})` sub (and the terminal `signal_done`
    // delta) reaches a board that never launched the session. Resolved live from
    // the viewer-store open list at each delta, so it follows session churn and
    // covers a story opened in the normal viewer (the origin bug).
    useDashboardStreamStore.getState().setStorySessionResolver((storyId) => {
      const state = useViewerStore.getState();
      // Prefer the active org on a cross-org same-id collision (matches the verb).
      // Hidden tabs count: a story agent hidden to the tray is still running.
      return (
        // Hidden tabs count: a story agent hidden to the tray is still running.
        findOpenStoryWorkspace(
          state.workspaces,
          selectRunningTerminalTabIndex(state),
          storyId,
          getActiveTeamKey(),
        )?.sessionId ?? null
      );
    });

    const onVisibility = (): void => {
      if (document.visibilityState === "hidden") {
        // A backgrounded webview can miss events — flip to stale rather than
        // silently show a possibly-stale view (AC-7, the never-lie invariant).
        // The next live session delta restores freshness.
        useDashboardStreamStore.getState().markStale();
      }
    };
    window.addEventListener("focus", onVisibility);
    document.addEventListener("visibilitychange", onVisibility);

    const unlisteners: Array<() => void> = [];
    const track = (p: Promise<() => void>): void => {
      p.then((fn) => {
        if (disposed) safeUnlisten(fn);
        else unlisteners.push(fn);
      }).catch(() => {
        // A listener that never registers means we could miss events — degrade
        // to stale rather than lie about being live (AC-7).
        if (!disposed) useDashboardStreamStore.getState().markStale();
      });
    };

    track(
      listen<AgentDonePayload>(AGENT_DONE_EVENT, (event) => {
        const p = event.payload;
        if (!p || !isSessionStatus(p.status)) return;
        useDashboardStreamStore.getState().pushSessionDelta({
          origin: "session",
          kind: "session",
          sessionId: p.sessionId,
          status: p.status,
          summary: p.summary ?? undefined,
        });
        // SW-AM-4 (AC-4): settle the retained progress slice on the terminal
        // signal — show the summary, never a stale in-progress note. Keyed only
        // under a real sessionId (an empty key would bleed onto every unbound
        // row). `needs-attention` maps to the `blocked` (needs-a-human) phase.
        if (p.sessionId) {
          progressLifetime.settle(
            p.sessionId,
            p.status,
            normalizeNote(p.summary),
            p.activityCompletion,
          );
        }
      }),
    );
    track(
      listen<AgentMessagePayload>(AGENT_MESSAGE_EVENT, (event) => {
        const p = event.payload;
        if (!p) return;
        useDashboardStreamStore.getState().pushSessionDelta({
          origin: "session",
          kind: "session-message",
          sessionId: p.sessionId,
          role: p.role,
          text: p.text,
        });
      }),
    );
    track(
      // SW-GW-4: mid-run progress. Drop an off-enum phase (defence-in-depth — the
      // handler already rejects it) rather than push an unrenderable delta. `null`
      // optionals from the emit collapse to `undefined` so the delta omits them.
      listen<AgentProgressPayload>(AGENT_PROGRESS_EVENT, (event) => {
        const p = event.payload;
        if (!p) return;
        const phase = asPhase(p.phase);
        if (!phase) return;
        useDashboardStreamStore.getState().pushSessionDelta({
          origin: "session",
          kind: "progress",
          sessionId: p.sessionId,
          phase,
          step: p.step ?? undefined,
          total: p.total ?? undefined,
          note: p.note ?? undefined,
        });
        // SW-AM-4 (AC-1): retain the latest progress for the row's scalar
        // subscription. Mirrors how `use-terminal-status-events` feeds
        // `setSessionAiStatus` — an O(1) write, no fan-out, off the UI thread.
        // Keyed only under a real sessionId (an empty key would bleed onto every
        // unbound row). A late/reordered tick must NOT resurrect a settled (done)
        // entry — else a following `pty-exit` would delete the AC-4 summary.
        if (p.sessionId) {
          progressLifetime.accept(p.sessionId, {
            phase,
            step: p.step ?? undefined,
            total: p.total ?? undefined,
            note: normalizeNote(p.note),
          });
        }
      }),
    );

    return () => {
      disposed = true;
      progressLifetime.dispose();
      window.removeEventListener("focus", onVisibility);
      document.removeEventListener("visibilitychange", onVisibility);
      for (const fn of unlisteners) safeUnlisten(fn);
      // SW-GW-3: drop the resolver so a story sub can't match after teardown.
      useDashboardStreamStore.getState().setStorySessionResolver(() => null);
    };
  }, []);
}
