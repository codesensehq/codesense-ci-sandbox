// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo, useState } from "react";
import type { RuntimeSessionTarget } from "@/features/viewer/lib/cli-workspace/transport/terminal-layout-source";
import { Loader2, WifiOff } from "lucide-react";
import {
  getViewerState,
  type TerminalTab,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { useTerminalLayout } from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { remoteShellTab } from "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source";
import { useMachine, useMachinesStore } from "@/features/machines/stores/machines-store";
import { machineSessionTarget } from "@/features/machines/stores/machine-projection-selectors";
import {
  connectMachineFor,
  openMachinesPanel,
  offerTerminalServerUpdate,
} from "@/features/machines/lib/machine-actions";
import { onMachineTerminalServerReplaced } from "@/features/shell/lib/machine-terminal-server";
import { sessionTarget } from "@/shared/lib/terminal-layout-projection";
import { onPrivateAuthorityRetired } from "@/shared/lib/private-terminal-authority";
import { getErrorMessage } from "@/shared/lib/error-message";
import { TerminalSlot } from "@/shared/terminal/terminal-slot";
import { terminalManager, type TerminalFindState } from "@/shared/terminal/terminal-manager";
import { TerminalFindBar, type TerminalSearch } from "./terminal-find-bar";
import { RemoteServerUpdateAction, useRemoteServerUpdate } from "./remote-server-update";
import {
  currentTerminalDisplay,
  terminalDisplayKey,
  type RemoteTerminalDisplayReference,
} from "@/shared/terminal/terminal-display-reference";
import type { TerminalThemeConfig } from "./terminal-constants";
import { shouldFocusTerminalOnReveal } from "../lib/focus-cascade";
import { endEndedTab } from "../lib/terminal-close";
import { TERMINAL_SESSION_ID_ATTR } from "../lib/terminal-tab-keys";
import {
  MachineCanvas,
  machineWatermarkText,
  useMachineTerminalTheme,
} from "@/features/machines/components/machine-canvas";

interface RemoteTerminalPaneProps {
  tab: TerminalTab;
  teamKey: string;
  workspaceId: string;
  /** The key the workspace's terminals are held under. */
  workspaceKey: string;
  visible: boolean;
  autoFocus: boolean;
  attachGeneration?: number;
  fontSizePx: number;
  themeConfig?: TerminalThemeConfig;
}

type Attachment =
  | { key: string; state: "attaching" | "attached" | "ended" }
  | { key: string; state: "elsewhere" | "failed"; message: string; code?: string };

/** The attach each remote session last succeeded with, by session: valid for
 *  the one connection (`admission`) it claimed input on. A pane mounting again
 *  on the same connection — a tab switch, a grid move — shows the display
 *  without asking the Machine again; a new connection attaches anew. */
const attached = new Map<string, { admission: string; state: "attached" | "ended" }>();

type AttachResult = ReturnType<typeof remoteShellTab>;

/** The attach in flight for each remote session, and the connection it is for.
 *  A pane that mounts again while its attach is still on the way — the tab grid
 *  reshaping, one tab ⇄ all tabs — waits on that same attach instead of asking
 *  the Machine a second time. */
const attaching = new Map<string, { admission: string; result: AttachResult }>();

onPrivateAuthorityRetired(() => {
  attached.clear();
  attaching.clear();
});

/** Forget every attach, as a relaunch of the app does. */
export function forgetRemoteAttaches(): void {
  attached.clear();
  attaching.clear();
}

/** `session`'s attach on `admission`: the one in flight, else a new one from
 *  `start` (always a new one for an explicit retry). */
function attachOnce(
  session: string,
  admission: string,
  retry: boolean,
  start: () => AttachResult,
): AttachResult {
  const inFlight = attaching.get(session);
  if (!retry && inFlight?.admission === admission) return inFlight.result;
  const result = start();
  attaching.set(session, { admission, result });
  const settle = () => {
    if (attaching.get(session)?.result === result) attaching.delete(session);
  };
  result.then(settle, settle);
  return result;
}

function cachedAttach(session: string, admission: string | null) {
  const hit = admission === null ? undefined : attached.get(session);
  if (hit && hit.admission !== admission) attached.delete(session);
  return hit?.admission === admission ? hit.state : undefined;
}

function offlineSinceLabel(since: number): string {
  return new Date(since).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/**
 * A remote tab's pane. It attaches its tab once for every admission of the
 * Machine (which claims the session's input for this app), then shows the
 * session's one display. While the Machine is offline the tab stays, saying
 * since when, with Connect; it reattaches in place once the Machine is back.
 */
export function RemoteTerminalPane({
  tab,
  teamKey,
  workspaceId,
  workspaceKey,
  visible,
  autoFocus,
  attachGeneration = 0,
  fontSizePx,
  themeConfig: baseTheme,
}: RemoteTerminalPaneProps) {
  const origin = tab.origin!;
  const machine = useMachine(origin.machineId);
  const label = machine?.label ?? origin.machineId;
  // The Machine's color tints the terminal's own background (and every notice
  // in its place), so a remote surface is told apart from a local one.
  const themeConfig = useMachineTerminalTheme(origin.machineId, baseTheme);
  const layout = useTerminalLayout();
  useMachinesStore((s) => s.authorityRevision);
  const offline = origin.offlineSince !== undefined;
  // Attach once the Machine is both admitted and reads as connected: the
  // display's every use is checked against both.
  const reachable = machine?.status.state === "connected";
  const target = offline || !reachable ? undefined : sessionTarget(layout, tab.sessionId);
  const epoch = layout.members[origin.member]?.privateAuthorityEpoch;
  // One attach per connection to the Machine: a new lifetime or connection
  // claims the session's input again; a re-admission of the same one does not.
  const admission = target ? JSON.stringify([target.member, target.namespace, epoch]) : null;
  const targetJson = target ? JSON.stringify(target) : null;
  const reference = useMemo<RemoteTerminalDisplayReference | null>(
    () =>
      targetJson && epoch
        ? {
            target: JSON.parse(targetJson) as RuntimeSessionTarget,
            teamKey,
            workspaceKey,
            privateAuthorityEpoch: epoch,
          }
        : null,
    [targetJson, epoch, teamKey, workspaceKey],
  );
  const [attempt, setAttempt] = useState(0);
  const attachKey = admission === null ? null : `${admission}:${attempt}`;
  const [attachment, setAttachment] = useState<Attachment | null>(() => {
    const state = attachKey === null ? undefined : cachedAttach(tab.sessionId, admission);
    return state && attachKey !== null ? { key: attachKey, state } : null;
  });
  const [connecting, setConnecting] = useState(false);
  const [connectError, setConnectError] = useState<string | null>(null);
  const [displayFailure, setDisplayFailure] = useState<string | null>(null);
  const agentStatusOutdated = useRemoteServerUpdate(tab.sessionId) !== null;
  const attachSettled =
    attachment?.key === attachKey &&
    (attachment.state === "attached" || attachment.state === "ended");

  useEffect(() => {
    if (
      !visible ||
      attachSettled ||
      attachKey === null ||
      admission === null ||
      !epoch ||
      !origin.handle
    )
      return;
    // Already attached on this connection: only an explicit retry asks again.
    const cached = attempt === 0 ? cachedAttach(tab.sessionId, admission) : undefined;
    if (cached) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- the attach this connection already made
      setAttachment((current) =>
        current?.key === attachKey ? current : { key: attachKey, state: cached },
      );
      return;
    }
    let live = true;
    setAttachment({ key: attachKey, state: "attaching" });
    setDisplayFailure(null);
    const handle = origin.handle;
    attachOnce(tab.sessionId, admission, attempt > 0, () =>
      remoteShellTab({
        operation: "attach",
        machineId: origin.machineId,
        teamKey,
        reference: { handle },
        headed: false,
      }),
    ).then(
      (result) => {
        if (!live) return;
        const shown = result.target;
        const held = sessionTarget(getViewerState().terminalLayout, tab.sessionId);
        if (
          (result.status === "opened" ||
            result.status === "alreadyLive" ||
            result.status === "ended") &&
          shown &&
          held &&
          shown.session === tab.sessionId &&
          shown.member.machineId === origin.machineId &&
          shown.namespace.serverId === held.namespace.serverId &&
          shown.namespace.epoch === held.namespace.epoch &&
          result.workspaceKey === workspaceKey &&
          machineSessionTarget(teamKey, held, workspaceKey, tab.sessionId)
        ) {
          const state = result.status === "ended" ? "ended" : "attached";
          attached.set(tab.sessionId, { admission, state });
          setAttachment({ key: attachKey, state });
          return;
        }
        attached.delete(tab.sessionId);
        setAttachment({
          key: attachKey,
          state: result.code === "attachedElsewhere" ? "elsewhere" : "failed",
          message:
            result.message ??
            (result.status === "pending"
              ? "Waiting for the machine to confirm this terminal."
              : "This terminal could not be attached."),
          code: result.code ?? undefined,
        });
      },
      (error: unknown) => {
        if (live)
          setAttachment({
            key: attachKey,
            state: "failed",
            message: `Attach failed: ${getErrorMessage(error)}`,
          });
      },
    );
    return () => {
      live = false;
    };
  }, [
    visible,
    attachSettled,
    attachKey,
    admission,
    attempt,
    epoch,
    origin.handle,
    origin.machineId,
    teamKey,
    workspaceKey,
    tab.sessionId,
  ]);

  // The server that refused this tab as too old was replaced: attach again.
  const outdated =
    (attachment?.state === "failed" || attachment?.state === "elsewhere") &&
    attachment.code === "terminalServerOutdated";
  useEffect(() => {
    if (!outdated) return;
    return onMachineTerminalServerReplaced((machineId) => {
      if (machineId === origin.machineId) setAttempt((n) => n + 1);
    });
  }, [outdated, origin.machineId]);

  // ⌘F and ⌘K work in a remote tab as in a local one.
  const getSearch = useCallback(
    (): TerminalSearch | null => (reference ? terminalManager.search(reference) : null),
    [reference],
  );
  const onFindStateChange = useCallback(
    (state: TerminalFindState | null) => {
      if (reference) terminalManager.setFindState(reference, state);
    },
    [reference],
  );
  const clearTerminal = useCallback((): boolean => {
    const term = reference ? terminalManager.terminal(reference) : null;
    if (!term || term.buffer.active.type === "alternate") return false;
    term.clear();
    return true;
  }, [reference]);
  const focusTerminal = useCallback(() => {
    if (reference) terminalManager.focus(reference);
  }, [reference]);

  const connect = () => {
    setConnecting(true);
    setConnectError(null);
    connectMachineFor(origin.machineId)
      .then((summary) => {
        if (summary.status.state !== "connected")
          setConnectError(`${label} didn't connect (${summary.status.state}).`);
      })
      .catch((error: unknown) =>
        setConnectError(`Couldn't connect to ${label}: ${getErrorMessage(error)}`),
      )
      .finally(() => setConnecting(false));
  };

  if (offline) {
    return (
      <PaneNotice testId="remote-terminal-offline" themeConfig={themeConfig}>
        <WifiOff className="size-4 text-[#8b949e]" aria-hidden />
        <p className="text-sm text-[#e6edf3]">
          {label} offline since {offlineSinceLabel(origin.offlineSince!)}
        </p>
        <p className="text-xs text-[#8b949e]">
          The terminal keeps running there. It reattaches here when {label} is back.
        </p>
        <div className="flex gap-2">
          <button
            type="button"
            data-testid="remote-terminal-connect"
            className={PRIMARY_BUTTON}
            disabled={connecting}
            onClick={connect}
          >
            {connecting && <Loader2 className="size-3.5 animate-spin" aria-hidden />}
            {connecting ? `Connecting to ${label}…` : "Connect"}
          </button>
          <button
            type="button"
            className={SECONDARY_BUTTON}
            onClick={() => openMachinesPanel(null)}
          >
            Manage Machines
          </button>
        </div>
        {connectError && (
          <p role="alert" className="text-xs text-[#ffa198]">
            {connectError}
          </p>
        )}
      </PaneNotice>
    );
  }

  const current = attachment && attachment.key === attachKey ? attachment : null;
  if (!current || current.state === "attaching") {
    return (
      <PaneNotice testId="remote-terminal-attaching" themeConfig={themeConfig}>
        <Loader2 className="size-4 animate-spin text-[#8b949e]" aria-hidden />
        <p className="text-xs text-[#8b949e]">
          {target ? `Attaching to ${label}…` : `Reconnecting to ${label}…`}
        </p>
      </PaneNotice>
    );
  }
  if (current.state === "elsewhere" || current.state === "failed") {
    return (
      <PaneNotice testId="remote-terminal-attach-failed" themeConfig={themeConfig}>
        <p className="max-w-md text-xs leading-5 text-[#c9d1d9]">{current.message}</p>
        <div className="flex gap-2">
          <button type="button" className={PRIMARY_BUTTON} onClick={() => setAttempt((n) => n + 1)}>
            {current.state === "elsewhere" ? "Attach here" : "Retry"}
          </button>
          {current.code === "terminalServerOutdated" && (
            <button
              type="button"
              className={SECONDARY_BUTTON}
              onClick={() => offerTerminalServerUpdate(origin.machineId)}
            >
              Update terminal server
            </button>
          )}
        </div>
      </PaneNotice>
    );
  }
  if (!reference) return null;
  return (
    <div
      className="relative flex h-full min-h-0 flex-col"
      data-testid="remote-terminal-pane"
      data-terminal-origin="remote"
      {...{ [TERMINAL_SESSION_ID_ATTR]: tab.sessionId }}
      data-terminal-display-key={terminalDisplayKey(reference)}
    >
      <TerminalFindBar
        sessionId={tab.sessionId}
        getSearch={getSearch}
        onClear={clearTerminal}
        onClose={focusTerminal}
        initialState={terminalManager.findState(reference)}
        onStateChange={onFindStateChange}
      />
      {(current.state === "ended" || tab.exit) && (
        <div className="flex items-center gap-3 border-b border-[#30363d] bg-[#161b22] px-3 py-1.5 text-xs text-[#8b949e]">
          <span>Ended on {label}</span>
          <button
            type="button"
            className="text-[#58a6ff] hover:underline"
            onClick={() => endEndedTab(teamKey, workspaceId, tab.id)}
          >
            Close
          </button>
        </div>
      )}
      {agentStatusOutdated && (
        <div
          data-testid="remote-terminal-server-outdated"
          className="flex items-center gap-3 border-b border-[#30363d] bg-[#161b22] px-3 py-1.5 text-xs text-[#d29922]"
        >
          <span>Agent status needs a terminal server update on {label}.</span>
          <RemoteServerUpdateAction sessionId={tab.sessionId} />
        </div>
      )}
      {displayFailure && (
        <div className="flex items-center gap-3 border-b border-[#30363d] bg-[#161b22] px-3 py-1.5 text-xs text-[#ffa198]">
          <span>{displayFailure}</span>
          <button
            type="button"
            className="text-[#58a6ff] hover:underline"
            onClick={() => setAttempt((n) => n + 1)}
          >
            Retry
          </button>
        </div>
      )}
      <div className="relative flex min-h-0 flex-1 flex-col">
        <TerminalSlot
          key={terminalDisplayKey(reference)}
          sessionId={reference}
          display={{ fontSizePx, themeConfig }}
          active={visible}
          className="min-h-0 flex-1"
          focusOnReveal={() =>
            autoFocus &&
            currentTerminalDisplay(reference) &&
            shouldFocusTerminalOnReveal(teamKey, workspaceId, attachGeneration)
          }
          onEnsured={(handle) => {
            if (handle.outcome.kind === "failed")
              setDisplayFailure(`Attach failed: ${handle.outcome.reason}`);
            else setDisplayFailure(null);
          }}
        />
        <MachineCanvas
          machineId={origin.machineId}
          hostname={machineWatermarkText(label, machine?.declaration?.host)}
          background={themeConfig.theme.background!}
        />
      </div>
    </div>
  );
}

const BUTTON =
  "inline-flex h-7 items-center gap-1.5 rounded-md px-3 text-xs font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50";
const PRIMARY_BUTTON = `${BUTTON} bg-[#1f6feb] text-white hover:bg-[#388bfd]`;
const SECONDARY_BUTTON = `${BUTTON} border border-[#30363d] bg-[#21262d] text-[#e6edf3] hover:bg-[#30363d]`;

function PaneNotice({
  testId,
  themeConfig,
  children,
}: {
  testId: string;
  themeConfig?: TerminalThemeConfig;
  children: React.ReactNode;
}) {
  return (
    <div
      data-testid={testId}
      data-terminal-origin="remote"
      className="flex h-full flex-col items-center justify-center gap-3 p-4 text-center"
      style={{ backgroundColor: themeConfig?.theme.background }}
    >
      {children}
    </div>
  );
}
