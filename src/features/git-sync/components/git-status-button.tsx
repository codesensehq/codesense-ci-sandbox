// SPDX-License-Identifier: MIT
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAgentsStore } from "@/features/agents/stores/agents-store";
import { useRef, useState } from "react";
import { ChevronDown, Lock } from "lucide-react";
import { toast } from "sonner";

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import type {
  ContextConflict,
  ContextConflictResolution,
  ContextRepoStatus,
  ContextSyncAction,
} from "@/tauri/commands";

import {
  agoText,
  conflictSideLine,
  elideMiddle,
  statusButtonModel,
  type StatusButtonModel,
} from "../lib/context-repo-status";
import {
  handWorktreeChanges,
  handConflict,
  runSyncAction,
  settleConflict,
} from "../lib/repo-status-source";

// ---------------------------------------------------------------------------
// Glyphs: one stroke family, drawn at the button's 10–11px.
// ---------------------------------------------------------------------------

function BranchGlyph() {
  return (
    <svg width="11" height="11" viewBox="0 0 16 16" fill="none" aria-hidden>
      <g stroke="currentColor" strokeWidth="1.6" strokeLinecap="round">
        <circle cx="4.5" cy="3.5" r="1.75" />
        <circle cx="4.5" cy="12.5" r="1.75" />
        <circle cx="11.5" cy="5.5" r="1.75" />
        <path d="M4.5 5.25v5.5M11.5 7.25c0 2.6-3 2.8-6.6 4.3" />
      </g>
    </svg>
  );
}

function UpGlyph() {
  return (
    <svg width="9" height="10" viewBox="0 0 9 10" aria-hidden>
      <path
        d="M4.5 9V1.6M1.4 4.6 4.5 1.5l3.1 3.1"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function DownGlyph() {
  return (
    <svg width="9" height="10" viewBox="0 0 9 10" aria-hidden>
      <path
        d="M4.5 1v7.4M1.4 5.4 4.5 8.5l3.1-3.1"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** Two lines that should meet and don't: the conflict mark. */
export function ConflictGlyph() {
  return (
    <svg width="10" height="10" viewBox="0 0 12 12" aria-hidden>
      <path
        d="M3 1.5v4.2M9 6.3v4.2M3 5.7h6"
        stroke="currentColor"
        strokeWidth="1.6"
        fill="none"
        strokeLinecap="round"
      />
    </svg>
  );
}

function FailedGlyph() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
      <circle cx="5" cy="5" r="4.2" fill="none" stroke="currentColor" strokeWidth="1.3" />
      <path d="M5 2.8v2.6" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
      <circle cx="5" cy="7.3" r=".75" fill="currentColor" />
    </svg>
  );
}

function OfflineGlyph() {
  return (
    <svg width="11" height="10" viewBox="0 0 11 10" aria-hidden>
      <path
        d="M3 8.5h5.2a2.3 2.3 0 0 0 .3-4.6A3.2 3.2 0 0 0 2.4 4.6 1.95 1.95 0 0 0 3 8.5zM1.5 1l8 8"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.25"
        strokeLinecap="round"
      />
    </svg>
  );
}

function KeyGlyph() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
      <circle cx="3.2" cy="6.8" r="2.2" fill="none" stroke="currentColor" strokeWidth="1.3" />
      <path
        d="M4.8 5.2 9 1M7.4 2.6l1.2 1.2"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.3"
        strokeLinecap="round"
      />
    </svg>
  );
}

/** A quiet ring that turns: the sync engine is on it. */
function Spinner() {
  return (
    <span
      aria-hidden
      className="inline-block size-2.5 flex-none animate-spin rounded-full border-[1.5px] border-[color-mix(in_srgb,var(--foreground)_25%,transparent)] border-t-foreground motion-reduce:animate-none"
    />
  );
}

// ---------------------------------------------------------------------------
// The button
// ---------------------------------------------------------------------------

/** The branch half: plain on main, a filled chip off it, dashed when detached. A long name
 *  keeps its start and its end; the narrow nav takes the shorter cut. */
function BranchPart({ model }: { model: StatusButtonModel }) {
  const { look, branchLabel } = model;
  const long = look === "detached" ? `detached ${branchLabel}` : elideMiddle(branchLabel, 18);
  const short = elideMiddle(branchLabel, 9);
  return (
    <span
      data-testid="git-status-branch"
      data-look={look}
      className={cn(
        "inline-flex min-w-0 items-center gap-[3px] whitespace-nowrap",
        look === "main" && "font-[450] text-muted-foreground",
        look === "branch" &&
          "-ml-[3px] h-[17px] rounded bg-foreground pr-[5px] pl-1 font-[550] text-[var(--cs-sidebar-bg,var(--background))]",
        look === "detached" &&
          "-ml-[3px] h-[17px] rounded border border-dashed border-foreground pr-[5px] pl-1 text-foreground",
      )}
    >
      <BranchGlyph />
      <span className="@max-[16rem]:hidden">{long}</span>
      <span className="hidden @max-[16rem]:inline">{short}</span>
    </span>
  );
}

function StatePart({ model, status }: { model: StatusButtonModel; status: ContextRepoStatus }) {
  const word = (w: string) => <span className="@max-[12rem]:hidden">{w}</span>;
  switch (model.state) {
    case "syncing":
      return (
        <span className="inline-flex items-center text-muted-foreground">
          <Spinner />
        </span>
      );
    case "retrying":
      return (
        <span className="inline-flex items-center gap-1 text-muted-foreground">
          <Spinner />
          {status.sync.attempt && status.sync.maxAttempts
            ? `${status.sync.attempt}/${status.sync.maxAttempts}`
            : null}
        </span>
      );
    case "failed":
      return (
        <span className="inline-flex items-center gap-1 font-semibold text-foreground">
          <FailedGlyph />
          {word("failed")}
        </span>
      );
    case "offline":
      return (
        <span className="inline-flex items-center gap-1 text-muted-foreground">
          <OfflineGlyph />
          {word("offline")}
        </span>
      );
    case "signin":
      return (
        <span className="inline-flex items-center gap-1 font-semibold text-foreground">
          <KeyGlyph />
          {word("sign in")}
        </span>
      );
    default:
      return null;
  }
}

export interface GitStatusButtonProps {
  /** The team whose agents the card asks. */
  teamKey: string;
  status: ContextRepoStatus;
  /** Changed files in scope that GitHub doesn't have. */
  uncommitted: number;
  orgRoot: string;
  /** Show the Changes view (a click when there is nothing to decide). */
  onOpenChanges: () => void;
  /** Open a conflicted file's changes (Compare). */
  onCompare: (relPath: string) => void;
  /** A card action went through: read the status again. */
  onSettled: () => void;
  /** The team's agents, for Hand to ‹agent›. */
  agentNames: string[];
}

/**
 * The scope row's one status control: `⎇ branch | ↑N ↓M ⟂C`. Branch and state in one
 * bordered button, one tooltip, one focus ring. A click opens Changes, or the card when there
 * is a decision (a conflict) or a way out of a failure (Retry, Sign in again, Set up).
 */
export function GitStatusButton({
  teamKey,
  status,
  uncommitted,
  orgRoot,
  onOpenChanges,
  onCompare,
  onSettled,
  agentNames,
}: GitStatusButtonProps) {
  const [now, setNow] = useState(() => Date.now());
  const [cardWanted, setCardWanted] = useState(false);
  // The tooltip's "last synced 3m ago" stays honest without a ticking render loop: it is
  // re-read whenever the pointer arrives.
  const model = statusButtonModel(status, uncommitted, now);
  // The card closes by itself once nothing on it is left to decide, and stays closed: the
  // next conflict or failure waits for a click rather than popping up over the user's work.
  if (cardWanted && !model.opensCard) setCardWanted(false);
  const open = cardWanted && model.opensCard;
  const setOpen = setCardWanted;
  const anchorRef = useRef<HTMLButtonElement>(null);

  const counts = (
    <>
      {model.upIsNew ? (
        <span className="inline-flex items-center gap-0.5 font-semibold">
          <UpGlyph />
          new
        </span>
      ) : model.up > 0 ? (
        <span className="inline-flex items-center gap-0.5 font-semibold" data-testid="git-up">
          <UpGlyph />
          {model.up}
        </span>
      ) : null}
      {model.down > 0 && (
        <span className="inline-flex items-center gap-0.5 font-semibold" data-testid="git-down">
          <DownGlyph />
          {model.down}
        </span>
      )}
      {model.conflicts > 0 && (
        <span
          className="inline-flex items-center gap-0.5 font-semibold text-[var(--cs-git-conflict)]"
          data-testid="git-conflicts"
        >
          <ConflictGlyph />
          {model.conflicts}
        </span>
      )}
    </>
  );
  const anyCount = model.upIsNew || model.up > 0 || model.down > 0 || model.conflicts > 0;
  const state = <StatePart model={model} status={status} />;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverAnchor asChild>
        <button
          ref={anchorRef}
          type="button"
          data-testid="git-status-button"
          data-state-kind={model.state}
          aria-label={model.sentence}
          aria-haspopup={model.opensCard ? "dialog" : undefined}
          aria-expanded={model.opensCard ? open : undefined}
          title={model.sentence}
          onPointerEnter={() => setNow(Date.now())}
          onFocus={() => setNow(Date.now())}
          onClick={() => {
            if (model.opensCard) setOpen((o) => !o);
            else onOpenChanges();
          }}
          className={cn(
            "-mr-0.5 inline-flex h-[22px] max-w-full min-w-0 flex-none items-center gap-1.5 whitespace-nowrap rounded-md border pr-[7px] pl-1.5 text-[11.5px] tabular-nums text-foreground",
            "border-[color-mix(in_srgb,var(--foreground)_14%,transparent)] transition-colors hover:bg-[color-mix(in_srgb,var(--foreground)_7%,transparent)]",
            "focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring",
            open && "bg-[color-mix(in_srgb,var(--foreground)_10%,transparent)]",
          )}
        >
          <BranchPart model={model} />
          {/* Detached, nothing syncs until an agent puts HEAD back on a branch: no
              "synced" word to claim otherwise, only what there is to count. */}
          {(anyCount || model.state !== "ok" || model.look !== "detached") && (
            <span
              aria-hidden
              className="h-3 w-px flex-none bg-[color-mix(in_srgb,var(--foreground)_18%,transparent)]"
            />
          )}
          {anyCount || model.state !== "ok" ? (
            <span className="inline-flex items-center gap-1.5">
              {counts}
              {state}
            </span>
          ) : model.look === "detached" ? null : (
            <span className="text-muted-foreground">
              {status.worktree ? "even with main" : "synced"}
            </span>
          )}
        </button>
      </PopoverAnchor>
      {model.opensCard && (
        <PopoverContent
          side="right"
          align="start"
          sideOffset={10}
          className="w-[264px] gap-0 rounded-md p-0 text-[11.5px] leading-[1.45] shadow-[0_6px_18px_rgba(0,0,0,.2)] ring-0 border border-border"
          data-testid="git-status-card"
          aria-label="Context repo status"
          // The button toggles the card itself: a press on it is not a click outside.
          onInteractOutside={(e) => {
            if (anchorRef.current?.contains(e.target as Node)) e.preventDefault();
          }}
        >
          {status.worktree ? (
            <WorktreeSection
              teamKey={teamKey}
              status={status}
              agentNames={agentNames}
              onCompare={() => {
                setOpen(false);
                onOpenChanges();
              }}
            />
          ) : (
            <>
              {status.conflicts.map((c) => (
                <ConflictSection
                  key={c.path}
                  teamKey={teamKey}
                  conflict={c}
                  orgRoot={orgRoot}
                  agentNames={agentNames}
                  onSettled={onSettled}
                  onCompare={() => {
                    setOpen(false);
                    onCompare(c.path);
                  }}
                />
              ))}
              <FailureSection
                teamKey={teamKey}
                status={status}
                model={model}
                orgRoot={orgRoot}
                agentNames={agentNames}
                onSettled={onSettled}
                onOpenChanges={() => {
                  setOpen(false);
                  onOpenChanges();
                }}
                now={now}
              />
            </>
          )}
        </PopoverContent>
      )}
    </Popover>
  );
}

// ---------------------------------------------------------------------------
// The card
// ---------------------------------------------------------------------------

const CARD_BTN =
  "inline-flex h-6 items-center gap-1 rounded-[5px] border border-border px-2 text-[11.5px] text-foreground hover:bg-[color-mix(in_srgb,var(--foreground)_6%,transparent)] focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring disabled:opacity-50";
const CARD_BTN_PRIMARY =
  "inline-flex h-6 items-center gap-1 rounded-[5px] border border-foreground bg-foreground px-2 text-[11.5px] font-[550] text-popover hover:opacity-90 focus-visible:outline-none focus-visible:ring-[1.5px] focus-visible:ring-ring focus-visible:ring-offset-1 disabled:opacity-50";

function WorktreeSection({
  teamKey,
  status,
  agentNames,
  onCompare,
}: {
  teamKey: string;
  status: ContextRepoStatus;
  agentNames: string[];
  onCompare: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const agents = status.agents ?? [];
  const ask = (name: string) => {
    const agent = agents.find((a) => a.name === name);
    const handle = agent?.handle ?? name;
    setBusy(true);
    void handWorktreeChanges(
      teamKey,
      handle,
      `Review the changes in context worktree ${status.worktree?.name} on ${status.branch}.`,
    )
      .catch((error: unknown) => toast.error(String(error)))
      .finally(() => setBusy(false));
  };
  const names = agents.map((a) => a.name);
  const first = names[0] ?? agentNames[0];
  return (
    <div className="space-y-2 p-3">
      <p className="font-medium">{status.worktree?.name} · vs main</p>
      <p className="text-muted-foreground">{status.uncommitted ?? 0} uncommitted files</p>
      {status.conflicts.length > 0 && (
        <p className="text-[var(--cs-git-conflict)]">
          {status.conflicts.length} {status.conflicts.length === 1 ? "conflict" : "conflicts"} to
          settle
        </p>
      )}
      {(status.sync.reason || status.sync.progress || status.sync.state !== "ok") && (
        <p className="text-muted-foreground">
          {status.sync.reason ?? status.sync.progress ?? "Git operation in progress"}
        </p>
      )}
      {status.owner && <p>Owner: {status.owner.kind === "person" ? "you" : status.owner.name}</p>}
      <div className="flex flex-wrap gap-1.5">
        <button type="button" className={CARD_BTN} onClick={onCompare}>
          Compare
        </button>
        {first && (
          <AskAgentButton
            label={`Hand to ${first}`}
            menuLabel={(name) => `Hand to ${name}`}
            menuAria="Hand to another agent"
            agent={first}
            others={names.slice(1)}
            busy={busy}
            onAsk={ask}
          />
        )}
      </div>
    </div>
  );
}

function fileName(path: string): string {
  const name = path.split("/").pop() ?? path;
  return name.replace(/\.(md|html?|excalidraw)$/i, "");
}

const RESOLUTION_WORD: Record<ContextConflictResolution, string> = {
  useMine: "Kept your version of",
  useTheirs: "Took GitHub's version of",
  keepBoth: "Kept both versions of",
};

function ConflictSection({
  teamKey,
  conflict,
  orgRoot,
  agentNames,
  onCompare,
  onSettled,
}: {
  teamKey: string;
  conflict: ContextConflict;
  orgRoot: string;
  agentNames: string[];
  onCompare: () => void;
  onSettled: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [now] = useState(() => Date.now());
  const name = fileName(conflict.path);
  const handTo = conflict.lastWriter ?? agentNames[0] ?? null;
  const others = agentNames.filter((a) => a !== handTo);

  const settle = (resolution: ContextConflictResolution) => {
    setBusy(true);
    settleConflict(orgRoot, conflict.path, resolution)
      .then(() => {
        toast.success(`${RESOLUTION_WORD[resolution]} ${name}.`);
        onSettled();
      })
      .catch((e) => toast.error(`Couldn't settle ${name}: ${String(e)}`))
      .finally(() => setBusy(false));
  };
  const hand = (agent: string) => {
    setBusy(true);
    handConflict(teamKey, orgRoot, conflict.path, agent)
      .then(() => {
        toast.success(`Handed ${name} to ${agent}.`);
        onSettled();
      })
      .catch((e) => toast.error(`Couldn't hand ${name} to ${agent}: ${String(e)}`))
      .finally(() => setBusy(false));
  };

  return (
    <section
      className="border-b border-border px-[9px] pt-[7px] pb-[9px] last:border-b-0"
      data-testid="git-conflict-card"
      aria-label={`${name} conflict`}
    >
      <p>
        <b className="font-semibold">{name}</b>
        <span className="text-muted-foreground"> · changed here and on GitHub</span>
      </p>
      <p className="mt-0.5 text-muted-foreground">
        {conflictSideLine(conflict.mine, "You", now)}{" "}
        {conflictSideLine(conflict.theirs, "GitHub", now)}
      </p>
      <div className="mt-[9px] flex flex-wrap gap-1.5">
        <button
          type="button"
          className={CARD_BTN}
          disabled={busy}
          onClick={() => settle("useMine")}
        >
          Use mine
        </button>
        <button
          type="button"
          className={CARD_BTN}
          disabled={busy}
          onClick={() => settle("useTheirs")}
        >
          Use theirs
        </button>
        <button
          type="button"
          className={CARD_BTN}
          disabled={busy}
          onClick={() => settle("keepBoth")}
        >
          Keep both
        </button>
        {handTo && (
          <AskAgentButton
            label={`Hand to ${handTo}`}
            menuLabel={(a) => `Hand to ${a}`}
            menuAria="Hand to another agent"
            agent={handTo}
            others={others}
            busy={busy}
            onAsk={hand}
          />
        )}
        <button type="button" className={CARD_BTN} onClick={onCompare}>
          Compare
        </button>
      </div>
      {conflict.lastWriter && (
        <p className="mt-1.5 text-muted-foreground">{conflict.lastWriter} last wrote this file</p>
      )}
      {conflict.heldBack.length > 0 && (
        <p className="mt-2 rounded-[5px] bg-[color-mix(in_srgb,var(--foreground)_6%,transparent)] px-2 py-1.5 text-[11px]">
          <b className="font-semibold">Held back:</b> {conflict.heldBack.length}{" "}
          {conflict.heldBack.length === 1 ? "change" : "changes"} from GitHub (
          {conflict.heldBack.map((p) => p.split("/").pop()).join(", ")}) arrive once this is
          settled.
        </p>
      )}
    </section>
  );
}

const ACTION_LABEL: Record<ContextSyncAction, string> = {
  commit: "Commit",
  retry: "Retry",
  signIn: "Sign in again",
  setUp: "Set up",
};

/** What the asked agent is asked to do, in the card's words. */
const ACTION_ASK: Record<ContextSyncAction, string> = {
  commit: "commit your private edits locally; never push",
  retry: "find out why and sync again",
  signIn: "sign in to GitHub again",
  setUp: "connect this repo to GitHub",
};

/** A primary button with ▾ for the team's other agents, when there are any. */
export function AskAgentButton({
  label,
  menuLabel,
  menuAria,
  agent,
  others,
  busy,
  onAsk,
  outline = false,
  compact = false,
}: {
  compact?: boolean;
  outline?: boolean;
  label: string;
  menuLabel: (agent: string) => string;
  menuAria: string;
  agent: string;
  others: string[];
  busy: boolean;
  onAsk: (agent: string) => void;
}) {
  return (
    <span className="inline-flex">
      <button
        type="button"
        className={cn(outline ? CARD_BTN : CARD_BTN_PRIMARY, others.length > 0 && "rounded-r-none")}
        disabled={busy}
        aria-label={label}
        onClick={() => onAsk(agent)}
      >
        {compact ? (
          <>
            <span className="@max-[16rem]:hidden">{label}</span>
            <span className="hidden @max-[16rem]:inline">Ask to commit</span>
          </>
        ) : (
          label
        )}
      </button>
      {others.length > 0 && (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              aria-label={menuAria}
              className={cn(
                outline ? CARD_BTN : CARD_BTN_PRIMARY,
                "rounded-l-none border-l-[color-mix(in_srgb,var(--popover)_30%,var(--foreground))] px-1",
              )}
              disabled={busy}
            >
              <ChevronDown size={11} aria-hidden />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="min-w-36">
            {others.map((a) => (
              <DropdownMenuItem key={a} className="text-[12px]" onSelect={() => onAsk(a)}>
                {menuLabel(a)}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </span>
  );
}

function FailureSection({
  teamKey,
  status,
  model,
  orgRoot,
  agentNames,
  onSettled,
  onOpenChanges,
  now,
}: {
  teamKey: string;
  status: ContextRepoStatus;
  model: StatusButtonModel;
  orgRoot: string;
  agentNames: string[];
  onSettled: () => void;
  /** The card stands where a click would have shown Changes, so it links there. */
  onOpenChanges: () => void;
  now: number;
}) {
  const [busy, setBusy] = useState(false);
  const { sync } = status;
  const ago = sync.lastSyncedAt ? agoText(sync.lastSyncedAt, now) : null;
  const safe =
    model.up > 0
      ? ` Your ${model.up} ${model.up === 1 ? "change is" : "changes are"} safe here.`
      : "";

  let title: string;
  let body: string;
  let action: ContextSyncAction;
  if (model.state === "failed") {
    title = "Sync failed";
    body = `${sync.reason ? `${sync.reason.replace(/\.$/, "")}. ` : ""}${ago ? `Last synced ${ago}.` : ""}${safe}`;
    action = "retry";
  } else if (model.state === "signin") {
    title = "GitHub sign-in expired";
    body = `Nothing can be sent or brought in until an agent signs in again.${ago ? ` Last synced ${ago}.` : ""}`;
    action = "signIn";
  } else if (model.unsent) {
    title = `${model.up} not sent`;
    body =
      "This repo has no GitHub remote, so nothing will send these changes until an agent connects it.";
    action = "setUp";
  } else {
    return null;
  }

  const agent = agentNames[0] ?? null;
  const ask = (name: string) => {
    setBusy(true);
    runSyncAction(teamKey, orgRoot, action, name, sync.reason ?? null)
      .then(() => {
        toast.success(`Asked ${name} to ${ACTION_ASK[action]}.`);
        onSettled();
      })
      .catch((e) => toast.error(`Couldn't ask ${name}: ${String(e)}`))
      .finally(() => setBusy(false));
  };

  return (
    <section
      className="px-[9px] pt-[7px] pb-[9px]"
      data-testid="git-failure-card"
      aria-label={title}
    >
      <p className="font-semibold">{title}</p>
      {body && <p className="mt-0.5 text-muted-foreground">{body.trim()}</p>}
      <div className="mt-[9px] flex gap-1.5">
        {agent ? (
          <AskAgentButton
            label={ACTION_LABEL[action]}
            menuLabel={(a) => `Ask ${a}`}
            menuAria="Ask another agent"
            agent={agent}
            others={agentNames.slice(1)}
            busy={busy}
            onAsk={ask}
          />
        ) : (
          <button type="button" className={CARD_BTN_PRIMARY} disabled>
            {ACTION_LABEL[action]}
          </button>
        )}
        {model.up > 0 && (
          <button type="button" className={CARD_BTN} onClick={onOpenChanges}>
            Show changes
          </button>
        )}
      </div>
      <p className="mt-1.5 text-muted-foreground">
        {agent
          ? `Asks ${agent} to ${ACTION_ASK[action]}.`
          : "No agent on this team to ask. Add one to the team first."}
      </p>
    </section>
  );
}

export function PrivateCommitButton({ teamKey, orgRoot }: { teamKey: string; orgRoot: string }) {
  const sessions = useAgentListProjectionStore((s) => s.sessions);
  const agents = useAgentsStore((s) => s.rows);
  const colon = teamKey.indexOf(":");
  const org = teamKey.slice(0, colon);
  const team = teamKey.slice(colon + 1);
  const keys = new Set(
    agents
      ?.filter((a) => a.org === org && (a.team === team || a.team === "_scratch") && !a.archived)
      .map((a) => a.key),
  );
  const agentNames = (sessions ?? [])
    .filter(
      (s) =>
        s.alive &&
        s.machineId === null &&
        s.agentRevision &&
        keys.has(s.agentRevision.slice(0, s.agentRevision.lastIndexOf("@"))),
    )
    .map((s) => s.handle);
  const [busy, setBusy] = useState(false);
  const ask = async (name: string) => {
    setBusy(true);
    try {
      await runSyncAction(teamKey, orgRoot, "commit", name, null);
      toast.success(`Asked ${name} to commit your private edits.`);
    } catch (e) {
      toast.error(`Couldn't ask the agent: ${String(e)}`);
    } finally {
      setBusy(false);
    }
  };
  if (!agentNames.length)
    return (
      <span className="text-[11px] font-normal text-muted-foreground">
        No agent on this Mac to ask.
      </span>
    );
  return (
    <AskAgentButton
      outline
      compact
      label={`Ask ${agentNames[0]} to commit`}
      menuLabel={(name) => `Ask ${name} to commit`}
      menuAria="Ask another agent"
      agent={agentNames[0]}
      others={agentNames.slice(1)}
      busy={busy}
      onAsk={(name) => void ask(name)}
    />
  );
}

export function PrivateRepoStatusButton({
  orgName,
  lastCommitAt,
  uncommitted,
  error,
  onOpenChanges,
  onRetry,
}: {
  orgName?: string;
  lastCommitAt?: number | null;
  uncommitted: number;
  error?: string | null;
  onOpenChanges: () => void;
  onRetry: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  if (!error && open) setOpen(false);
  const sentence = `Private · local only · only you see it · ${error ? "couldn't read its git state" : uncommitted ? `${uncommitted} uncommitted edits${orgName ? ` in ${orgName}` : ""}` : "nothing changed since the last commit"}${lastCommitAt ? ` · last commit ${agoText(lastCommitAt, now)}` : ""}`;
  return (
    <Popover open={!!error && open} onOpenChange={setOpen}>
      <PopoverAnchor asChild>
        <button
          type="button"
          data-testid="git-status-button"
          aria-label={sentence}
          title={sentence}
          onPointerEnter={() => setNow(Date.now())}
          onFocus={() => setNow(Date.now())}
          onClick={() => (error ? setOpen(!open) : onOpenChanges())}
          className="inline-flex h-[22px] items-center gap-1.5 rounded-md border border-border px-1.5 text-[11.5px] text-muted-foreground hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <Lock size={11} aria-hidden />
          Private
          <span aria-hidden className="h-3 w-px bg-border" />
          <span className="@max-[12rem]:hidden">{error ? "can't read" : "local only"}</span>
        </button>
      </PopoverAnchor>
      {error && (
        <PopoverContent aria-label="Private repo status" className="w-[264px] p-3 text-xs">
          <h3 className="font-semibold">Couldn't read your private repo</h3>
          <p className="py-2">{error}</p>
          <button type="button" className={CARD_BTN_PRIMARY} onClick={onRetry}>
            Read again
          </button>
        </PopoverContent>
      )}
    </Popover>
  );
}
