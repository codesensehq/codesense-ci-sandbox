// SPDX-License-Identifier: MIT
/**
 * Opening a terminal tab on a remote Machine. The Machine's host places the tab
 * in the workspace's layout, beside This Mac's tabs; this module only sends the
 * request and keeps every request whose answer is not in yet — or was lost — on
 * a list the workspace shows ("Opening on X…"). A request is created exactly
 * once: a lost reply is only ever inspected again by its request id, never sent
 * as a new create.
 */

import { useEffect, useSyncExternalStore } from "react";
import { getCurrentUser, useCurrentUser } from "@/shared/lib/get-current-user";
import {
  remoteShellTab,
  type RemoteShellLaunch,
  type RemoteShellTabRequest,
  type RemoteTabOutcome,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source";
import { terminalLayoutMemberSnapshots } from "@/features/viewer/lib/cli-workspace/transport/terminal-layout-source";
import { getViewerState } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { machineSessionTarget } from "@/features/machines/stores/machine-projection-selectors";
import {
  currentPrivateAuthorityEpoch,
  onPrivateAuthorityRetired,
} from "@/shared/lib/private-terminal-authority";
import { clientBootId } from "@/shared/lib/terminal-layout";
import { getErrorMessage } from "@/shared/lib/error-message";
import { onMachineTerminalServerReplaced } from "@/features/shell/lib/machine-terminal-server";
import { workspaceKey } from "@/shared/lib/workspace-key";
import { NATIVE_LAUNCHES } from "@/shared/lib/agent-runtimes";
import { showNewTab, whereTheUserIs } from "./terminal-tab-keys";

/** One remote open this viewer sent and has no tab for yet. */
export interface PendingRemoteOpen {
  frameId?: string;
  pagePath?: string;
  requestId: string;
  teamKey: string;
  workspaceId: string;
  machineId: string;
  /** The Machine's label, for "Opening on X…". */
  label: string;
  launch: RemoteShellLaunch;
  agentKey?: string;
  preparedWorkspaceHandle?: string;
  cwd?: string;
  /** The tab's name, for a named agent session. */
  name?: string;
  /** A saved Agent's first prompt, in place of its own. */
  initialPrompt?: string;
  /** `false`: open in the background, never taking the focus. */
  headed?: boolean;
  retryable?: boolean;
  /** `opening`: the request is in flight. `unknown`: its answer was lost or
   *  not conclusive — check again (by request id) before anything else.
   *  `refused`: the host said no; nothing was started. */
  status: "opening" | "unknown" | "refused";
  message?: string;
  code?: string;
  /** The send that marked this row stalled: only its late reply may land. */
  stall?: string;
}

let pending: readonly PendingRemoteOpen[] = [];
const listeners = new Set<() => void>();
let pendingOwner: string | null = null;
const storageKey = (owner: string) => `codesense.pending-remote-opens.${owner}`;

/** Receipts are recovery hints only; the backend reauthorizes every attach. */
export function restoreRemoteOpens(owner: string | null): void {
  if (pendingOwner === owner) return;
  pendingOwner = owner;
  pending = [];
  if (owner !== null) {
    try {
      const raw = localStorage.getItem(storageKey(owner));
      const rows: unknown = raw && raw.length <= 1024 * 1024 ? JSON.parse(raw) : [];
      if (Array.isArray(rows))
        pending = rows.slice(0, 128).flatMap((row: unknown) => {
          if (!row || typeof row !== "object") return [];
          const value = row as Record<string, unknown>;
          if (
            !["requestId", "teamKey", "workspaceId", "machineId", "label"].every(
              (key) => typeof value[key] === "string",
            )
          )
            return [];
          if (!NATIVE_LAUNCHES.includes(String(value.launch) as (typeof NATIVE_LAUNCHES)[number]))
            return [];
          return [
            {
              frameId: typeof value.frameId === "string" ? value.frameId : undefined,
              pagePath: typeof value.pagePath === "string" ? value.pagePath : undefined,
              requestId: value.requestId as string,
              teamKey: value.teamKey as string,
              workspaceId: value.workspaceId as string,
              machineId: value.machineId as string,
              label: value.label as string,
              launch: value.launch as RemoteShellLaunch,
              agentKey: typeof value.agentKey === "string" ? value.agentKey : undefined,
              preparedWorkspaceHandle:
                typeof value.preparedWorkspaceHandle === "string"
                  ? value.preparedWorkspaceHandle
                  : undefined,
              cwd: typeof value.cwd === "string" ? value.cwd : undefined,
              name: typeof value.name === "string" ? value.name : undefined,
              headed: value.headed === false ? false : undefined,
              status: "unknown" as const,
              message: "Check the previous request before launching again.",
            },
          ];
        });
    } catch {
      /* An unreadable local hint grants no launch authority. */
    }
  }
  for (const listener of listeners) listener();
}

function publish(next: readonly PendingRemoteOpen[]): void {
  pending = next;
  if (pendingOwner !== null) {
    try {
      localStorage.setItem(storageKey(pendingOwner), JSON.stringify(next));
    } catch {
      /* Keep recovery available for this viewer if storage is full. */
    }
  }
  for (const listener of listeners) listener();
}

function update(requestId: string, patch: Partial<PendingRemoteOpen> | null): void {
  publish(
    patch === null
      ? pending.filter((p) => p.requestId !== requestId)
      : pending.map((p) => (p.requestId === requestId ? { ...p, ...patch } : p)),
  );
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Every remote open still waiting on an answer, in the order sent. */
export function pendingRemoteOpens(): readonly PendingRemoteOpen[] {
  return pending;
}

/** The remote opens of one workspace that have no tab yet. */
export function usePendingRemoteOpens(
  teamKey: string,
  workspaceId: string,
): readonly PendingRemoteOpen[] {
  const owner = useCurrentUser()?.principal ?? null;
  useEffect(() => restoreRemoteOpens(owner), [owner]);
  const all = useSyncExternalStore(subscribe, pendingRemoteOpens, pendingRemoteOpens);
  return pendingOwner === owner
    ? all.filter((p) => p.teamKey === teamKey && p.workspaceId === workspaceId)
    : [];
}

/** Forget a refused or abandoned request. A terminal it did start still shows
 *  as a tab once its Machine announces it. */
export function dismissRemoteOpen(requestId: string): void {
  update(requestId, null);
}

/** Drop every pending open: the private owner changed, and none is theirs. */
export function clearRemoteOpens(): void {
  if (pending.length > 0) publish([]);
}
onPrivateAuthorityRetired(() => {
  pendingOwner = null;
  pending = [];
  for (const listener of listeners) listener();
});

/** The workspace key a workspace's terminals are held under. */
function keyOf(teamKey: string, workspaceId: string): string | null {
  const ws = getViewerState().workspaces[teamKey]?.find((w) => w.id === workspaceId);
  return ws ? workspaceKey(teamKey, ws) : null;
}

/** Read every member's layout again, so the new tab shows at once (and the
 *  app's copy of the Machine's layout is current if it drops next). */
export async function refreshLayout(): Promise<void> {
  const replies = await terminalLayoutMemberSnapshots(clientBootId());
  getViewerState().applyLayout({ kind: "snapshot", replies });
}

/** How a remote open ended: its tab, or why there is none (yet). */
export type RemoteOpenSettled =
  | { status: "opened"; session: string }
  | { status: "refused" | "unknown"; message: string; code?: string };

/** How long "Opening on X…" spins before it turns into a recovery row: the
 *  machine's answer, or the layout read after it, may never come. */
export const REMOTE_OPEN_SETTLE_MS = 8000;

/** Codes of a request that stalled — no answer within the settle time. */
export const STALLED_OPEN_CODES: readonly string[] = ["noAnswer", "timedOut"];

/** `promise`'s value, or `timedOut` once `ms` pass first. Rejections pass through. */
function within<T>(promise: Promise<T>, ms: number): Promise<{ value: T } | "timedOut"> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => resolve("timedOut"), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve({ value });
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

async function send(
  open: PendingRemoteOpen,
  request: RemoteShellTabRequest,
  stillHere: () => boolean,
): Promise<RemoteOpenSettled> {
  const authority = currentPrivateAuthorityEpoch();
  const current = () => authority !== null && currentPrivateAuthorityEpoch() === authority;
  const retired: RemoteOpenSettled = {
    status: "unknown",
    message: "The private owner changed before the machine answered.",
  };
  const settle = (patch: Partial<PendingRemoteOpen>): RemoteOpenSettled => {
    update(open.requestId, patch);
    return {
      status: patch.status === "refused" ? "refused" : "unknown",
      message: patch.message ?? "",
      ...(patch.code ? { code: patch.code } : {}),
    };
  };
  const token = crypto.randomUUID();
  const stalled = (message: string) =>
    settle({ status: "unknown", code: "noAnswer", message, stall: token });
  /** Still the row this send marked stalled: nobody re-checked or dismissed
   *  it, and no later send (an Attach) stalled on it since. */
  const stillStalled = () =>
    pending.some(
      (p) =>
        p.requestId === open.requestId &&
        p.status === "unknown" &&
        p.code === "noAnswer" &&
        p.stall === token,
    );

  const answered = async (result: RemoteTabOutcome): Promise<RemoteOpenSettled> => {
    if (!current()) return retired;
    if (
      result.status === "opened" ||
      result.status === "alreadyLive" ||
      result.status === "ended"
    ) {
      try {
        if ((await within(refreshLayout(), REMOTE_OPEN_SETTLE_MS)) === "timedOut") {
          if (!current()) return retired;
          return stalled(
            `${open.label} answered, but its tab list hasn't arrived. Attach to bring the tab in.`,
          );
        }
      } catch {
        /* The host's own announcement brings the tab. */
      }
      if (!current()) return retired;
      const key = keyOf(open.teamKey, open.workspaceId);
      const target = result.target;
      if (
        !target ||
        !key ||
        result.workspaceKey !== key ||
        target.member.machineId !== open.machineId ||
        !machineSessionTarget(open.teamKey, target, key, target.session)
      ) {
        return settle({
          status: "unknown",
          message: "The reply did not name the tab it opened. Check again before opening another.",
        });
      }
      update(open.requestId, null);
      if (open.headed !== false) {
        getViewerState().setActiveTerminalTab(open.teamKey, target.session, open.workspaceId);
        await showNewTab(open.teamKey, open.workspaceId, target.session, stillHere);
      }
      return { status: "opened", session: target.session };
    }
    if (result.status === "refused" && result.code !== "attachedElsewhere") {
      return settle({
        status: "refused",
        message: result.message ?? "The machine refused to open a terminal.",
        code: result.code ?? undefined,
        retryable: result.retryable === true,
      });
    }
    return settle({
      status: "unknown",
      message: result.message ?? "Waiting for the machine to confirm the terminal.",
      code: result.code ?? undefined,
    });
  };

  const reply =
    open.frameId || open.pagePath
      ? remoteShellTab(request, { frameId: open.frameId, pagePath: open.pagePath })
      : remoteShellTab(request);
  let first: { value: RemoteTabOutcome } | "timedOut";
  try {
    first = await within(reply, REMOTE_OPEN_SETTLE_MS);
  } catch (error) {
    if (!current()) return retired;
    return settle({
      status: "unknown",
      message: `The reply was lost: ${getErrorMessage(error)}`,
    });
  }
  if (first !== "timedOut") return answered(first.value);
  if (!current()) return retired;
  const outcome = stalled(`${open.label} hasn't answered yet.`);
  // The row offers recovery now; a caller awaiting the open still gets the
  // late answer (the backend bounds it), unless the person acted on the row.
  try {
    const late = await reply;
    return stillStalled() ? answered(late) : outcome;
  } catch {
    return outcome;
  }
}

/** What a new remote tab is asked to be. */
export interface RemoteTabInput {
  frameId?: string;
  pagePath?: string;
  teamKey: string;
  workspaceId: string;
  machineId: string;
  label: string;
  launch?: RemoteShellLaunch;
  agentKey?: string;
  preparedWorkspaceHandle?: string;
  cwd?: string;
  name?: string;
  /** A saved Agent's first prompt, in place of its own: it starts with it. */
  initialPrompt?: string;
  /** `false`: open in the background, never taking the focus. */
  headed?: boolean;
}

/** Bring an already-open tab of a workspace forward. */
export function focusRemoteTab(teamKey: string, workspaceId: string, session: string): void {
  getViewerState().revealTerminalInPlace(teamKey);
  getViewerState().setActiveTerminalTab(teamKey, session, workspaceId);
}

/** Open a new tab on `machineId` in the workspace: a shell (or Claude, Codex)
 *  in the remote account's home. The tab shows once its host places it and
 *  takes the focus; until then the workspace lists it as opening. */
export function openRemoteTab(input: RemoteTabInput): string {
  return startRemoteTab(input).requestId;
}

/** {@link openRemoteTab}, resolving once the machine has answered. */
export function openRemoteTabSettled(input: RemoteTabInput): Promise<RemoteOpenSettled> {
  return startRemoteTab(input).settled;
}

function startRemoteTab(input: RemoteTabInput): {
  requestId: string;
  settled: Promise<RemoteOpenSettled>;
} {
  restoreRemoteOpens(getCurrentUser()?.principal ?? null);
  const open: PendingRemoteOpen = {
    frameId: input.frameId,
    pagePath: input.pagePath,
    requestId: crypto.randomUUID(),
    teamKey: input.teamKey,
    workspaceId: input.workspaceId,
    machineId: input.machineId,
    label: input.label,
    launch: input.launch ?? "shell",
    agentKey: input.agentKey,
    preparedWorkspaceHandle: input.preparedWorkspaceHandle,
    cwd: input.cwd,
    name: input.name,
    ...(input.agentKey && input.initialPrompt?.trim()
      ? { initialPrompt: input.initialPrompt }
      : {}),
    headed: input.headed === false ? false : undefined,
    status: "opening",
  };
  publish([...pending, open]);
  const stillHere = whereTheUserIs(input.teamKey);
  const settled = send(
    open,
    {
      operation: "create",
      requestId: open.requestId,
      machineId: open.machineId,
      ...(open.agentKey
        ? { agentKey: open.agentKey, preparedWorkspaceHandle: open.preparedWorkspaceHandle }
        : { launch: open.launch }),
      ...(open.initialPrompt ? { initialPrompt: open.initialPrompt } : {}),
      cwd: open.cwd,
      ...(open.name ? { name: open.name } : {}),
      teamKey: open.teamKey,
      workspaceId: open.workspaceId,
      headed: open.headed !== false,
    },
    stillHere,
  );
  return { requestId: open.requestId, settled };
}

/** Refusals that started nothing and that the person can clear on the
 *  machine (install the CLI, allow agents), so the launch can be asked again. */
export const RELAUNCHABLE_REFUSALS: readonly string[] = [
  "agentCliMissing",
  "agentsNotAllowed",
  "agentSetupFailed",
];

/** Ask again, as a new request, for a launch the machine refused before it
 *  started anything. */
export function relaunchRefusedRemoteOpen(requestId: string): void {
  const open = pending.find((item) => item.requestId === requestId);
  if (!open || open.status !== "refused" || !RELAUNCHABLE_REFUSALS.includes(open.code ?? ""))
    return;
  dismissRemoteOpen(requestId);
  openRemoteTab(open);
}

/** An open the old terminal server refused as too old started nothing, so
 *  once the server is replaced it is sent again, as a new request. */
function reopenRefusedAsOutdated(machineId: string): void {
  for (const open of pending) {
    if (
      open.machineId !== machineId ||
      open.status !== "refused" ||
      open.code !== "terminalServerOutdated"
    )
      continue;
    dismissRemoteOpen(open.requestId);
    openRemoteTab(open);
  }
}
onMachineTerminalServerReplaced(reopenRefusedAsOutdated);

/** Ask the Machine again about a request whose answer was lost: by its id, so
 *  it is never created twice. */
export function recheckRemoteOpen(requestId: string): void {
  const open = pending.find((p) => p.requestId === requestId);
  if (!open || open.status === "opening") return;
  update(requestId, { status: "opening", message: undefined, code: undefined, stall: undefined });
  void send(
    open,
    {
      operation: "attach",
      machineId: open.machineId,
      teamKey: open.teamKey,
      reference: { requestId },
      headed: open.headed !== false,
    },
    whereTheUserIs(open.teamKey),
  );
}

/** Only an authoritative definitive refusal permits a fresh launch intent. */
export function restartRefusedRemoteOpen(requestId: string): void {
  const open = pending.find((item) => item.requestId === requestId);
  if (
    !open ||
    open.status !== "refused" ||
    open.retryable ||
    !["newIntentRequired"].includes(open.code ?? "")
  )
    return;
  dismissRemoteOpen(requestId);
  openRemoteTab(open);
}
