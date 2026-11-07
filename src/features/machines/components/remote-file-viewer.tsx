// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { Copy, Loader2, RefreshCw, SquareTerminal } from "lucide-react";
import { toast } from "sonner";

import { AppButton as Button } from "@/shared/components/app-button";
import {
  CodeEditor,
  detectLanguageId,
  useEditorAffordances,
} from "@/shared/components/code-editor";
import { FileWarningPlaceholder } from "@/features/viewer/components/file-warning-placeholder";
import { looksBinary } from "@/features/viewer/lib/worktree-file-view";
import { useCopyToClipboard } from "@/shared/hooks/use-copy-to-clipboard";
import { basename, dirname } from "@/shared/lib/basename";
import { getErrorMessage } from "@/shared/lib/error-message";
import { shellEscape } from "@/shared/lib/shell-escape";
import { machineFsRead, openMachineTerminal } from "@/tauri/draft-commands";

import { machineBlockedReason, machineQualifiedPath } from "../lib/machine-status";
import {
  clockTime,
  MB,
  REMOTE_SIZE_GATE_BYTES,
  type MachineFileRef,
} from "../lib/machine-file-path";
import { readBuffer, rememberBuffer, type LoadedBuffer } from "../lib/remote-buffers";
import { useLatencyPhase } from "../lib/use-latency-phase";
import { useMachine } from "../stores/machines-store";
import { MachineIcon } from "./machine-status-label";

const noop = (): void => {};

type Load = { status: "loading" } | LoadedBuffer | { status: "error"; message: string };

/**
 * SW-RM-UI-1 — a remote doc tab: a file ON a machine, read through its terminal
 * server, shown READ-ONLY (v1). The header repeats the machine
 * (`devbox-01:/abs/path`), says Read-only, and offers the editing path — **Open in
 * terminal** (`$EDITOR <file>` in a new tab on that machine) — plus Copy path
 * (machine-qualified) / Copy path on machine. Over ~1 MB it gates to "Load first
 * 1 MB · Open in terminal (less)". Offline, the loaded buffer stays with "Last
 * loaded hh:mm, may be out of date"; nothing is persisted.
 */
export function RemoteFileViewer({ file }: { file: MachineFileRef }) {
  const { machineId, absPath } = file;
  const machine = useMachine(machineId);
  const [load, setLoad] = useState<Load>(
    () => readBuffer(machineId, absPath) ?? { status: "loading" },
  );
  const [reloadKey, setReloadKey] = useState(0);
  /** Only a reload the user asked for toasts on failure. */
  const [userReload, setUserReload] = useState(false);
  const [showGated, setShowGated] = useState(false);
  const phase = useLatencyPhase(load.status === "loading");
  const affordances = useEditorAffordances(absPath);
  const { handleCopy } = useCopyToClipboard();

  const name = basename(absPath);
  const label = machine?.label ?? machineId;
  const online = machine?.status.state === "connected";

  // Back online after showing a stale buffer → read it again.
  const [wasOnline, setWasOnline] = useState(online);
  if (wasOnline !== online) {
    setWasOnline(online);
    if (online) {
      setUserReload(false);
      setReloadKey((k) => k + 1);
    }
  }

  useEffect(() => {
    let stale = false;
    machineFsRead(machineId, absPath, REMOTE_SIZE_GATE_BYTES)
      .then((read) => {
        const loaded: LoadedBuffer = { status: "ready", read, at: Date.now() };
        rememberBuffer(machineId, absPath, loaded);
        if (!stale) setLoad(loaded);
      })
      .catch((e) => {
        if (stale) return;
        // A failed read over content already on screen keeps it (labeled stale
        // while the machine is offline) — and says so on a reload.
        const message = getErrorMessage(e);
        setLoad((prev) => (prev.status === "ready" ? prev : { status: "error", message }));
        if (userReload) toast.error(`Couldn't reload ${basename(absPath)}: ${message}`);
      });
    return () => {
      stale = true;
    };
  }, [machineId, absPath, reloadKey, userReload]);

  const openInTerminal = async (program: "editor" | "less") => {
    // An unset $EDITOR must never leave the shell running the file itself.
    const command = program === "editor" ? "${EDITOR:-vi}" : "less";
    try {
      await openMachineTerminal(machineId, dirname(absPath), `${command} ${shellEscape(absPath)}`);
      toast.success(`Opened ${name} in a new terminal on ${label}`);
    } catch (e) {
      toast.error(`Couldn't open a terminal on ${label}: ${getErrorMessage(e)}`);
    }
  };

  const blockedReason = machine ? machineBlockedReason(machine) : null;
  const ready = load.status === "ready" ? load : null;
  const binary = ready !== null && looksBinary(ready.read.content);
  const gated = ready !== null && !binary && ready.read.truncated && !showGated;

  return (
    <div
      data-testid="remote-file-viewer"
      className="flex h-full min-h-0 flex-col bg-background text-foreground"
    >
      <div className="flex flex-wrap items-center gap-2 border-b border-border px-3 py-1.5 text-xs">
        <span
          className="flex min-w-0 items-center gap-1"
          title={machineQualifiedPath({ label }, absPath)}
        >
          {machine && <MachineIcon machine={machine} size={12} />}
          <span className="shrink-0 font-medium">{label}</span>
          <span className="min-w-0 truncate font-mono text-muted-foreground">:{absPath}</span>
        </span>
        <span
          data-testid="remote-file-readonly"
          className="shrink-0 rounded border border-border px-1 text-[11px] text-muted-foreground"
        >
          Read-only
        </span>
        <div className="ml-auto flex shrink-0 items-center gap-1">
          <Button
            size="sm"
            variant="outline"
            data-testid="remote-file-open-in-terminal"
            disabled={!online}
            title={blockedReason ?? `Open ${name} with $EDITOR in a new terminal on ${label}`}
            onClick={() => void openInTerminal("editor")}
          >
            <SquareTerminal size={12} className="mr-1" aria-hidden="true" />
            Open in terminal
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => handleCopy(machineQualifiedPath({ label }, absPath))}
          >
            <Copy size={12} className="mr-1" aria-hidden="true" />
            Copy path
          </Button>
          <Button size="sm" variant="ghost" onClick={() => handleCopy(absPath)}>
            Copy path on machine
          </Button>
          <Button
            size="sm"
            variant="ghost"
            aria-label={`Reload ${name} from ${label}`}
            disabled={!online}
            onClick={() => {
              setUserReload(true);
              setReloadKey((k) => k + 1);
            }}
          >
            <RefreshCw size={12} aria-hidden="true" />
          </Button>
        </div>
      </div>

      {ready && !online && (
        <p
          data-testid="remote-file-stale"
          className="border-b border-border bg-amber-500/10 px-3 py-1 text-[11px] text-amber-700 dark:text-amber-300"
        >
          Last loaded {clockTime(ready.at)}, may be out of date — {label} isn't connected.
        </p>
      )}

      <div className="min-h-0 flex-1">
        {load.status === "loading" && phase !== "idle" && (
          <p className="flex items-center gap-1.5 p-3 text-xs text-muted-foreground">
            <Loader2 size={12} className="animate-spin" aria-hidden="true" />
            {phase === "slow" ? `Still loading from ${label}…` : "Loading…"}
          </p>
        )}
        {load.status === "error" && (
          <FileWarningPlaceholder
            message={`Couldn't read from ${label}: ${load.message}`}
            filename={name}
            testId="remote-file-error"
          />
        )}
        {binary && (
          <FileWarningPlaceholder
            message="Binary file — no preview"
            filename={name}
            testId="remote-file-binary"
          />
        )}
        {ready && gated && (
          <div
            data-testid="remote-file-size-gate"
            className="flex flex-col items-start gap-2 p-4 text-xs"
          >
            <p>
              {name} is {(ready.read.size / MB).toFixed(1)} MB on {label} — too big to load in full.
            </p>
            <div className="flex gap-2">
              <Button size="sm" variant="outline" onClick={() => setShowGated(true)}>
                Load first 1 MB
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={!online}
                title={blockedReason ?? undefined}
                onClick={() => void openInTerminal("less")}
              >
                Open in terminal (less)
              </Button>
            </div>
          </div>
        )}
        {ready && !binary && !gated && (
          <CodeEditor
            value={ready.read.content}
            onChange={noop}
            readOnly
            path={absPath}
            language={detectLanguageId(absPath, ready.read.content)}
            folding={affordances.folding}
            showWhitespace={affordances.showWhitespace}
            tabSize={affordances.tabSize}
            height="100%"
            ariaLabel={`Read-only view of ${name} on ${label}`}
          />
        )}
      </div>
    </div>
  );
}
