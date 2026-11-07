// SPDX-License-Identifier: MIT
import { InfoTip } from "@/components/ui/tooltip";
import { readyTitle, type BlockedDep } from "@/shared/lib/pipeline-graph/resolve-nav-readiness";
import { normalizeStatus } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { STAGE_GROUPS } from "@/shared/lib/nav-lens";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { NAV_TIP_CLASS } from "./nav-agent-mark";

/**
 * Whether a not-started story can start, as a card on its rail: ready (and how many
 * dependencies that rests on), or each dependency it waits on with that story's stage.
 * Anything else (a story underway, or a waiting one with nothing named) is the rail alone.
 */
export function ReadinessTip({
  ready,
  dependencyCount,
  blockedBy,
  children,
}: {
  ready: boolean;
  dependencyCount: number;
  blockedBy: readonly BlockedDep[] | undefined;
  children: React.ReactNode;
}) {
  const statesById = useStatusTaxonomyStore((s) => s.byId);
  const waiting = !ready && blockedBy && blockedBy.length > 0;
  if (!ready && !waiting) return <>{children}</>;
  // A dependency's stage in the nav's own words (the menu's and the stage headers'), by role.
  const stageOf = (dep: BlockedDep) => {
    if (dep.reason === "unresolved") return "not found";
    const role = statesById.get(normalizeStatus(dep.status))?.role;
    return STAGE_GROUPS.find((g) => g.key === role)?.label ?? dep.status;
  };
  const [readyHead, readyDetail] = readyTitle(dependencyCount).split(" · ");
  const tip = ready ? (
    <span>
      <b className="font-semibold">{readyHead}</b>{" "}
      <span className="text-muted-foreground">· {readyDetail}</span>
    </span>
  ) : (
    <>
      <b className="mb-0.5 font-semibold">Waiting on</b>
      {blockedBy!.map((dep) => (
        <span key={dep.ref} className="flex min-w-0 gap-1.5">
          <span className="truncate">{dep.ref}</span>
          <span className="flex-none text-muted-foreground">· {stageOf(dep)}</span>
        </span>
      ))}
    </>
  );
  return (
    <InfoTip
      label={tip}
      side="bottom"
      align="start"
      collisionPadding={8}
      contentClassName={NAV_TIP_CLASS}
    >
      <span className="inline-flex">{children}</span>
    </InfoTip>
  );
}
