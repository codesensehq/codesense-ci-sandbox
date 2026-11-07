// SPDX-License-Identifier: MIT
import { useEffect, useRef } from "react";

import type { InboxLens } from "@/features/viewer/stores/agent-inbox-rail-store";

/**
 * Honour a "write a message" request (`request` above `handled`): Mailspool has no composer, so
 * it switches to Threads first; then `open` may pick a conversation (returning true while it
 * still has something to show) and `focus` puts the caret in the composer.
 *
 * `handled` comes from a store, not this component, so a remount (another view and back, the
 * tab moved to another pane) never re-handles an old request; `onHandled` records the new one
 * there. A request made while the inbox is still loading (`ready` false) is honoured once it
 * shows, unless the human picked another lens after asking: their pick wins.
 */
export function useComposeRequest({
  request,
  handled,
  onHandled,
  ready,
  scope,
  lens,
  setLens,
  open,
  openKey,
  focus,
}: {
  request: number;
  handled: number;
  onHandled?: (request: number) => void;
  ready: boolean;
  /** Whose lens `lens` is (it can change as the inbox loads): only a pick on it counts. */
  scope: string;
  lens: InboxLens;
  setLens: (lens: InboxLens) => void;
  /** Opens what the composer lives in; true when it changed something and must render first. */
  open?: () => boolean;
  /** Whatever `open` changes, so the request is looked at again once it has. */
  openKey?: unknown;
  focus: () => void;
}) {
  // Without a store (a test, a host that keeps none) this mount's own record still applies.
  const local = useRef(handled);
  // The lens the newest request found (or switched to): a later change is the human's pick.
  const seen = useRef<{ request: number; scope: string; lens: InboxLens } | null>(null);
  useEffect(() => {
    if (request <= Math.max(handled, local.current)) return;
    if (seen.current?.request !== request || seen.current.scope !== scope)
      seen.current = { request, scope, lens };
    if (!ready) return;
    const finish = () => {
      local.current = request;
      onHandled?.(request);
    };
    if (lens !== seen.current.lens) return finish();
    if (lens === "spool") {
      seen.current = { request, scope, lens: "threads" };
      setLens("threads");
      return;
    }
    if (open?.()) return;
    finish();
    // Not cancelled on a re-run: recording the request re-runs this at once.
    focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- callbacks are read fresh each pass
  }, [request, handled, ready, scope, lens, openKey]);
}
