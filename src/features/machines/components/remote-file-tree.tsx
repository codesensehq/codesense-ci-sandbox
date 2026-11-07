// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";
import { Loader2, RefreshCw } from "lucide-react";

import { cn } from "@/lib/utils";
import {
  WorktreeFileTree,
  type WorktreeFileSelection,
} from "@/features/viewer/components/worktree-file-tree";
import { useNow } from "@/shared/hooks/use-now";
import { formatAgo } from "@/shared/lib/format-updated-ago";
import { type MachineSummary } from "@/tauri/commands";
import { machineFsList, type MachineWorkspaceDescriptor } from "@/tauri/draft-commands";

import { describeMachineStatus } from "../lib/machine-status";
import { clockTime, tildePath } from "../lib/machine-file-path";
import { useLatencyPhase } from "../lib/use-latency-phase";
import { MachineIcon } from "./machine-status-label";

/**
 * SW-RM-UI-1 — a workspace's worktree ON a machine: the SAME `WorktreeFileTree`
 * listing through the machine's terminal server, under remote chrome (Sally §6):
 *   - header `🖥 ‹machine› · ~/path · branch ⟳` — the machine name never truncates;
 *   - "Updated Ns ago", refresh on window focus + manual ⟳ (no live watcher in v1);
 *   - latency: a spinner after 150 ms, "Still loading from ‹machine›…" + Cancel
 *     after 2 s;
 *   - offline: the last-loaded tree stays, dimmed, under one quiet line naming the
 *     machine (or, with nothing loaded yet, just says so) — the CLI area's state
 *     panel owns Retry / Switch, never each tile.
 */
export function RemoteFileTree({
  machine,
  workspace,
  selectedPath,
  onOpenFile,
}: {
  machine: MachineSummary;
  workspace: MachineWorkspaceDescriptor;
  selectedPath: string | null;
  onOpenFile: (file: WorktreeFileSelection) => void;
}) {
  const online = machine.status.state === "connected";
  const machineId = machine.machineId;
  const [pending, setPending] = useState(0);
  const [loadedAt, setLoadedAt] = useState<number | null>(null);
  const [refreshToken, setRefreshToken] = useState(0);
  const [treeKey, setTreeKey] = useState(0);
  const [stopped, setStopped] = useState(false);
  const generation = useRef(0);
  const phase = useLatencyPhase(pending > 0);
  const now = useNow(loadedAt !== null, 5000);

  const list = useCallback(
    (dirPath: string) => {
      const gen = generation.current;
      setPending((n) => n + 1);
      return machineFsList(machineId, dirPath)
        .then((entries) => {
          if (generation.current === gen) setLoadedAt(Date.now());
          return entries;
        })
        .finally(() => {
          if (generation.current === gen) setPending((n) => Math.max(0, n - 1));
        });
    },
    [machineId],
  );

  // Coming back online re-lists what's expanded (the last-loaded tree may be stale).
  const [wasOnline, setWasOnline] = useState(online);
  if (wasOnline !== online) {
    setWasOnline(online);
    if (online) setRefreshToken((t) => t + 1);
  }

  // Refresh on window focus while the machine is reachable (no live watcher in v1).
  useEffect(() => {
    if (!online) return;
    const onFocus = () => setRefreshToken((t) => t + 1);
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [online]);

  /** Cancel stops the load for real: the tree unmounts (late results are dropped)
   *  and nothing re-lists until the user asks. */
  const cancel = () => {
    generation.current += 1;
    setPending(0);
    setStopped(true);
  };
  const loadAgain = () => {
    setStopped(false);
    setTreeKey((k) => k + 1);
  };

  const view = describeMachineStatus(machine, now);
  const offlineLine =
    machine.status.state === "unreachable"
      ? `${machine.label} is unreachable · last reached ${clockTime(machine.status.lastReachedAtMs, now)}`
      : `${machine.label}: ${view.word}`;

  return (
    <div data-testid="remote-file-tree" className="flex min-h-0 flex-col gap-1 text-xs">
      <div
        data-testid="remote-tree-header"
        className="flex min-w-0 items-center gap-1 border-b border-border pb-1"
        title={`${machine.label}:${workspace.worktreePath}${workspace.branch ? ` · ${workspace.branch}` : ""}`}
      >
        <MachineIcon machine={machine} size={12} />
        <span className="shrink-0 font-medium">{machine.label}</span>
        <span className="min-w-0 truncate text-muted-foreground">
          · {tildePath(workspace.worktreePath, machine.binding?.username ?? null)}
          {workspace.branch ? ` · ${workspace.branch}` : ""}
        </span>
        <button
          type="button"
          aria-label={`Refresh files from ${machine.label}`}
          disabled={!online || stopped}
          onClick={() => setRefreshToken((t) => t + 1)}
          className="ml-auto shrink-0 rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40"
        >
          <RefreshCw size={11} aria-hidden="true" />
        </button>
      </div>

      {/* Quiet on purpose: the CLI area's state panel above owns the recovery
          actions — each tile repeating them in red is noise. */}
      {!online && loadedAt !== null && (
        <p
          role="status"
          data-testid="remote-tree-offline"
          className="flex items-start gap-1 text-[11px] text-muted-foreground"
        >
          <span aria-hidden="true">{view.glyph}</span>
          <span className="min-w-0">{offlineLine}</span>
        </p>
      )}

      {online && loadedAt !== null && phase === "idle" && (
        <p data-testid="remote-tree-updated" className="text-[11px] text-muted-foreground">
          Updated {formatAgo(loadedAt, now)}
        </p>
      )}
      {phase === "spinner" && (
        <p
          data-testid="remote-tree-loading"
          className="flex items-center gap-1 text-muted-foreground"
        >
          <Loader2 size={11} className="animate-spin" aria-hidden="true" />
          Loading…
        </p>
      )}
      {phase === "slow" && (
        <p
          data-testid="remote-tree-slow"
          className="flex flex-wrap items-center gap-1 text-muted-foreground"
        >
          <Loader2 size={11} className="animate-spin" aria-hidden="true" />
          <span className="break-words">Still loading from {machine.label}…</span>
          <button
            type="button"
            onClick={cancel}
            className="rounded border border-border px-1 text-foreground hover:bg-accent"
          >
            Cancel
          </button>
        </p>
      )}

      {stopped ? (
        <p data-testid="remote-tree-stopped" className="px-1 py-1 text-muted-foreground">
          Stopped loading from {machine.label}.{" "}
          <button
            type="button"
            onClick={loadAgain}
            className="rounded border border-border px-1 text-foreground hover:bg-accent"
          >
            Load again
          </button>
        </p>
      ) : !online && loadedAt === null ? (
        <p role="status" data-testid="remote-tree-nothing-loaded" className="text-muted-foreground">
          Nothing loaded from {machine.label} yet — files appear once it's back.
        </p>
      ) : (
        <div className={cn("min-h-0 overflow-y-auto", !online && "pointer-events-none opacity-50")}>
          <WorktreeFileTree
            key={treeKey}
            rootPath={workspace.worktreePath}
            selectedPath={selectedPath}
            onSelect={onOpenFile}
            listDirectory={list}
            refreshToken={refreshToken}
          />
        </div>
      )}
    </div>
  );
}
