// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import {
  agentPreview,
  type AgentForm,
  type AgentTeamRef,
  type ResolvedAgentView,
} from "@/tauri/commands";

/** How long the form settles before the preview re-resolves. */
export const PREVIEW_DEBOUNCE_MS = 300;

/** The editor's live preview: what the unsaved form would resolve to. `servers` is what the
 *  team the last resolution resolved against offers (the bridge aside), `null` until one lands. */
export type AgentPreviewState = { servers: string[] | null } & (
  | { status: "loading"; last: ResolvedAgentView | null }
  | { status: "resolved"; resolved: ResolvedAgentView }
  | { status: "invalid"; field: string | null; message: string; last: ResolvedAgentView | null }
  | { status: "error"; message: string; last: ResolvedAgentView | null }
);

/** The last resolution a state still shows while a newer one is on its way or refused. */
export function lastResolved(state: AgentPreviewState): ResolvedAgentView | null {
  return state.status === "resolved" ? state.resolved : state.last;
}

/** A reply, with the request it answers. */
type Answered =
  | { request: string; outcome: "resolved"; resolved: ResolvedAgentView; servers: string[] }
  | { request: string; outcome: "invalid"; field: string | null; message: string }
  | { request: string; outcome: "error"; message: string };

/**
 * Resolve `form` (against `team`, when given) whenever it settles, writing nothing. A reply
 * that a newer request has overtaken is dropped, and the last resolution keeps showing until
 * the next one lands, so the panel does not flicker while typing. It also resolves again
 * when the backend re-resolves dependencies (a CLI upgrade and a Re-check), since a warning
 * about the installed CLI's version comes from that cache. While `enabled` is false it asks
 * nothing and stays loading.
 */
export function useAgentPreview(
  form: AgentForm,
  team: AgentTeamRef | undefined,
  enabled = true,
  cwd?: string,
): AgentPreviewState {
  const request = JSON.stringify([form, team ?? null, cwd ?? null]);
  const [answer, setAnswer] = useState<Answered | null>(null);
  const [last, setLast] = useState<{ resolved: ResolvedAgentView; servers: string[] } | null>(null);
  const seq = useRef(0);
  const [depsRevision, setDepsRevision] = useState(0);

  useEffect(() => {
    const group = createListenerGroup();
    group.track(listen("dep-status-changed", () => setDepsRevision((n) => n + 1)));
    return () => group.dispose();
  }, []);

  useEffect(() => {
    const mine = ++seq.current;
    if (!enabled) return;
    const timer = setTimeout(() => {
      const [sent, target, directory] = JSON.parse(request) as [
        AgentForm,
        AgentTeamRef | null,
        string | null,
      ];
      agentPreview(sent, target ?? undefined, directory ?? undefined).then(
        (reply) => {
          if (seq.current !== mine) return;
          setAnswer({ ...reply, request });
          if (reply.outcome === "resolved") {
            setLast({ resolved: reply.resolved, servers: reply.servers });
          }
        },
        (e: unknown) => {
          if (seq.current !== mine) return;
          setAnswer({
            request,
            outcome: "error",
            message: e instanceof Error ? e.message : String(e),
          });
        },
      );
    }, PREVIEW_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [request, depsRevision, enabled]);

  const shown = last?.resolved ?? null;
  const servers = last?.servers ?? null;
  if (!answer || answer.request !== request) return { status: "loading", last: shown, servers };
  switch (answer.outcome) {
    case "resolved":
      return { status: "resolved", resolved: answer.resolved, servers: answer.servers };
    case "invalid":
      return {
        status: "invalid",
        field: answer.field,
        message: answer.message,
        last: shown,
        servers,
      };
    default:
      return { status: "error", message: answer.message, last: shown, servers };
  }
}
