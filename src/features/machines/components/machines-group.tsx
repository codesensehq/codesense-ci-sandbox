// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { listen } from "@tauri-apps/api/event";
import { Plus } from "lucide-react";
import { getErrorMessage } from "@/shared/lib/error-message";

import { RowAction } from "@/features/shell/components/connections-reserve";
import { reopen, restartCopy, useTerminalServer } from "@/features/shell/lib/terminal-server";
import { onMachineTerminalServerReplaced } from "@/features/shell/lib/machine-terminal-server";
import { useNow } from "@/shared/hooks/use-now";
import { useWindowVisible } from "@/shared/hooks/use-window-focused";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import {
  disconnectMachine,
  resolveMachineIdentity,
  machinePendingTabs,
  removeMachine,
  reopenMachinePendingTabs,
  retireLegacyTerminalServer,
  setMachineHidden,
  setMachineAutoConnect,
  setMachineRemoteMcpControl,
  unshareMachine,
  type MachineSummary,
  type MachineStats,
} from "@/tauri/commands";

import {
  PRIVATE_ACCOUNT_FOR_TABS,
  agentCallbacksNote,
  byLabel,
  describeMachineStatus,
  needsPrivateAccount,
  plural,
  TONE_CLASS,
} from "../lib/machine-status";
import {
  openMachinesPanel,
  openShellOn,
  runMachineAction,
  supersedeMachineAction,
} from "../lib/machine-actions";
import { reloadMachines, useTeamMachines } from "../stores/machines-store";
import { AttentionBadge, MachineIcon, MachineStatusLabel } from "./machine-status-label";
import {
  MachineAppearanceButton,
  useMachineAppearanceOpen,
  useSettleAppearanceRequest,
} from "./machine-appearance-popover";
import { forgetMachineAppearance } from "../lib/machine-appearance";
import { forgetMachineBuffers } from "../lib/remote-buffers";
import { describeTerminalServer } from "../lib/terminal-server-line";
import { TerminalServersNotice } from "./terminal-servers-notice";
import { UpdateAllDialog } from "./update-all-dialog";
import { followUpdateRuns, resumeUpdateAllRun } from "../lib/update-all";
import { outOfDateServers } from "../lib/out-of-date-servers";
import { UnreadableMachineFiles } from "./unreadable-machine-files";
import { StrayTerminalServers } from "./stray-terminal-servers";
import { MachineStatsLine } from "./machine-stats-line";

/** Actions that change what this window does with a machine, not the machine
 *  itself — never offered as Connections row verbs. */
const LENS_ONLY = new Set(["keepReadOnly"]);

function scopeLine(machine: MachineSummary): string {
  if (machine.kind === "local") return "This Mac · Terminal server";
  const where = machine.scope?.kind === "team" ? `Shared with ${machine.scope.team}` : "Only me";
  return machine.declaration ? `${where} · ${machine.declaration.host}` : where;
}

/**
 * SW-RM-UI-1 — the Connections tab's **Machines** group (between Agents and Code
 * hosts). This Mac is always the first row (its terminal server, + Restart while
 * another version's server runs there). With no
 * remote machine it is ONLY that row plus a quiet "Add machine" — no banner, no
 * first-run step. Row verbs follow the declaration's governance: a publisher sees
 * Edit / Unshare / Remove on shared rows; everyone else sees Set up / Reconnect /
 * Hide (a local-only flag). Renders nothing until the registry is available.
 */
export function MachinesGroup({
  teamKey,
  visible: tabVisible = true,
}: {
  teamKey: string;
  visible?: boolean;
}) {
  const machines = useTeamMachines(teamKey);
  const windowVisible = useWindowVisible();
  const visible = tabVisible && windowVisible;
  const [stats, setStats] = useState<Record<string, MachineStats>>({});
  useEffect(() => {
    if (!visible) return;
    return liveHub.watch("machineStats", "all", (snapshot) => {
      setStats(
        Object.fromEntries(snapshot.snapshot.map(({ id, ...row }) => [id, row as MachineStats])),
      );
    });
  }, [visible]);
  const now = useNow(true, 30_000);
  const ids = useMemo(() => machines?.map((m) => m.machineId) ?? null, [machines]);
  useSettleAppearanceRequest(ids);
  const [updateAll, setUpdateAll] = useState(false);
  // SW-AGR-19 PR-8: a terminal server update run (from any window, or an
  // agent) is followed here, so single machine updates wait for it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let live = true;
    void followUpdateRuns()
      .then((stop) => {
        if (live) unlisten = stop;
        else stop();
      })
      .catch(() => undefined);
    void resumeUpdateAllRun();
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);
  if (!machines) return null;

  const stale = outOfDateServers(machines);
  const local = machines.filter((m) => m.kind === "local");
  const remote = machines.filter((m) => m.kind === "ssh").sort(byLabel);

  return (
    <div data-testid="connections-machines-group" className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-3">
          <h3 className="shrink-0 text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Machines
          </h3>
          <TerminalServersNotice machines={machines} />
        </div>
        <div className="flex shrink-0 items-center gap-3">
          {stale.length > 0 && (
            <button
              type="button"
              data-testid="connections-update-all"
              onClick={() => setUpdateAll(true)}
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              Update all…
            </button>
          )}
          <button
            type="button"
            data-testid="connections-add-machine"
            onClick={() => openMachinesPanel({ mode: "add" })}
            className="flex shrink-0 items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
          >
            <Plus size={12} aria-hidden="true" />
            Add machine
          </button>
        </div>
      </div>
      <UpdateAllDialog
        machines={machines}
        teamKey={teamKey}
        open={updateAll}
        onOpenChange={setUpdateAll}
      />
      <ul className="flex flex-col gap-1.5">
        {local.map((m) => (
          <MachineRow
            key={m.machineId}
            machine={m}
            stats={stats[m.machineId]}
            teamKey={teamKey}
            now={now}
          />
        ))}
        {local.length > 0 && <StrayTerminalServers machines={machines} />}
        {remote.map((m) => (
          <MachineRow
            key={m.machineId}
            machine={m}
            stats={stats[m.machineId]}
            teamKey={teamKey}
            now={now}
          />
        ))}
        <UnreadableMachineFiles teamKey={teamKey} machines={machines} />
      </ul>
    </div>
  );
}

/** The declaration verbs that change what teammates or every team see — each asks
 *  first, inline in the row. */
type Confirming = "unshare" | "remove" | "remoteMcpControl" | "stopLegacy";

function confirmCopy(machine: MachineSummary, what: Confirming, sessions: number): string {
  if (what === "stopLegacy") {
    return `${plural(sessions, "terminal")} still run in the old terminal server on ${machine.label}. Stop it, ending them, and remove ${machine.label}?`;
  }
  if (what === "remoteMcpControl") {
    return `Allow agents on ${machine.label} to use CodeSense with your permissions? This setting is yours alone; it disconnects the current connection and applies to newly opened agent shells. It does not share your credentials.`;
  }
  if (what === "unshare") {
    return `Unshare ${machine.label}? Teammates lose it; it stays yours, for you only.`;
  }
  const legacy = needsPrivateAccount(machine)
    ? " CodeSense also stops the old terminal server it left running there, asking first if terminals still run in it."
    : "";
  return machine.scope?.kind === "team"
    ? `Remove ${machine.label} for everyone on ${machine.scope.team}?${legacy || " Sessions keep running on it."}`
    : `Remove ${machine.label} from all your teams?${legacy || " Its sessions keep running there."}`;
}

function MachineRow({
  machine,
  stats,
  teamKey,
  now,
}: {
  machine: MachineSummary;
  stats?: MachineStats;
  teamKey: string;
  now: number;
}) {
  const view = describeMachineStatus(machine, now);
  const note = agentCallbacksNote(machine);
  const hidden = machine.binding?.hidden === true;
  const shared = machine.scope?.kind === "team";
  const id = machine.machineId;
  const rowRef = useRef<HTMLLIElement>(null);
  const [appearanceOpen, setAppearanceOpen] = useMachineAppearanceOpen(id, rowRef);

  const [busy, setBusy] = useState(false);
  const [pendingConfirm, setConfirming] = useState<Confirming | null>(null);
  const [legacySessions, setLegacySessions] = useState(0);
  // Declaration actions require publishing rights; agent control belongs to the personal binding.
  const canControl =
    machine.declaration?.mode === "private-account" &&
    machine.binding != null &&
    machine.status.state !== "needsCredential";
  const confirming =
    pendingConfirm === "remoteMcpControl"
      ? canControl
        ? pendingConfirm
        : null
      : machine.canPublish
        ? pendingConfirm
        : null;
  const confirmRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    // Keyboard users land on the safe answer.
    if (confirming)
      confirmRef.current?.querySelector<HTMLButtonElement>("button:last-child")?.focus();
  }, [confirming]);
  const act = async (label: string, fn: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(true);
    supersedeMachineAction(id);
    try {
      await fn();
      await reloadMachines(teamKey);
    } catch (e) {
      toast.error(`Couldn't ${label} ${machine.label}: ${getErrorMessage(e)}`);
    } finally {
      setBusy(false);
    }
  };
  const confirmed = () => {
    const what = confirming;
    setConfirming(null);
    if (what === "remoteMcpControl")
      void act("allow agent control for", () => setMachineRemoteMcpControl(id, true));
    if (what === "unshare") void act("unshare", () => unshareMachine(id));
    if (what === "remove" || what === "stopLegacy") {
      void act("remove", async () => {
        if (needsPrivateAccount(machine)) {
          // Stop the server this record leaves behind while its sign-in still
          // exists. An unreachable host is no reason to keep the record.
          const retired = await retireLegacyTerminalServer(id, what === "stopLegacy").catch(
            () => null,
          );
          if (retired?.status === "running") {
            setLegacySessions(retired.sessions);
            setConfirming("stopLegacy");
            return;
          }
        }
        await removeMachine(id);
        forgetMachineBuffers(id);
        forgetMachineAppearance(id);
      });
    }
  };

  // SW-AGR-19: tabs an unfinished terminal server update left to reopen.
  const connected = machine.kind === "ssh" && machine.status.state === "connected";
  const [pendingTabs, setPendingTabs] = useState<number | null>(null);
  // Asked again once an update of this machine finished (its own restore
  // may have used the record).
  const [asked, setAsked] = useState(0);
  useEffect(
    () =>
      onMachineTerminalServerReplaced((replaced) => {
        if (replaced === id) setAsked((n) => n + 1);
      }),
    [id],
  );
  useEffect(() => {
    // The backend says when a machine's recorded tabs change.
    const unlisten = listen<string>("terminal-restore-changed", (event) => {
      if (event.payload === id) setAsked((n) => n + 1);
    }).catch(() => null);
    return () => {
      void unlisten.then((stop) => stop?.());
    };
  }, [id]);
  useEffect(() => {
    if (!connected) {
      setPendingTabs(null);
      return;
    }
    let live = true;
    machinePendingTabs(id)
      .then((count) => {
        if (live) setPendingTabs(count);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [connected, id, asked]);
  const reopenPending = () =>
    void act("reopen the tabs of", async () => {
      const summary = await reopenMachinePendingTabs(id);
      setPendingTabs(null);
      if (summary) {
        if (summary.complete) toast.success(summary.message);
        else toast.warning(summary.message);
      }
    });

  // The address is the declaration: only its publisher can edit it.
  const identityChange = machine.declaration?.identityChange;
  const statusVerbs = identityChange
    ? []
    : view.actions.filter(
        (a) => !LENS_ONLY.has(a.id) && (a.id !== "editAddress" || machine.canPublish),
      );
  return (
    <li
      ref={rowRef}
      data-testid={`machine-row-${id}`}
      className={`flex items-center gap-3 rounded-md border border-border px-3 py-2 ${hidden ? "opacity-60" : ""}`}
    >
      <span className="text-muted-foreground">
        <MachineIcon machine={machine} size={16} />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          {machine.kind !== "local" && (
            <MachineAppearanceButton
              machine={machine}
              open={appearanceOpen}
              onOpenChange={setAppearanceOpen}
            />
          )}
          <span className="shrink-0 text-sm font-medium">{machine.label}</span>
          <AttentionBadge count={machine.attention} />
        </div>
        <MachineStatusLabel
          view={view}
          testId={`machine-row-status-${id}`}
          className="max-w-full"
        />
        {identityChange && (
          <div
            className="space-y-1 break-words text-xs text-muted-foreground"
            role="group"
            aria-label="Machine identity review"
          >
            <p className="text-foreground">Machine identity changed in team config</p>
            {(
              [
                ["Accepted", identityChange.accepted],
                ["In team config", identityChange.declared],
              ] as const
            ).map(([label, identity]) => (
              <p key={label}>
                {label}: {identity.host}:{identity.port}
                {identity.jumpHost ? ` via ${identity.jumpHost}` : ""} ·{" "}
                {identity.hostKeyFingerprint ?? "No host key"} · Server{" "}
                {identity.serverId ?? "not pinned"}
              </p>
            ))}
            {identityChange.observedServerId && (
              <p>Observed server (diagnostic only): {identityChange.observedServerId}</p>
            )}
            {identityChange.author && (
              <p>
                {identityChange.author}
                {identityChange.date ? ` · ${identityChange.date}` : ""}
              </p>
            )}
          </div>
        )}
        <TerminalServerStatus machine={machine} />
        <MachineStatsLine machine={{ label: machine.label, status: machine.status, ...stats }} />
        <p className="truncate text-xs text-muted-foreground">
          {scopeLine(machine)}
          {hidden ? " · Hidden" : ""}
        </p>
        {note && (
          <p data-testid={`machine-row-note-${id}`} className="text-[11px] text-muted-foreground">
            <span aria-hidden="true">ⓘ </span>
            {note}
          </p>
        )}
        {needsPrivateAccount(machine) && (
          <p
            data-testid={`machine-legacy-note-${id}`}
            className="text-[11px] text-amber-600 dark:text-amber-400"
          >
            <span aria-hidden="true">ⓘ </span>
            {PRIVATE_ACCOUNT_FOR_TABS}
            {machine.canPublish ? "." : ". Ask whoever added it to switch it."}
          </p>
        )}
        {machine.kind === "ssh" && machine.binding && (
          <label
            className="flex items-center gap-2 text-xs"
            title="Your setting. Background reconnect never grants trust or prompts for credentials."
          >
            <input
              type="checkbox"
              checked={machine.binding.autoConnect === true}
              disabled={busy}
              aria-label={`Reconnect ${machine.label} in background`}
              onChange={(event) =>
                void act("update background reconnect for", () =>
                  setMachineAutoConnect(id, event.target.checked),
                )
              }
            />
            Reconnect in background
            <span className="text-muted-foreground">Your setting · no credential prompts</span>
          </label>
        )}

        {canControl && (
          <label className="flex items-center gap-2 text-xs">
            <input
              type="checkbox"
              checked={machine.remoteMcpControl === true}
              disabled={busy}
              aria-label="Allow agents on this machine to use CodeSense"
              onChange={(event) => {
                if (event.target.checked) setConfirming("remoteMcpControl");
                else
                  void act("disable agent control for", () =>
                    setMachineRemoteMcpControl(id, false),
                  );
              }}
            />
            Allow agents on this machine to use CodeSense
            <span className="text-muted-foreground">Your setting</span>
          </label>
        )}
      </div>
      {confirming ? (
        <div
          ref={confirmRef}
          role="group"
          aria-label={`Confirm ${confirming}`}
          data-testid={`machine-confirm-${id}`}
          className="flex shrink-0 items-center gap-1"
          onKeyDown={(e) => {
            if (e.key === "Escape") setConfirming(null);
          }}
        >
          <span className="max-w-64 text-xs text-foreground">
            {confirmCopy(machine, confirming, legacySessions)}
          </span>
          <RowAction
            testid={`machine-confirm-yes-${id}`}
            label={
              confirming === "remoteMcpControl"
                ? "Allow"
                : confirming === "unshare"
                  ? "Unshare"
                  : confirming === "stopLegacy"
                    ? "Stop and remove"
                    : "Remove"
            }
            destructive
            disabled={busy}
            onClick={confirmed}
          />
          <RowAction
            testid={`machine-confirm-no-${id}`}
            label="Cancel"
            onClick={() => setConfirming(null)}
          />
        </div>
      ) : (
        <div className="flex max-w-[55%] shrink-0 flex-wrap items-center justify-end gap-1">
          {identityChange && (
            <>
              <RowAction
                testid={`machine-identity-confirm-${id}`}
                label="Confirm"
                disabled={busy}
                onClick={() =>
                  void act("confirm the identity of", () =>
                    resolveMachineIdentity(id, identityChange.revision, true),
                  )
                }
              />
              <RowAction
                testid={`machine-identity-refuse-${id}`}
                label="Keep refusing"
                disabled={busy}
                onClick={() =>
                  void act("keep refusing", () =>
                    resolveMachineIdentity(id, identityChange.revision, false),
                  )
                }
              />
            </>
          )}
          {machine.kind === "local" && <LocalServerRestart />}
          {pendingTabs !== null && pendingTabs > 0 && (
            <RowAction
              testid={`machine-action-reopenTabs-${id}`}
              label={`Reopen ${plural(pendingTabs, "tab")} from before the update`}
              disabled={busy}
              onClick={reopenPending}
            />
          )}
          {statusVerbs.map((a) => (
            <RowAction
              key={a.id}
              testid={`machine-action-${a.id}-${id}`}
              label={a.label}
              disabled={busy}
              onClick={() => void runMachineAction(a.id, machine, teamKey)}
            />
          ))}
          {machine.kind === "ssh" &&
            machine.status.state === "connected" &&
            !needsPrivateAccount(machine) && (
              <RowAction
                testid={`machine-action-openShell-${id}`}
                label="Open shell"
                disabled={busy}
                onClick={() => openShellOn(machine, teamKey)}
              />
            )}
          {machine.kind === "ssh" && machine.status.state === "connected" && (
            <RowAction
              testid={`machine-action-disconnect-${id}`}
              label="Disconnect"
              disabled={busy}
              onClick={() => void act("disconnect", () => disconnectMachine(id))}
            />
          )}
          {machine.kind === "ssh" && (
            <RowAction
              testid={`machine-action-appearance-${id}`}
              label="Appearance…"
              onClick={() => setAppearanceOpen(true)}
            />
          )}
          {!identityChange && needsPrivateAccount(machine) && machine.canPublish && (
            <RowAction
              testid={`machine-action-switchPrivate-${id}`}
              label="Switch to private account"
              disabled={busy}
              onClick={() =>
                openMachinesPanel({ mode: "setUp", machineId: id, privateAccount: true })
              }
            />
          )}
          {machine.kind === "ssh" && machine.canPublish && (
            <>
              <RowAction
                testid={`machine-action-edit-${id}`}
                label="Edit"
                disabled={busy}
                onClick={() => openMachinesPanel({ mode: "setUp", machineId: id })}
              />
              {shared && (
                <RowAction
                  testid={`machine-action-unshare-${id}`}
                  label="Unshare"
                  disabled={busy}
                  onClick={() => setConfirming("unshare")}
                />
              )}
              <RowAction
                testid={`machine-action-remove-${id}`}
                label="Remove"
                disabled={busy}
                onClick={() => setConfirming("remove")}
              />
            </>
          )}
          {machine.kind === "ssh" && !machine.canPublish && (
            <RowAction
              testid={`machine-action-${hidden ? "unhide" : "hide"}-${id}`}
              label={hidden ? "Unhide" : "Hide"}
              disabled={busy}
              onClick={() =>
                void act(hidden ? "unhide" : "hide", () => setMachineHidden(id, !hidden))
              }
            />
          )}
        </div>
      )}
    </li>
  );
}

/** This Mac's Restart: offered only while an incompatible terminal server (another
 *  CodeSense version's) runs here — restarting a healthy one would only end its
 *  terminals. It opens the RT-6b prompt, which names what the restart closes. */
function LocalServerRestart() {
  const { server } = useTerminalServer();
  if (!server) return null;
  return (
    <RowAction
      testid="machine-action-restartServer-local"
      label={restartCopy(server.liveSessions).action}
      onClick={() => void reopen()}
    />
  );
}

/** SW-AGR-19: the machine's running terminal server next to the app — its
 *  version and whether it is this app's build; for This Mac also when it
 *  started and the executable it runs. Quiet unless it needs attention; a
 *  remote row's own status action offers the update (This Mac's comes with
 *  its update flow). */
function TerminalServerStatus({ machine }: { machine: MachineSummary }) {
  const line = describeTerminalServer(machine);
  if (!line) return null;
  return (
    <div
      data-testid={`machine-terminal-server-${machine.machineId}`}
      className="min-w-0 text-[11px] text-muted-foreground"
    >
      <p className="truncate">
        <span className={line.tone === "warn" ? TONE_CLASS.warn : undefined}>{line.text}</span>
        {line.details.length > 0 && ` · ${line.details.join(" · ")}`}
      </p>
      {line.path && (
        // The end of a path names it: elide from the start (rtl box, ltr text).
        <p
          data-testid={`machine-terminal-server-path-${machine.machineId}`}
          className="truncate text-left font-mono text-[11px] [direction:rtl]"
          title={line.path}
        >
          <bdi dir="ltr">{line.path}</bdi>
        </p>
      )}
    </div>
  );
}
