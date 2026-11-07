// SPDX-License-Identifier: MIT
import { offerTerminalServerUpdate } from "@/features/machines/lib/machine-actions";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { selectTabBySessionId, useViewerStore } from "@/shared/stores/viewer-store";

/** The machine whose out-of-date terminal server keeps `sessionId`'s agent
 *  status from being read, or null (a local session, or one that reports). */
export function useRemoteServerUpdate(
  sessionId: string,
): { machineId: string; label: string } | null {
  const machineId = useViewerStore((s) => selectTabBySessionId(s, sessionId)?.origin?.machineId);
  const outdated = useAgentStateStore(
    (s) => s.activityBySession[sessionId]?.freshness === "upgrade-required",
  );
  const label = useMachinesStore((s) =>
    machineId
      ? Object.values(s.byTeam)
          .flat()
          .find((m) => m.machineId === machineId)?.label
      : undefined,
  );
  if (!machineId || !outdated) return null;
  return { machineId, label: label ?? "this machine" };
}

/** One click to the machine's "Update terminal server": the same action its
 *  Machines entry offers. Renders nothing while the session's status reads. */
export function RemoteServerUpdateAction({
  sessionId,
  className,
}: {
  sessionId: string;
  className?: string;
}) {
  const update = useRemoteServerUpdate(sessionId);
  if (!update) return null;
  return (
    <button
      type="button"
      data-testid="remote-server-update"
      className={className ?? "text-[#58a6ff] hover:underline"}
      onClick={() => offerTerminalServerUpdate(update.machineId)}
    >
      Update terminal server on {update.label}
    </button>
  );
}
