// SPDX-License-Identifier: MIT
import { useEffect, useId, useRef, useState } from "react";
import { Loader2, Monitor, Server } from "lucide-react";
import { toast } from "sonner";
import { useTeamMachines } from "@/features/machines/stores/machines-store";
import {
  connectMachineFor,
  openMachinesPanel,
  type MachineSummary,
} from "@/features/machines/lib/machine-actions";
import {
  PRIVATE_ACCOUNT_FOR_TABS,
  describeMachineStatus,
  needsPrivateAccount,
} from "@/features/machines/lib/machine-status";
import { useAgentRows } from "@/features/agents/lib/agent-names";
import {
  launchAgentInWorkspace,
  orderStoryLaunchAgents,
} from "@/features/story-launch/story-agent-launch";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import {
  currentPrivateAuthorityEpoch,
  onPrivateAuthorityRetired,
} from "@/shared/lib/private-terminal-authority";
import {
  listPreparedRemoteWorkspaces,
  type PreparedRemoteWorkspaceChoice,
  type RemoteShellLaunch,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-launch-source";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  NATIVE_LAUNCHES,
  PREVIEW_RUNTIME_NOTE,
  REMOTE_LAUNCH_CLIS,
} from "@/shared/lib/agent-runtimes";
import { agentCliLabel } from "@/shared/lib/agent-clis";
import { PreviewBadge, previewNoteRef } from "@/shared/components/preview-badge";
import {
  NEW_WORKSPACE_TERMINAL_EVENT,
  takeNewWorkspaceTerminalRequest,
  type NewTerminalStart,
  type NewWorkspaceTerminalDetail,
} from "../lib/new-workspace-terminal";
import { newTerminalTab } from "../lib/terminal-tab-keys";
import { openRemoteTab } from "../lib/remote-tab-open";
import { useRemoteWorkspaceLocation } from "@/features/machines/components/machine-location-badge";

const WHERE_KEY = "codesense.new-terminal-where";
const LOCAL = "local";

/** The place a workspace's last chosen new terminal opened, per viewer. */
function rememberedWhere(scope: string): string {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(WHERE_KEY) ?? "{}");
    if (saved && typeof saved === "object" && scope in saved) {
      const value = (saved as Record<string, unknown>)[scope];
      if (typeof value === "string") return value;
    }
  } catch {
    /* No saved choice: This Mac. */
  }
  return LOCAL;
}

function rememberWhere(scope: string, machineId: string): void {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(WHERE_KEY) ?? "{}");
    const next = saved && typeof saved === "object" ? { ...saved } : {};
    localStorage.setItem(WHERE_KEY, JSON.stringify({ ...next, [scope]: machineId }));
  } catch {
    /* The choice is a convenience only. */
  }
}

const LAUNCHES: { id: RemoteShellLaunch; label: string }[] = NATIVE_LAUNCHES.map((id) => ({
  id,
  label: id === "shell" ? "Shell" : agentCliLabel(id),
}));

/**
 * The new-tab menu's chooser (⌘⇧N, ⌥-click on "+"): Where — This Mac or a
 * connected Machine — and what to start there. A Machine's tab opens in its
 * account home and sits in the workspace's tab bar with This Mac's.
 */
export function NewTerminalChooser({
  teamKey,
  workspaceId,
}: {
  teamKey: string;
  workspaceId: string;
}) {
  const [open, setOpen] = useState(false);
  // What an opener chose ahead (Start managed: a CLI, its tab's folder and machine). `request`
  // counts the opens: a new one into an open chooser starts it over with its own choices.
  const [start, setStart] = useState<Partial<NewTerminalStart> & { request: number }>({
    request: 0,
  });
  const owner = useCurrentUser()?.principal ?? null;
  const scope = JSON.stringify([teamKey, workspaceId]);
  useEffect(() => {
    const show = (detail: NewWorkspaceTerminalDetail) => {
      setStart((previous) => ({
        launch: detail.launch,
        cwd: detail.cwd,
        machineId: detail.machineId,
        request: previous.request + 1,
      }));
      setOpen(true);
    };
    // A request made while this workspace was not shown waits for its chooser to mount.
    const waiting = takeNewWorkspaceTerminalRequest(teamKey, workspaceId);
    if (waiting) show(waiting);
    const onOpen = (event: Event) => {
      const detail = (event as CustomEvent<NewWorkspaceTerminalDetail>).detail;
      if (detail.teamKey !== teamKey || detail.workspaceId !== workspaceId) return;
      takeNewWorkspaceTerminalRequest(teamKey, workspaceId);
      show(detail);
    };
    window.addEventListener(NEW_WORKSPACE_TERMINAL_EVENT, onOpen);
    return () => window.removeEventListener(NEW_WORKSPACE_TERMINAL_EVENT, onOpen);
  }, [teamKey, workspaceId]);
  if (!open) return null;
  return (
    <ChooserDialog
      key={JSON.stringify([owner, teamKey, workspaceId, start.request])}
      teamKey={teamKey}
      workspaceId={workspaceId}
      initialWhere={start.machineId ?? rememberedWhere(scope)}
      initialLaunch={start.launch}
      initialCwd={start.cwd ?? null}
      onChosen={(machineId) => rememberWhere(scope, machineId)}
      onClose={() => setOpen(false)}
    />
  );
}

/** A preselected launch, when it is one the chooser offers. */
function launchChoice(launch: string | undefined): RemoteShellLaunch {
  return LAUNCHES.find((option) => option.id === launch)?.id ?? "shell";
}

function ChooserDialog({
  teamKey,
  workspaceId,
  initialWhere,
  initialLaunch,
  initialCwd,
  onChosen,
  onClose,
}: {
  teamKey: string;
  workspaceId: string;
  initialWhere: string;
  /** The CLI to start chosen up front. */
  initialLaunch?: string;
  /** The folder a session started on `initialWhere` opens in, instead of the workspace's. */
  initialCwd: string | null;
  onChosen: (machineId: string) => void;
  onClose: () => void;
}) {
  // A workspace that lives on a machine opens its terminals there, at its
  // worktree: that machine is its only Where.
  const home = useRemoteWorkspaceLocation(teamKey, workspaceId);
  const machines = (useTeamMachines(teamKey) ?? []).filter(
    (m) => m.kind === "ssh" && (!home || m.machineId === home.machineId),
  );
  const [launch, setLaunch] = useState<RemoteShellLaunch>(() => launchChoice(initialLaunch));
  const previewNoteId = useId();
  /** The preselected folder applies on its own machine only. */
  const cwdOn = (where: string) =>
    initialCwd && where === initialWhere ? { cwd: initialCwd } : {};
  const [machineId, setMachineId] = useState<string | null>(null);
  const owner = useCurrentUser()?.principal ?? null;
  const { rows: agentRows, error: agentError } = useAgentRows();
  const orderedAgents = orderStoryLaunchAgents(agentRows, teamKey);
  const [agentKey, setAgentKey] = useState("");
  const remoteDestination = !!home || (!!machineId && !!agentKey);
  const agents = orderedAgents.filter(
    (agent) =>
      !remoteDestination || (REMOTE_LAUNCH_CLIS as readonly string[]).includes(agent.runtime),
  );
  const selectedAgent = orderedAgents.find((agent) => agent.agentKey === agentKey);
  const [preparedHandle, setPreparedHandle] = useState("");
  const [refresh, setRefresh] = useState(0);
  const [prepared, setPrepared] = useState<{
    scope: string;
    epoch: string;
    choices: PreparedRemoteWorkspaceChoice[];
  } | null>(null);
  const [preparedError, setPreparedError] = useState<string | null>(null);
  const preparedScope = JSON.stringify([owner, teamKey, workspaceId, machineId, agentKey, refresh]);
  const selectedMachine = machines.find((machine) => machine.machineId === machineId);
  const choices =
    prepared?.scope === preparedScope && prepared.epoch === currentPrivateAuthorityEpoch()
      ? prepared.choices
      : [];
  const selectedWorkspace = choices.find((choice) => choice.handle === preparedHandle);
  useEffect(() => {
    if (!agentKey || !machineId || owner === null) return;
    const epoch = currentPrivateAuthorityEpoch();
    if (epoch === null) return;
    let active = true;
    const stop = onPrivateAuthorityRetired(() => {
      active = false;
      setPrepared(null);
    });
    void listPreparedRemoteWorkspaces(machineId, teamKey, workspaceId).then(
      (choices) => {
        if (active && currentPrivateAuthorityEpoch() === epoch) {
          setPrepared({ scope: preparedScope, epoch, choices });
          setPreparedError(null);
        }
      },
      (error: unknown) => {
        if (active && currentPrivateAuthorityEpoch() === epoch) {
          setPrepared({ scope: preparedScope, epoch, choices: [] });
          setPreparedError(getErrorMessage(error));
        }
      },
    );
    return () => {
      active = false;
      stop();
    };
  }, [agentKey, machineId, owner, teamKey, workspaceId, preparedScope]);
  const [openedAt] = useState(() => Date.now());
  const [connecting, setConnecting] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const initialRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    const remembered = initialRef.current;
    (remembered && !remembered.disabled
      ? remembered
      : dialogRef.current?.querySelector<HTMLButtonElement>("ul button:not([disabled])")
    )?.focus();
    return () => {
      live.current = false;
    };
  }, []);

  const openOn = (machine: MachineSummary | null) => {
    if (machine && home) {
      onChosen(machine.machineId);
      onClose();
      openRemoteTab({
        teamKey,
        workspaceId,
        machineId: machine.machineId,
        label: machine.label,
        launch,
        cwd: home.cwd,
        // Claude or Codex in a workspace on a machine starts as an agent (its
        // built-in one when none is chosen), set up in the worktree there first.
        ...(agentKey
          ? { agentKey, preparedWorkspaceHandle: home.handle }
          : launch !== "shell"
            ? { agentKey: `builtin/${launch}`, preparedWorkspaceHandle: home.handle }
            : {}),
      });
      return;
    }
    if (machine && agentKey) {
      setMachineId(machine.machineId);
      setPreparedHandle("");
      setPreparedError(null);
      return;
    }
    if (!machine) {
      if (agentKey || launch !== "shell") {
        setConnecting(LOCAL);
        void Promise.resolve()
          .then(() =>
            launchAgentInWorkspace({
              teamKey,
              workspaceKey: workspaceId,
              agent: agentKey || `builtin/${launch}`,
              ...cwdOn(LOCAL),
            }),
          )
          .then((result) => {
            for (const warning of result.warnings ?? []) toast.warning(warning.message);
            if (!live.current) return;
            onChosen(LOCAL);
            onClose();
          })
          .catch((error: unknown) => {
            if (live.current) setError(getErrorMessage(error));
          })
          .finally(() => {
            if (live.current) setConnecting(null);
          });
        return;
      }
      newTerminalTab(teamKey, workspaceId);
      onChosen(LOCAL);
      onClose();
      return;
    }
    onChosen(machine.machineId);
    onClose();
    openRemoteTab({
      teamKey,
      workspaceId,
      machineId: machine.machineId,
      label: machine.label,
      launch,
      ...cwdOn(machine.machineId),
    });
  };

  const connectThenOpen = (machine: MachineSummary) => {
    setConnecting(machine.machineId);
    setError(null);
    connectMachineFor(machine.machineId)
      .then((summary) => {
        if (!live.current) return;
        if (summary.status.state === "connected") openOn({ ...machine, ...summary });
        else
          setError(
            `${machine.label} didn't connect: ${describeMachineStatus(summary, Date.now()).word}`,
          );
      })
      .catch((e: unknown) => {
        if (live.current) setError(`Couldn't connect to ${machine.label}: ${getErrorMessage(e)}`);
      })
      .finally(() => {
        if (live.current) setConnecting(null);
      });
  };

  return (
    <div
      className="absolute inset-0 z-40 flex items-center justify-center bg-[#0d1117]/70 p-4"
      onPointerDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-label="New terminal"
        data-testid="new-terminal-chooser"
        className="w-full max-w-[24rem] rounded-xl border border-[#30363d] bg-[#161b22] p-4 text-sm text-[#e6edf3] shadow-[0_16px_48px_rgba(1,4,9,0.6)]"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
        }}
      >
        <h2 className="text-[15px] font-semibold">New terminal</h2>
        <span id={previewNoteId} hidden>
          {PREVIEW_RUNTIME_NOTE}
        </span>
        {initialCwd && (
          <p className="mt-1 truncate text-xs" title={initialCwd}>
            <span className="opacity-70">Opens in</span>{" "}
            <span className="font-mono">{initialCwd}</span>
          </p>
        )}
        <div role="group" aria-label="Start" className="mt-3 flex flex-wrap gap-1">
          {LAUNCHES.map((option) => (
            <button
              key={option.id}
              type="button"
              disabled={
                connecting !== null ||
                (!!home &&
                  option.id !== "shell" &&
                  !(REMOTE_LAUNCH_CLIS as readonly string[]).includes(option.id))
              }
              aria-pressed={!agentKey && launch === option.id}
              aria-describedby={previewNoteRef(previewNoteId, option.id)}
              onClick={() => {
                setLaunch(option.id);
                setAgentKey("");
                setMachineId(null);
                setPreparedHandle("");
                setPrepared(null);
                setPreparedError(null);
              }}
              className={`inline-flex h-7 items-center gap-1.5 rounded-md px-2.5 text-xs ${!agentKey && launch === option.id ? "bg-[#1f6feb] text-white" : "border border-[#30363d] bg-[#21262d] text-[#c9d1d9] hover:bg-[#30363d]"} [--cs-preview-ink:currentColor] [--cs-preview-line:color-mix(in_oklab,currentColor_55%,transparent)]`}
            >
              {option.label}
              <PreviewBadge cli={option.id} className="h-4 px-[5px] text-[10.5px]" />
            </button>
          ))}
        </div>
        <label className="mt-3 block text-xs">
          Saved Agent
          <select
            disabled={connecting !== null}
            aria-label="Saved Agent"
            value={agentKey}
            className="mt-1 block w-full rounded border border-[#30363d] bg-[#0d1117] p-2"
            onChange={(event) => {
              setAgentKey(event.target.value);
              setPreparedHandle("");
              setPreparedError(null);
              const agent = agents.find((item) => item.agentKey === event.target.value);
              if (agent) setLaunch(agent.runtime);
            }}
          >
            <option value="">Use the selected runtime</option>
            {agents.map((agent) => (
              <option key={agent.agentKey} value={agent.agentKey}>
                {agent.name}
                {agent.place ? ` · ${agent.place}` : ""}
              </option>
            ))}
          </select>
        </label>
        {agentError && (
          <p role="alert" className="mt-2 text-xs text-[#ffa198]">
            {agentError}
          </p>
        )}
        <p className="mt-3 text-[11px] font-medium uppercase tracking-wide text-[#8b949e]">Where</p>
        <ul role="list" aria-label="Where" className="mt-1.5 flex flex-col gap-1">
          <li>
            <button
              ref={initialWhere === LOCAL ? initialRef : undefined}
              type="button"
              data-testid="new-terminal-where-local"
              disabled={!!home || connecting !== null}
              title={home ? `This workspace lives on ${machines[0]?.label ?? home.machineId}` : ""}
              onClick={() => openOn(null)}
              className={ROW}
            >
              <Monitor className="size-4 text-[#8b949e]" aria-hidden />
              This Mac
            </button>
          </li>
          {machines.map((machine) => {
            if (needsPrivateAccount(machine))
              return (
                <li key={machine.machineId} className="px-2 py-1 text-xs text-[#8b949e]">
                  {machine.label}: {PRIVATE_ACCOUNT_FOR_TABS}.{" "}
                  <button
                    type="button"
                    className="underline"
                    onClick={() =>
                      openMachinesPanel({
                        mode: "setUp",
                        machineId: machine.machineId,
                        privateAccount: true,
                      })
                    }
                  >
                    Set up
                  </button>
                </li>
              );
            const view = describeMachineStatus(machine, openedAt);
            const connected = machine.status.state === "connected";
            const canConnect = view.actions.some((a) => a.id === "connect" || a.id === "retry");
            return (
              <li key={machine.machineId}>
                <button
                  ref={initialWhere === machine.machineId ? initialRef : undefined}
                  type="button"
                  data-testid={`new-terminal-where-${machine.machineId}`}
                  aria-pressed={!!agentKey && machineId === machine.machineId}
                  disabled={
                    connecting !== null ||
                    (!connected && !canConnect) ||
                    (!(REMOTE_LAUNCH_CLIS as readonly string[]).includes(
                      selectedAgent?.runtime ?? launch,
                    ) &&
                      launch !== "shell")
                  }
                  title={
                    !(REMOTE_LAUNCH_CLIS as readonly string[]).includes(
                      selectedAgent?.runtime ?? launch,
                    ) && launch !== "shell"
                      ? `${agentCliLabel(selectedAgent?.runtime ?? launch)} launches locally only`
                      : (view.blockedReason ?? undefined)
                  }
                  onClick={() => (connected ? openOn(machine) : connectThenOpen(machine))}
                  className={ROW}
                >
                  {connecting === machine.machineId ? (
                    <Loader2 className="size-4 animate-spin text-[#8b949e]" aria-hidden />
                  ) : (
                    <Server className="size-4 text-[#8b949e]" aria-hidden />
                  )}
                  <span className="flex-1 truncate text-left">{machine.label}</span>
                  {!connected && (
                    <span className="text-xs text-[#8b949e]">
                      {connecting === machine.machineId
                        ? "Connecting…"
                        : canConnect
                          ? "Connect and open"
                          : view.word}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
        {agentKey && machineId && (
          <div className="mt-3">
            <label className="block text-xs">
              Prepared workspace on {selectedMachine?.label ?? machineId}
              <select
                aria-label="Prepared workspace"
                value={selectedWorkspace?.handle ?? ""}
                className="mt-1 block w-full rounded border border-[#30363d] bg-[#0d1117] p-2"
                onChange={(event) => setPreparedHandle(event.target.value)}
              >
                <option value="">Choose a prepared workspace</option>
                {choices.map((choice) => (
                  <option key={choice.handle} value={choice.handle}>
                    {choice.branch} · {choice.cwd}
                  </option>
                ))}
              </select>
            </label>
            <button
              type="button"
              className="mt-1 text-xs text-[#58a6ff]"
              onClick={() => {
                setPreparedHandle("");
                setPreparedError(null);
                setRefresh((value) => value + 1);
              }}
            >
              Refresh workspaces
            </button>
            {prepared?.scope !== preparedScope ? (
              <p className="mt-1 text-xs">Loading prepared workspaces…</p>
            ) : (
              choices.length === 0 && (
                <p className="mt-1 text-xs text-[#8b949e]">
                  Prepare a remote workspace first, then refresh.
                </p>
              )
            )}
            {preparedError && (
              <p role="alert" className="mt-1 text-xs text-[#ffa198]">
                {preparedError}
              </p>
            )}
            <button
              type="button"
              className="mt-3 rounded bg-[#238636] px-3 py-1.5 text-xs"
              disabled={!selectedWorkspace || selectedMachine?.status.state !== "connected"}
              onClick={() => {
                if (
                  !selectedWorkspace ||
                  !selectedMachine ||
                  selectedMachine.status.state !== "connected" ||
                  prepared?.epoch !== currentPrivateAuthorityEpoch()
                )
                  return;
                onChosen(selectedMachine.machineId);
                openRemoteTab({
                  teamKey,
                  workspaceId,
                  machineId: selectedMachine.machineId,
                  label: selectedMachine.label,
                  launch,
                  agentKey,
                  preparedWorkspaceHandle: selectedWorkspace.handle,
                  cwd: selectedWorkspace.cwd,
                });
                onClose();
              }}
            >
              Open Agent
            </button>
          </div>
        )}
        {error && (
          <p role="alert" className="mt-2 text-xs text-[#ffa198]">
            {error}
          </p>
        )}
        <div className="mt-3 flex justify-between">
          <button
            type="button"
            className="text-xs text-[#8b949e] hover:text-[#e6edf3]"
            onClick={() => {
              onClose();
              openMachinesPanel({ mode: "add" });
            }}
          >
            Add Machine
          </button>
          <button
            type="button"
            className="text-xs text-[#8b949e] hover:text-[#e6edf3]"
            onClick={onClose}
          >
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}

const ROW =
  "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[13px] hover:bg-[#21262d] focus-visible:bg-[#21262d] focus-visible:outline focus-visible:outline-1 focus-visible:outline-[#388bfd] disabled:cursor-not-allowed disabled:opacity-50";
