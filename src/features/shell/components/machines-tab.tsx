// SPDX-License-Identifier: MIT
import { MachinesGroup } from "@/features/machines/components/machines-group";
import { MachineReserve } from "@/features/machines/components/machine-reserve";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { isConsoleLayout } from "@/features/viewer/lib/terminal-column";

export function MachinesTab({ hostTeamKey }: { hostTeamKey?: string } = {}) {
  const selected = useViewerStore(
    (s) =>
      !isConsoleLayout(s, hostTeamKey !== undefined) &&
      (!hostTeamKey || s.activeTab[hostTeamKey] === "virtual://machines"),
  );
  const machinesPanel = useUiStore((s) => s.machinesPanel);
  const setMachinesPanel = useUiStore((s) => s.setMachinesPanel);
  const teamKey = useActiveTeamKey();
  const visible = selected && (!hostTeamKey || hostTeamKey === teamKey);
  return (
    <div
      data-testid="machines-tab"
      className="flex h-full overflow-auto bg-background text-foreground"
    >
      <section className="flex-1 space-y-5 p-6">
        <h1 className="text-lg font-semibold">Machines</h1>
        <p className="text-sm text-muted-foreground">Manage machines, SSH access, and trust.</p>
        <MachinesGroup teamKey={teamKey} visible={visible} />
      </section>
      {machinesPanel && (
        <MachineReserve
          key={`${teamKey}:${JSON.stringify(machinesPanel)}`}
          panel={machinesPanel}
          teamKey={teamKey}
          onClose={() => setMachinesPanel(null)}
        />
      )}
    </div>
  );
}
