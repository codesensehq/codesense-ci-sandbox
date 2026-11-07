// SPDX-License-Identifier: MIT
import { useState, type ReactNode } from "react";
import { MoreHorizontal } from "lucide-react";

import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { PortaledSelect } from "@/components/ui/portaled-select";
import { resolveTeamTargetContentRoot } from "@/features/context-mobility/context-target-facts";
import { MoveSheet } from "@/features/context-mobility/move-sheet";
import { RemoveSheet } from "@/features/context-mobility/remove-sheet";
import { cn } from "@/lib/utils";
import { AppButton as Button } from "@/shared/components/app-button";
import type { Context } from "@/shared/lib/context-model";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import type { ContextMobilityOp, MoveGateKind } from "@/shared/lib/move-gate-ladder";
import { activeTeams } from "@/shared/lib/user-teams";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";
import { toast } from "sonner";

import {
  endpointForContextId,
  moveOutcomeToast,
  planMove,
} from "@/features/context-mobility/move-controller";
import { listScratchSpace, moveContentBetweenContexts } from "@/tauri/commands";
import {
  fileListErrorMessage,
  isNotIndexedError,
  requestContentFileIndex,
} from "@/shared/lib/pipeline-graph/content-scan-queue";
import { toastError } from "@/shared/lib/with-error-toast";

import {
  allOrgsTarget,
  itemFileName,
  otherOrgTarget,
  scratchSource,
  scratchVerbs,
  teamTarget,
  thisOrgTarget,
  type ScratchItem,
  type ScratchVerb,
  type ScratchVerbRow,
} from "../lib/scratch-item-actions";

/**
 * SLW-PT-22 — Share with ‹Team›…, Copy to another org…, Use in all orgs / Only this
 * org, and Remove for one scratch item. The verbs run through the same gate sheets
 * and backend as every other context move, so the steps a person sees here are the
 * ones an agent's move/copy/remove tools return. Mounted as a ⋯ menu on the Scratch
 * space tab rows and as the right-click menu on "From scratch" worktree files.
 */

type Stage =
  | { kind: "idle" }
  | { kind: "pickTeam" }
  /** `synced`: the orgs synced on this Mac (`null` while they load). */
  | { kind: "pickOrg"; synced: readonly string[] | null }
  | {
      kind: "gating";
      op: ContextMobilityOp;
      target: Context;
      occupied: ReadonlySet<string>;
      preconfirmed: readonly MoveGateKind[];
    }
  | { kind: "removing" };

/** The names already taken at a target — the collision facts the gate sheet needs.
 *  An unreadable target reads empty: the backend still refuses to overwrite. */
async function occupiedAt(target: Context, orgs: readonly Org[]): Promise<ReadonlySet<string>> {
  try {
    if (target.kind === "team-all") {
      const root = resolveTeamTargetContentRoot(target, orgs);
      if (!root) return new Set();
      try {
        return new Set((await requestContentFileIndex(root)).map((f) => f.relativePath));
      } catch (error) {
        // A root the index refuses is said out loud; the backend still refuses to overwrite.
        if (isNotIndexedError(error))
          toastError("Couldn't check for name clashes: ", new Error(fileListErrorMessage(error)));
        throw error;
      }
    }
    if (target.id === allOrgsTarget().id) {
      return new Set((await listScratchSpace(null)).allOrgs.map((f) => f.logical));
    }
    const orgSlug = target.id.slice(target.id.indexOf(":") + 1);
    return new Set((await listScratchSpace(orgSlug)).files.map((f) => f.logical));
  } catch {
    return new Set();
  }
}

export interface ScratchItemActions {
  rows: ScratchVerbRow[];
  start: (verb: ScratchVerb) => void;
  /** The sheets the verbs open; render once next to the trigger. */
  sheets: ReactNode;
}

export function useScratchItemActions(item: ScratchItem, onDone: () => void): ScratchItemActions {
  const orgs = useOrgStore((s) => s.orgs);
  const user = useCurrentUser();
  const userId = user?.github_user_id ?? null;
  const org = orgs.find((o) => o.slug === item.orgSlug) ?? null;
  const teams = org ? activeTeams(org) : [];
  const otherOrgs = orgs.filter((o) => o.slug !== item.orgSlug);
  const [stage, setStage] = useState<Stage>({ kind: "idle" });
  const close = () => setStage({ kind: "idle" });
  const source = scratchSource(item);
  const fileName = itemFileName(item.logical);

  /**
   * Run the op once the caller's own sheet has put every gate but a collision to
   * the user. The backend is asked first: it does the op, says the target already
   * holds this exact item (a retry — done, never a collision with itself), or
   * reports a real collision, and only then does the Keep both / Replace chooser
   * open. Anything else still left to confirm goes through the full sheet.
   */
  const gate = (
    op: ContextMobilityOp,
    target: Context,
    preconfirmed: readonly MoveGateKind[] = [],
  ) => {
    if (source === null || userId === null) return;
    const openSheet = (occupied: ReadonlySet<string>) =>
      setStage({ kind: "gating", op, target, occupied, preconfirmed });
    const unconfirmed = planMove(source.id, target.id, { targetCollision: false, op }).steps.filter(
      (step) => !preconfirmed.includes(step.kind),
    );
    const from = endpointForContextId(source.id);
    const to = endpointForContextId(target.id);
    if (unconfirmed.length > 0 || !from || !to) {
      void occupiedAt(target, orgs).then(openSheet);
      return;
    }
    close();
    void moveContentBetweenContexts({
      from,
      to,
      logicalName: item.logical,
      userId,
      retainSource: op === "copy",
    }).then(
      (outcome) => {
        if (outcome.kind === "rejectedTargetExists") {
          void occupiedAt(target, orgs).then((occupied) =>
            openSheet(new Set([...occupied, item.logical])),
          );
          return;
        }
        const t = moveOutcomeToast(outcome, { fileName, target });
        toast[t.kind](t.message);
        if (
          outcome.kind === "moved" ||
          outcome.kind === "copied" ||
          outcome.kind === "movedSourceKept"
        )
          onDone();
      },
      (e: unknown) => toast.error(e instanceof Error ? e.message : String(e)),
    );
  };

  const rows = scratchVerbs(item, { teamCount: teams.length, otherOrgCount: otherOrgs.length });

  const start = (verb: ScratchVerb) => {
    switch (verb) {
      case "share":
        setStage({ kind: "pickTeam" });
        return;
      case "copyToOrg":
        setStage({ kind: "pickOrg", synced: null });
        void listScratchSpace(null).then(
          (listing) => setStage({ kind: "pickOrg", synced: listing.syncedOrgs }),
          () => setStage({ kind: "pickOrg", synced: [] }),
        );
        return;
      case "useInAllOrgs":
        gate("move", allOrgsTarget());
        return;
      case "onlyThisOrg":
        if (item.orgSlug) {
          gate("move", thisOrgTarget(item.orgSlug, org?.display_name ?? item.orgSlug));
        }
        return;
      case "remove":
        setStage({ kind: "removing" });
    }
  };

  const sheets =
    source === null || userId === null ? null : (
      <>
        {stage.kind === "pickTeam" && item.orgSlug && (
          <ShareWithTeamSheet
            fileName={fileName}
            teams={teams.map((t) => t.name)}
            defaultTeam={item.defaultTeam}
            onCancel={close}
            onShare={(team) => gate("move", teamTarget(item.orgSlug ?? "", team), ["privacy"])}
          />
        )}
        {stage.kind === "pickOrg" && (
          <CopyToOrgSheet
            fileName={fileName}
            orgs={
              stage.synced === null ? null : otherOrgs.filter((o) => stage.synced?.includes(o.slug))
            }
            onCancel={close}
            onCopy={(target) => gate("copy", otherOrgTarget(target.slug, target.display_name))}
          />
        )}
        {stage.kind === "gating" && (
          <MoveSheet
            open
            op={stage.op}
            logicalName={item.logical}
            fileName={fileName}
            source={source}
            target={stage.target}
            userId={userId}
            targetCollision={stage.occupied.has(item.logical)}
            targetHas={(candidate) => stage.occupied.has(candidate)}
            preconfirmed={stage.preconfirmed}
            onMoved={onDone}
            onClose={close}
          />
        )}
        {stage.kind === "removing" && (
          <RemoveSheet
            open
            logicalName={item.logical}
            fileName={fileName}
            source={source}
            userId={userId}
            onRemoved={onDone}
            onClose={close}
          />
        )}
      </>
    );

  return { rows, start, sheets };
}

/** One verb in a menu: its label, and — when it can't run — why, right under it. */
function VerbLabel({ row }: { row: ScratchVerbRow }) {
  return (
    <span className="flex min-w-0 flex-col">
      <span>{row.label}</span>
      {row.disabledReason && (
        <span className="text-xs text-muted-foreground">{row.disabledReason}</span>
      )}
    </span>
  );
}

/** The ⋯ menu at the end of a Private tab row. */
export function ScratchItemMenu({ item, onDone }: { item: ScratchItem; onDone: () => void }) {
  const { rows, start, sheets } = useScratchItemActions(item, onDone);
  const fileName = itemFileName(item.logical);
  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            data-testid="scratch-item-menu"
            aria-label={`Actions for ${fileName}`}
            className="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground opacity-60 transition-opacity hover:bg-muted hover:text-foreground hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring group-hover/row:opacity-100 aria-expanded:bg-muted aria-expanded:opacity-100"
          >
            <MoreHorizontal size={14} aria-hidden="true" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-56">
          {rows.map((row) => (
            <DropdownMenuItem
              key={row.verb}
              data-testid={`scratch-verb-${row.verb}`}
              disabled={row.disabledReason !== undefined}
              onSelect={() => start(row.verb)}
              className={cn(
                row.destructive &&
                  "text-destructive focus:bg-destructive/10 focus:text-destructive",
              )}
            >
              <VerbLabel row={row} />
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
      {sheets}
    </>
  );
}

/** The right-click menu over a "From scratch" file elsewhere (the worktree tree). */
export function ScratchItemContextMenu({
  item,
  onDone,
  children,
}: {
  item: ScratchItem;
  onDone: () => void;
  children: ReactNode;
}) {
  const { rows, start, sheets } = useScratchItemActions(item, onDone);
  return (
    <>
      <ContextMenu>
        <ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
        <ContextMenuContent className="min-w-56">
          {rows.map((row) => (
            <ContextMenuItem
              key={row.verb}
              data-testid={`scratch-verb-${row.verb}`}
              disabled={row.disabledReason !== undefined}
              variant={row.destructive ? "destructive" : "default"}
              onSelect={() => start(row.verb)}
            >
              <VerbLabel row={row} />
            </ContextMenuItem>
          ))}
        </ContextMenuContent>
      </ContextMenu>
      {sheets}
    </>
  );
}

function ShareWithTeamSheet({
  fileName,
  teams,
  defaultTeam,
  onCancel,
  onShare,
}: {
  fileName: string;
  teams: string[];
  defaultTeam: string | null;
  onCancel: () => void;
  onShare: (team: string) => void;
}) {
  const [team, setTeam] = useState(
    defaultTeam && teams.includes(defaultTeam) ? defaultTeam : (teams[0] ?? ""),
  );
  return (
    <Dialog open onOpenChange={(open) => (!open ? onCancel() : undefined)}>
      <DialogContent data-testid="scratch-share-sheet">
        <DialogHeader>
          <DialogTitle>Share &lsquo;{fileName}&rsquo;</DialogTitle>
          <DialogDescription>
            Everyone on {team || "the team"} will see it, and it leaves Private. From then on
            it&apos;s the team&apos;s file: anyone on the team can change or remove it.
          </DialogDescription>
        </DialogHeader>
        <label className="flex flex-col gap-1.5 text-sm">
          <span className="font-medium">With</span>
          <PortaledSelect
            ariaLabel="Team"
            value={team}
            onValueChange={setTeam}
            options={teams.map((t) => ({ value: t, label: t }))}
            triggerClassName="h-8 text-sm"
          />
        </label>
        <p className="text-xs text-muted-foreground">
          A Markdown file notes who shared it, from where, and when.
        </p>
        <DialogFooter>
          <Button variant="outline" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            data-testid="scratch-share-confirm"
            disabled={!team}
            onClick={() => onShare(team)}
          >
            Share
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function CopyToOrgSheet({
  fileName,
  orgs,
  onCancel,
  onCopy,
}: {
  fileName: string;
  /** The other orgs synced on this Mac; `null` while they load. */
  orgs: readonly Org[] | null;
  onCancel: () => void;
  onCopy: (org: Org) => void;
}) {
  const [picked, setSlug] = useState<string | null>(null);
  const slug = picked ?? orgs?.[0]?.slug ?? "";
  const target = orgs?.find((o) => o.slug === slug) ?? null;
  return (
    <Dialog open onOpenChange={(open) => (!open ? onCancel() : undefined)}>
      <DialogContent data-testid="scratch-copy-sheet">
        <DialogHeader>
          <DialogTitle>Copy &lsquo;{fileName}&rsquo; to another org</DialogTitle>
          <DialogDescription>
            A copy goes in Private for {target?.display_name ?? "that org"} and applies only there.
            The original stays here.
          </DialogDescription>
        </DialogHeader>
        {orgs === null ? (
          <p className="text-sm text-muted-foreground">Finding the orgs on this Mac…</p>
        ) : orgs.length === 0 ? (
          <p data-testid="scratch-copy-none" className="text-sm text-muted-foreground">
            No other org is synced on this Mac yet. Sync one, then copy it there.
          </p>
        ) : (
          <label className="flex flex-col gap-1.5 text-sm">
            <span className="font-medium">To</span>
            <PortaledSelect
              ariaLabel="Org"
              value={slug}
              onValueChange={setSlug}
              options={orgs.map((o) => ({ value: o.slug, label: o.display_name || o.slug }))}
              triggerClassName="h-8 text-sm"
            />
          </label>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onCancel}>
            Cancel
          </Button>
          <Button
            data-testid="scratch-copy-confirm"
            disabled={target === null}
            onClick={() => target && onCopy(target)}
          >
            Copy
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
