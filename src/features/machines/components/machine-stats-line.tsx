// SPDX-License-Identifier: MIT
import type { MachineStats, MachineSummary } from "@/tauri/commands";

const gb = (bytes: number) => (bytes / 1e9).toLocaleString(undefined, { maximumFractionDigits: 1 });

export function MachineStatsLine({
  machine,
}: {
  machine: Pick<MachineSummary, "label" | "status"> & MachineStats;
}) {
  if (machine.status.state !== "connected") return null;
  const className = "text-[11px] text-muted-foreground tabular-nums";
  if (machine.machineStatsSupported === false)
    return <p className={className}>Load, memory and disk unknown · update the terminal server</p>;
  const {
    load1,
    cpuCount,
    memUsedBytes,
    memTotalBytes,
    diskFreeBytes,
    diskTotalBytes,
    diskPath,
    sampledAt,
  } = machine;
  if (
    load1 === undefined ||
    !cpuCount ||
    memUsedBytes === undefined ||
    memTotalBytes === undefined ||
    diskFreeBytes === undefined ||
    diskTotalBytes === undefined ||
    !diskPath ||
    sampledAt === undefined
  )
    return null;
  const time = new Date(sampledAt).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  return (
    <p className={className} title={`Reported by ${machine.label} at ${time} (${diskPath})`}>
      Load {Math.round((load1 / cpuCount) * 100)}% of {cpuCount} cores · Memory {gb(memUsedBytes)} /{" "}
      {gb(memTotalBytes)} GB · {gb(diskFreeBytes)} GB free of {gb(diskTotalBytes)} GB · reported{" "}
      {time}
    </p>
  );
}
