// SPDX-License-Identifier: MIT
import { ShieldAlert } from "lucide-react";

import { AppButton as Button } from "@/shared/components/app-button";
import { ReservePanel } from "@/features/shell/components/connections-reserve";
import { useCopyToClipboard } from "@/shared/hooks/use-copy-to-clipboard";
import type { MachinesPanel } from "@/shared/stores/ui-store";
import type { MachineSummary } from "@/tauri/commands";

import { useTeamMachines } from "../stores/machines-store";
import { AddMachinePanel } from "./add-machine-panel";
import { QuickAddMachine } from "./quick-add-machine";

/**
 * SW-RM-UI-1 — the Connections reserve for machines: Add machine, Set up (Set up /
 * Reconnect / Edit address / Install, prefilled), or Review key. Resolves the
 * machine from the team registry; a machine that disappeared says so, with Close.
 */
export function MachineReserve({
  panel,
  teamKey,
  onClose,
}: {
  panel: MachinesPanel;
  teamKey: string;
  onClose: () => void;
}) {
  const machines = useTeamMachines(teamKey);
  if (panel.mode === "add") return <QuickAddMachine teamKey={teamKey} onClose={onClose} />;
  if (!machines) return null;
  const machine = machines.find((m) => m.machineId === panel.machineId);
  if (!machine) {
    return (
      <ReservePanel
        testId="machine-reserve-gone"
        icon={<ShieldAlert size={16} aria-hidden="true" />}
        title="Machine not found"
        subtitle="It was removed, or it isn't shared with this team."
        closeLabel="Close"
        onClose={onClose}
      />
    );
  }
  return panel.mode === "reviewKey" ? (
    <HostKeyReviewPanel machine={machine} onClose={onClose} />
  ) : (
    <AddMachinePanel
      teamKey={teamKey}
      machine={machine}
      initialMode={panel.mode === "setUp" && panel.privateAccount ? "private-account" : undefined}
      onClose={onClose}
    />
  );
}

/**
 * "Review key…" for a machine whose host key changed. Deliberately NO accept
 * button: a changed key is either a rebuilt machine (its publisher removes it
 * and adds it again, trusting the new key — v1 has no in-place re-pin) or an
 * attack. CodeSense stays blocked until the key matches.
 */
function HostKeyReviewPanel({
  machine,
  onClose,
}: {
  machine: MachineSummary;
  onClose: () => void;
}) {
  const { handleCopy } = useCopyToClipboard();
  if (machine.status.state !== "hostKeyChanged") {
    return (
      <ReservePanel
        testId="machine-review-key-panel"
        icon={<ShieldAlert size={16} aria-hidden="true" />}
        title={`${machine.label}'s host key`}
        subtitle="No host-key change to review."
        closeLabel="Close key review"
        onClose={onClose}
      />
    );
  }
  const { pinnedFp, presentedFp } = machine.status;
  const details = `${machine.label} host key changed\npinned:    ${pinnedFp}\npresented: ${presentedFp}`;
  return (
    <ReservePanel
      testId="machine-review-key-panel"
      icon={<ShieldAlert size={16} aria-hidden="true" />}
      title={`Review ${machine.label}'s host key`}
      subtitle="Blocked — CodeSense won't connect until the key matches."
      closeLabel="Close key review"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" size="sm" onClick={() => handleCopy(details)}>
            Copy details
          </Button>
          <Button size="sm" onClick={onClose}>
            Close
          </Button>
        </>
      }
    >
      <dl className="mt-4 grid grid-cols-[6rem_1fr] gap-x-2 gap-y-1 text-xs">
        <dt className="text-muted-foreground">Pinned</dt>
        <dd>
          <code data-testid="review-key-pinned">{pinnedFp}</code>
        </dd>
        <dt className="text-muted-foreground">Presented</dt>
        <dd>
          <code data-testid="review-key-presented">{presentedFp}</code>
        </dd>
      </dl>
      <p className="mt-4 text-xs text-muted-foreground">
        If {machine.label} was rebuilt, whoever manages it removes it and adds it again, trusting
        the new key. If not, someone may be intercepting the connection — don't connect, and tell
        your team.
      </p>
    </ReservePanel>
  );
}
