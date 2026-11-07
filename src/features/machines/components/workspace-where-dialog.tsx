// SPDX-License-Identifier: MIT
import { useEffect, useRef, useState } from "react";
import { Monitor, Server } from "lucide-react";

import { machineColor } from "@/features/machines/lib/machine-color";
import {
  PRIVATE_ACCOUNT_FOR_TABS,
  describeMachineStatus,
  needsPrivateAccount,
} from "@/features/machines/lib/machine-status";
import { useWorkspaceWhere } from "@/features/machines/lib/remote-story-workspace";
import { useMachine } from "@/features/machines/stores/machines-store";
import type { MachineSummary } from "@/tauri/commands";

/**
 * Where a new story workspace lives: This Mac, or one of the team's machines.
 * Asked once, when the workspace is created, and only when the team has a
 * machine; the answer is the team's next default. A machine that is not
 * connected is connected when chosen.
 */
export function WorkspaceWhereDialog() {
  const request = useWorkspaceWhere((s) => s.request);
  const attention = useWorkspaceWhere((s) => s.attention);
  const dialogRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (attention > 0)
      dialogRef.current?.querySelector<HTMLButtonElement>("button[data-where]")?.focus();
  }, [attention]);
  useEffect(() => {
    if (!request) return;
    const remembered = dialogRef.current?.querySelector<HTMLButtonElement>(
      `button[data-where="${CSS.escape(request.initial)}"]:not([disabled])`,
    );
    (
      remembered ?? dialogRef.current?.querySelector<HTMLButtonElement>("button[data-where]")
    )?.focus();
  }, [request]);
  if (!request) return null;
  const cancel = () => request.answer(null);
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-[#0d1117]/70 p-4"
      onPointerDown={(e) => {
        if (e.target === e.currentTarget) cancel();
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-label="Where should this workspace live?"
        data-testid="workspace-where-dialog"
        className="w-full max-w-[24rem] rounded-xl border border-[#30363d] bg-[#161b22] p-4 text-sm text-[#e6edf3] shadow-[0_16px_48px_rgba(1,4,9,0.6)]"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            cancel();
          }
        }}
      >
        <h2 className="text-[15px] font-semibold">Where should this workspace live?</h2>
        <p className="mt-1 truncate text-xs text-[#8b949e]" title={request.label}>
          {request.label}
        </p>
        <ul role="list" aria-label="Where" className="mt-3 flex flex-col gap-1">
          <li>
            <button
              type="button"
              data-where="local"
              data-testid="workspace-where-local"
              onClick={() => request.answer({ kind: "local" })}
              className={ROW}
            >
              <Monitor className="size-4 text-[#8b949e]" aria-hidden />
              <span className="flex-1 text-left">This Mac</span>
            </button>
          </li>
          {request.machines.map((machine) => (
            <WhereMachine
              key={machine.machineId}
              machine={machine}
              onChoose={() => request.answer({ kind: "machine", machineId: machine.machineId })}
            />
          ))}
        </ul>
        <p className="mt-3 text-[11px] text-[#8b949e]">
          A workspace stays where it is created. Its terminals and agents run there.
        </p>
        <div className="mt-3 flex justify-end">
          <button
            type="button"
            className="text-xs text-[#8b949e] hover:text-[#e6edf3]"
            onClick={cancel}
          >
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}

function WhereMachine({
  machine: initial,
  onChoose,
}: {
  machine: MachineSummary;
  onChoose: () => void;
}) {
  const machine = useMachine(initial.machineId) ?? initial;
  const [shownAt] = useState(() => Date.now());
  if (needsPrivateAccount(machine))
    return (
      <li className="px-2 py-1 text-xs text-[#8b949e]">
        {machine.label}: {PRIVATE_ACCOUNT_FOR_TABS}.
      </li>
    );
  const view = describeMachineStatus(machine, shownAt);
  const connected = machine.status.state === "connected";
  const canConnect = view.actions.some((a) => a.id === "connect" || a.id === "retry");
  return (
    <li>
      <button
        type="button"
        data-where={machine.machineId}
        data-testid={`workspace-where-${machine.machineId}`}
        disabled={!connected && !canConnect}
        title={view.blockedReason ?? undefined}
        onClick={onChoose}
        className={ROW}
      >
        <Server className="size-4 text-[#8b949e]" aria-hidden />
        <span
          aria-hidden
          className="size-1.5 flex-shrink-0 rounded-full"
          style={{ backgroundColor: machineColor(machine.machineId) }}
        />
        <span className="flex-1 truncate text-left">{machine.label}</span>
        {!connected && (
          <span className="text-xs text-[#8b949e]">
            {canConnect ? "Connects first" : view.word}
          </span>
        )}
      </button>
    </li>
  );
}

const ROW =
  "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-[13px] hover:bg-[#21262d] focus-visible:bg-[#21262d] focus-visible:outline focus-visible:outline-1 focus-visible:outline-[#388bfd] disabled:cursor-not-allowed disabled:opacity-50";
