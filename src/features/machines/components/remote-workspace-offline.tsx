// SPDX-License-Identifier: MIT
import { useState } from "react";
import { Loader2, Server } from "lucide-react";

import { machineOfflineWord } from "@/features/machines/components/machine-location-badge";
import { connectRemoteWorkspace } from "@/features/machines/lib/remote-story-workspace";
import { useMachine } from "@/features/machines/stores/machines-store";
import { getErrorMessage } from "@/shared/lib/error-message";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { selectWorkspaceTabs, useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

/**
 * A workspace whose machine is not connected and that has no terminal to show:
 * it says so ("offline since…") and offers Connect, then Retry. It never opens
 * a local terminal in the machine's stead.
 */
export function RemoteWorkspaceOffline({ teamKey, ws }: { teamKey: string; ws: Workspace }) {
  const remote = isRemoteWorkspace(ws) && ws.status === "ready" ? ws.location : null;
  const machine = useMachine(remote?.machineId ?? "");
  const hasTabs = useViewerStore((s) => selectWorkspaceTabs(s, teamKey, ws.id).length > 0);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!remote) return null;
  const offline = machineOfflineWord(machine);
  if ((!offline && !error) || hasTabs) return null;
  const label = machine?.label ?? remote.machineId;
  const connect = () => {
    setConnecting(true);
    setError(null);
    connectRemoteWorkspace(teamKey, ws.id, remote)
      .catch((e: unknown) => setError(getErrorMessage(e)))
      .finally(() => setConnecting(false));
  };
  return (
    <div
      data-testid={`remote-workspace-offline-${ws.id}`}
      className="absolute inset-0 z-20 flex items-center justify-center bg-[#0d1117]/80 p-4"
    >
      <div className="max-w-[22rem] rounded-lg border border-[#30363d] bg-[#161b22] p-4 text-sm text-[#e6edf3]">
        <div className="flex items-center gap-2">
          <Server className="size-4 text-[#8b949e]" aria-hidden />
          <span className="font-semibold">{label}</span>
          {offline && <span className="text-xs text-[#d29922]">{offline}</span>}
        </div>
        <p className="mt-2 text-xs text-[#8b949e]">
          This workspace lives on {label}. Connect to open its terminals there.
        </p>
        {error && (
          <p role="alert" className="mt-2 text-xs text-[#ffa198]">
            {error}
          </p>
        )}
        <button
          type="button"
          data-testid={`remote-workspace-connect-${ws.id}`}
          disabled={connecting}
          onClick={connect}
          className="mt-3 inline-flex items-center gap-1.5 rounded bg-[#238636] px-3 py-1.5 text-xs disabled:opacity-60"
        >
          {connecting && <Loader2 className="size-3 animate-spin" aria-hidden />}
          {connecting ? "Connecting…" : error ? "Retry" : "Connect"}
        </button>
      </div>
    </div>
  );
}
