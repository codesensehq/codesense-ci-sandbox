// SPDX-License-Identifier: MIT
/**
 * The launch restore of the terminal layout: the admitted runtime members own
 * it, so restoring is listening to their pushes and admissions and asking for
 * every member's sealed snapshot. It never opens a terminal — a workspace
 * whose terminals are gone opens empty.
 */

import {
  onTerminalAdmissionChanged,
  onTerminalLayoutChanged,
  onTerminalLayoutResync,
  terminalLayoutMemberSnapshots,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-layout-source";
import {
  acceptsPrivateAuthority,
  onPrivateAuthorityRetired,
} from "@/shared/lib/private-terminal-authority";
import { memberKey } from "@/shared/lib/terminal-layout-projection";
import type { RuntimeMemberAddress } from "@/features/viewer/lib/cli-workspace/transport/terminal-layout-source";
import type { LayoutInput } from "@/shared/lib/terminal-layout-projection";
import {
  getViewerState,
  subscribeViewerState,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { logger } from "@/shared/lib/logger";
import { getErrorMessage } from "@/shared/lib/error-message";

const CLIENT_BOOT_ID_KEY = "codesense.terminal.clientBootId";

/** How long a failed snapshot waits before it is asked for again. */
const SNAPSHOT_RETRY_MS = 3000;

/** The longest an incomplete snapshot waits before it is asked for again:
 *  the wait doubles from {@link SNAPSHOT_RETRY_MS} while it stays incomplete. */
const INCOMPLETE_RETRY_MAX_MS = 30_000;

/** How many pushes wait for the first snapshot at most. */
const MAX_WAITING = 1000;

/** Resolves once the hosts' layout is known (the first snapshot landed): until
 *  then, a workspace or role without a tab means "not known yet", not "none". */
export function whenTerminalLayoutHydrated(): Promise<void> {
  if (getViewerState().terminalLayout.hydrated) return Promise.resolve();
  return new Promise((resolve) => {
    const unsubscribe = subscribeViewerState((s) => {
      if (!s.terminalLayout.hydrated) return;
      unsubscribe();
      resolve();
    });
  });
}

/** This app launch's id with the terminal hosts: a webview reload keeps it
 *  (so the exited tabs it showed stay), a relaunch mints a new one (so the
 *  hosts drop the exited tabs this viewer was showing). */
export function clientBootId(): string {
  try {
    const kept = sessionStorage.getItem(CLIENT_BOOT_ID_KEY);
    if (kept) return kept;
  } catch {
    // No session storage: a fresh id per call still restores every live tab.
  }
  const minted = crypto.randomUUID();
  try {
    sessionStorage.setItem(CLIENT_BOOT_ID_KEY, minted);
  } catch {
    // As above.
  }
  return minted;
}

/**
 * Subscribe to the members' layout changes and admissions FIRST, then fetch
 * every member's snapshot — the whole restore. What lands before the snapshot
 * waits for it (the snapshot's revisions then drop what it already includes);
 * what the viewer cannot place (an unknown member, a new admission, a rev gap)
 * or a host's resync notice re-fetches the snapshot. A failed snapshot is
 * retried: until one lands the layout reads "not known yet", never "empty".
 * Returns the teardown.
 */
export async function hydrateTerminalLayout(): Promise<() => void> {
  const store = getViewerState;
  let disposed = false;
  let inFlight: Promise<void> | null = null;
  let again = false;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let incompleteWait = SNAPSHOT_RETRY_MS;
  const waiting: LayoutInput[] = [];
  // Once a member is observed private, absent stamps cannot downgrade it to
  // legacy. Keep this small identity tombstone after clearing its metadata.
  const privateMembers = new Set<string>([
    ...Object.entries(store().terminalLayout.members)
      .filter(([, m]) => m.privateAuthorityEpoch != null)
      .map(([key]) => key),
    ...Object.entries(store().terminalLayout.admission)
      .filter(([, row]) => row.privateAuthorityEpoch != null)
      .map(([key]) => key),
  ]);
  const accepts = (member: RuntimeMemberAddress, epoch?: string): boolean => {
    const key = memberKey(member);
    if (epoch != null) privateMembers.add(key);
    return !(epoch == null && privateMembers.has(key)) && acceptsPrivateAuthority(epoch);
  };
  const accepted = (input: LayoutInput): boolean => {
    if (input.kind === "change") return accepts(input.target.member, input.privateAuthorityEpoch);
    if (input.kind === "admission")
      return accepts(input.row.member, input.row.privateAuthorityEpoch);
    return true;
  };

  const apply = (input: LayoutInput): void => {
    if (disposed || !accepted(input)) return;
    if (!store().terminalLayout.hydrated) {
      // While no snapshot lands the backlog is bounded: dropping the oldest
      // only opens a rev gap, which re-fetches.
      if (waiting.length >= MAX_WAITING) waiting.shift();
      waiting.push(input);
      return;
    }
    if (store().applyLayout(input) === "resync") refetch();
  };

  /** Schedule one re-ask after `wait`; `false` when one is pending already. */
  const retryLater = (wait = SNAPSHOT_RETRY_MS): boolean => {
    if (disposed || retry !== null) return false;
    retry = setTimeout(() => {
      retry = null;
      refetch();
    }, wait);
    return true;
  };

  const fetchOnce = async (): Promise<void> => {
    try {
      const replies = await terminalLayoutMemberSnapshots(clientBootId());
      if (disposed) return;
      const outcome = store().applyLayout({
        kind: "snapshot",
        replies: replies.filter((reply) =>
          accepts(
            reply.member,
            reply.mode === "unavailable" ? undefined : reply.privateAuthorityEpoch,
          ),
        ),
      });
      for (const input of waiting.splice(0)) apply(input);
      // An incomplete snapshot (a member withheld or unreadable mid-reconnect)
      // is asked for again, not left until the next event, which may never
      // come: after 3 s, backing off to 30 s while it stays incomplete. One
      // pending timer at most, so it never loops hot.
      if (outcome !== "resync") incompleteWait = SNAPSHOT_RETRY_MS;
      else if (retryLater(incompleteWait)) {
        incompleteWait = Math.min(incompleteWait * 2, INCOMPLETE_RETRY_MAX_MS);
      }
    } catch (e) {
      logger.warn("terminal", `terminal layout snapshot failed: ${getErrorMessage(e)}`);
      if (!disposed) store().noteTerminalHostsUnreachable();
      retryLater();
    }
  };

  const refetch = (): void => {
    if (disposed) return;
    if (inFlight) {
      again = true;
      return;
    }
    inFlight = fetchOnce().finally(() => {
      inFlight = null;
      if (again) {
        again = false;
        refetch();
      }
    });
  };

  const unlisten: Array<() => void> = [
    onPrivateAuthorityRetired(() => {
      if (disposed) return;
      for (let i = waiting.length - 1; i >= 0; i--) if (!accepted(waiting[i])) waiting.splice(i, 1);
      store().applyLayout({ kind: "private-retired" });
    }),
  ];
  try {
    unlisten.push(
      await onTerminalLayoutChanged((target, change, privateAuthorityEpoch) =>
        apply({ kind: "change", target, change, privateAuthorityEpoch }),
      ),
      await onTerminalAdmissionChanged((row) => apply({ kind: "admission", row })),
      await onTerminalLayoutResync(refetch),
    );
  } catch (e) {
    for (const stop of unlisten) stop();
    throw e;
  }
  refetch();
  await inFlight;
  return () => {
    disposed = true;
    if (retry) clearTimeout(retry);
    for (const stop of unlisten) stop();
  };
}
