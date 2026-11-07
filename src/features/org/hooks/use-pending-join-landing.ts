// SPDX-License-Identifier: MIT
import { useEffect } from "react";

import { openWorkspaces } from "@/features/org/lib/open-workspaces";
import { clearPendingJoinUrl } from "@/shared/lib/pending-join";
import { useFirstRunStore } from "@/shared/stores/first-run-store";

/**
 * SW-FU-B6: reopen a join the user started before a sign-in or a window reload.
 *
 * A guest who connects a code host mid-join is reloaded into the signed-in
 * identity with `joinPending` set, and once the app is ready (discovery settled, a user, a workspace to show)
 * the join form opens in the Workspaces tab, with the typed URL kept. It opens
 * once: landing clears `joinPending`. (With no workspace at all, App shows the
 * join form itself instead of auto-creating one.)
 *
 * A join URL left in storage by a form that was open when the app quit is not a
 * pending join: it is dropped at launch so it never prefills a later join.
 */
export function usePendingJoinLanding(ready: boolean): void {
  const joinPending = useFirstRunStore((s) => s.joinPending);

  useEffect(() => {
    if (!useFirstRunStore.getState().joinPending) clearPendingJoinUrl();
  }, []);

  useEffect(() => {
    if (!ready || !joinPending) return;
    useFirstRunStore.getState().landJoinPending();
    // A landing at launch, not the user's open: full-screen CLI stays on.
    openWorkspaces("join", { keepLayout: true });
  }, [ready, joinPending]);
}
