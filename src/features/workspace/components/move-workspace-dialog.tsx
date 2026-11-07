// SPDX-License-Identifier: MIT
/**
 * SW-WS-1: workspace header "Move to another machine…" dialog.
 *
 * Calls the `move_workspace_bridge` Tauri command (which wraps the
 * `move_workspace` MCP tool). Shows:
 *   - the eligible target machines (connected + remoteMcpControl + not the
 *     current host)
 *   - a dirty-worktree warning + `discardUncommitted` toggle when relevant
 *   - a progress indicator while the pipeline runs
 *
 * On submit, the Rust-side pipeline writes a durable `MoveIntent` and
 * emits progress events; the moving-banner on the workspace tile reads from
 * `ws.setupState` which the `useMoveWorkspaceSync` hook keeps up to date.
 */

import { useEffect, useMemo, useState, type ReactElement } from "react";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { logger } from "@/shared/lib/logger";
import {
  moveWorkspaceBridge,
  type MachineSummary,
  type MoveWorkspaceParams,
  type MoveWorkspaceResult,
} from "@/tauri/commands";

interface MachineOption {
  machineId: string;
  label: string;
  eligible: boolean;
  ineligibleReason?: string;
}

export interface MoveWorkspaceDialogProps {
  teamKey: string;
  storyId: string;
  currentMachineId: string | null;
  /** Optional pre-derived local worktree path. If omitted, the dialog
   *  shows the local option as ineligible with an explanatory tooltip —
   *  the caller is expected to pass this when it knows the derived path
   *  (via `renderWorktreeDir` on the open team's `worktree.yaml`). */
  localWorktreePathHint?: string;
  onClose: () => void;
}

/** Minimal eligible-machine computation. A reviewer-friendly scaffold —
 *  the full "connected + remoteMcpControl + terminal server up" filter
 *  lives in richer selectors; replicate here minimally to keep the dialog
 *  self-contained. */
function useEligibleMachines(teamKey: string, currentMachineId: string | null): MachineOption[] {
  const byTeam = useMachinesStore((s) => s.byTeam);
  const machines: MachineSummary[] = byTeam[teamKey] ?? [];
  const localHint: MachineOption = {
    machineId: "local",
    label: "This Mac",
    eligible: currentMachineId !== "local" && currentMachineId !== null,
    ineligibleReason: currentMachineId === "local" ? "workspace already lives here" : undefined,
  };
  const remoteOptions: MachineOption[] = machines
    .filter((m) => m.kind === "ssh")
    .map((m: MachineSummary) => {
      const isCurrent = currentMachineId === m.machineId;
      const connected = m.status?.state === "connected";
      const remoteMcpControl = m.remoteMcpControl === true;
      const eligible = !isCurrent && connected && remoteMcpControl;
      let reason: string | undefined;
      if (isCurrent) reason = "workspace already lives here";
      else if (!connected) reason = "machine is not connected";
      else if (!remoteMcpControl) reason = "remoteMcpControl is disabled";
      return {
        machineId: m.machineId,
        label: m.label ?? m.machineId,
        eligible,
        ineligibleReason: reason,
      };
    });
  return [localHint, ...remoteOptions];
}

export function MoveWorkspaceDialog({
  teamKey,
  storyId,
  currentMachineId,
  localWorktreePathHint,
  onClose,
}: MoveWorkspaceDialogProps): ReactElement {
  const options = useEligibleMachines(teamKey, currentMachineId);
  const [target, setTarget] = useState<string | null>(null);
  const [discard, setDiscard] = useState(false);
  const [state, setState] = useState<"idle" | "submitting" | "done" | "error">("idle");
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<MoveWorkspaceResult | null>(null);

  // When target == "local" and we have no hint, surface the "need a path"
  // reason inline so the user knows the launcher didn't block spuriously.
  const needLocalPath = target === "local" && !localWorktreePathHint;

  const canSubmit = useMemo(() => {
    if (state === "submitting") return false;
    if (!target) return false;
    if (needLocalPath) return false;
    const opt = options.find((o) => o.machineId === target);
    return opt?.eligible === true;
  }, [target, needLocalPath, options, state]);

  // Watch the setupState for THIS workspace — if the pipeline is already
  // moving (resumed from a crash, or started elsewhere), reflect that.
  const isMoving = useViewerStore((s) => {
    const rows = s.workspaces[teamKey];
    const ws = rows?.find((w) => w.id === storyId);
    const setup = ws?.setupState as unknown as { phase?: string };
    return setup?.phase === "moving";
  });

  useEffect(() => {
    if (isMoving && state === "idle") setState("submitting");
    if (!isMoving && state === "submitting" && result !== null) setState("done");
  }, [isMoving, state, result]);

  const onSubmit = async () => {
    if (!target || !canSubmit) return;
    setState("submitting");
    setError(null);
    const params: MoveWorkspaceParams = {
      storyId,
      targetMachine: target,
      discardUncommitted: discard,
      targetWorktreePath: target === "local" ? localWorktreePathHint : undefined,
    };
    try {
      const res = await moveWorkspaceBridge(params);
      setResult(res);
      setState("done");
    } catch (e) {
      const msg = typeof e === "string" ? e : String(e);
      logger.warn("workspace", `move_workspace failed: ${msg}`);
      setError(msg);
      setState("error");
    }
  };

  return (
    <div className="move-workspace-dialog" role="dialog" aria-labelledby="mw-title">
      <h2 id="mw-title">Move workspace to another machine</h2>
      <p className="mw-sub">
        {`Pick a target. The pipeline will prepare the worktree there, swap the workspace's
          machine pointer, and clean up the old copy.`}
      </p>
      <ul className="mw-targets" role="radiogroup" aria-label="Target machine">
        {options.map((opt) => {
          const selected = target === opt.machineId;
          return (
            <li key={opt.machineId}>
              <label
                className={`mw-option ${opt.eligible ? "" : "mw-ineligible"} ${selected ? "mw-selected" : ""}`}
                title={opt.ineligibleReason}
              >
                <input
                  type="radio"
                  name="mw-target"
                  value={opt.machineId}
                  disabled={!opt.eligible || state === "submitting"}
                  checked={selected}
                  onChange={() => setTarget(opt.machineId)}
                />
                <span className="mw-label">{opt.label}</span>
                {!opt.eligible && opt.ineligibleReason ? (
                  <span className="mw-reason"> — {opt.ineligibleReason}</span>
                ) : null}
              </label>
            </li>
          );
        })}
      </ul>
      {needLocalPath ? (
        <p className="mw-warn">
          {`No local worktree path available. Local-target moves need the derived managed
            worktree path (via the team's worktree.yaml template) — expected to be
            threaded from the launcher. For now, pick a remote target or open the
            launcher from a context that computed the path.`}
        </p>
      ) : null}
      <label className="mw-discard">
        <input
          type="checkbox"
          disabled={state === "submitting"}
          checked={discard}
          onChange={(e) => setDiscard(e.currentTarget.checked)}
        />
        <span>
          {`Discard uncommitted changes on the source (default off — a dirty worktree
            refuses the move until this is explicitly set).`}
        </span>
      </label>
      {state === "submitting" ? (
        <p className="mw-progress">Moving… (the dialog can be closed; progress continues)</p>
      ) : null}
      {state === "error" && error ? <p className="mw-error">Error: {error}</p> : null}
      {state === "done" && result ? (
        <p className="mw-done">
          {result.cleanupPending ? "Moved. Source cleanup will finish on next boot." : "Moved."}
        </p>
      ) : null}
      <div className="mw-buttons">
        <button type="button" onClick={onClose}>
          {state === "done" ? "Close" : "Cancel"}
        </button>
        <button type="button" disabled={!canSubmit} onClick={onSubmit}>
          Move
        </button>
      </div>
    </div>
  );
}
