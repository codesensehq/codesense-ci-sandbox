// SPDX-License-Identifier: MIT
import type { ReactNode } from "react";
import { GitBranch, GitCommitHorizontal } from "lucide-react";
import type { GitPromptStatus } from "../hooks/use-session-prompt";
import { InfoTip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { formatPromptPath, type PromptPath } from "../lib/prompt-path";

/**
 * The terminal caption's prompt segment, read like a good shell prompt
 * (starship / p10k): an abbreviated path whose current directory carries the
 * weight, the Machine it runs on for a remote tab, and — inside a checkout —
 * the branch and compact status counts.
 *
 * Colour: the caption inherits its terminal surface palette,
 * so the counts share the status mark palette on that ground. Each falls back
 * to the matching `--cs-status-added/modified/deleted` diff token.
 */

const COUNT_TONE = {
  conflicted: "text-[var(--cs-status-failed,var(--cs-status-deleted))]",
  staged: "text-[var(--cs-status-done,var(--cs-status-added))]",
  modified: "text-[var(--cs-status-needs,var(--cs-status-modified))]",
  untracked: "opacity-70",
  sync: "opacity-80",
} as const;

/** The path, never wrapping: when even the abbreviated form is too wide it
 *  gives way from the left, so the current directory stays in view. */
export function PromptPathLabel({
  path,
  id,
  emphasis,
  className,
}: {
  path: PromptPath;
  id?: string;
  /** The active tab's leaf takes 600, the rest 500. */
  emphasis: "medium" | "semibold";
  className?: string;
}) {
  const muted = `${path.head}${path.parent ? `${path.parent}/` : ""}`;
  return (
    <span
      id={id}
      data-testid="prompt-path"
      aria-label={formatPromptPath(path)}
      // `direction: rtl` puts the overflow (and its ellipsis) at the start;
      // the inner `bdi` keeps the characters in reading order.
      className={cn(
        "block min-w-0 overflow-hidden text-ellipsis whitespace-nowrap font-mono text-[12px] leading-4 [direction:rtl] text-left",
        className,
      )}
    >
      <bdi dir="ltr">
        {muted && (
          <span data-testid="prompt-path-parent" className="opacity-60">
            {muted}
          </span>
        )}
        <span
          data-testid="prompt-path-leaf"
          className={emphasis === "semibold" ? "font-semibold" : "font-medium"}
        >
          {path.leaf}
        </span>
      </bdi>
    </span>
  );
}

/** The Machine a remote tab runs on, set before its path. */
export function MachineChip({ label }: { label: string }) {
  return (
    <span
      data-testid="prompt-machine"
      className="inline-flex h-[18px] max-w-[14ch] shrink-0 items-center truncate rounded-[4px] px-1.5 text-[11px] font-medium leading-none bg-[color-mix(in_srgb,currentColor_12%,transparent)]"
    >
      <span className="truncate">{label}</span>
    </span>
  );
}

interface Count {
  key: keyof typeof COUNT_TONE;
  symbol: string;
  value: number;
  tip: string;
}

/** The status counts a prompt shows, in prompt order; zeros are left out. */
export function gitPromptCounts(status: GitPromptStatus): Count[] {
  const upstream = status.upstream ?? "upstream";
  const counts: Count[] = [
    {
      key: "conflicted",
      symbol: "!",
      value: status.conflicted,
      tip: `${status.conflicted} conflicted ${plural(status.conflicted, "file")}`,
    },
    {
      key: "staged",
      symbol: "+",
      value: status.staged,
      tip: `${status.staged} staged ${plural(status.staged, "change")}`,
    },
    {
      key: "modified",
      symbol: "~",
      value: status.modified,
      tip: `${status.modified} modified, not staged`,
    },
    {
      key: "untracked",
      symbol: "?",
      value: status.untracked,
      tip: `${status.untracked} untracked ${plural(status.untracked, "file")}`,
    },
    {
      key: "sync",
      symbol: "⇡",
      value: status.ahead,
      tip: `${status.ahead} ${plural(status.ahead, "commit")} ahead of ${upstream}`,
    },
    {
      key: "sync",
      symbol: "⇣",
      value: status.behind,
      tip: `${status.behind} ${plural(status.behind, "commit")} behind ${upstream}`,
    },
  ];
  return counts.filter((c) => c.value > 0);
}

function plural(n: number, word: string): string {
  return n === 1 ? word : `${word}s`;
}

/** The ref the segment names: the branch, else the short commit. */
export function gitPromptRef(status: GitPromptStatus): string {
  if (!status.detached && status.branch) return status.branch;
  return status.sha ? status.sha.slice(0, 7) : "HEAD";
}

/** A one-line reading of the whole segment, for assistive tech. */
export function gitPromptSummary(status: GitPromptStatus): string {
  const ref = status.detached ? `detached at ${gitPromptRef(status)}` : gitPromptRef(status);
  const counts = gitPromptCounts(status).map((c) => c.tip);
  return counts.length ? `${ref}, ${counts.join(", ")}` : `${ref}, clean`;
}

/**
 * Branch (or short SHA when detached) and the compact counts. Clicking opens
 * the tab's source diff when the tab has one; otherwise the segment is read
 * only. Every piece names itself on hover.
 */
export function GitPromptSegment({
  status,
  onOpen,
  openLabel = "Open diff",
  openDisabled = false,
  testId,
}: {
  status: GitPromptStatus;
  onOpen?: () => void;
  openLabel?: string;
  /** What `onOpen` opens is already open: the segment stays the same button
   *  (an overlay that returns focus to it on close still finds it) but reads
   *  `aria-disabled` and does nothing. */
  openDisabled?: boolean;
  testId?: string;
}) {
  const canOpen = !!onOpen && !openDisabled;
  const ref = gitPromptRef(status);
  const counts = gitPromptCounts(status);
  const RefIcon = status.detached ? GitCommitHorizontal : GitBranch;
  const refTip = status.detached
    ? `Detached HEAD at ${status.sha ? status.sha.slice(0, 12) : "an unborn commit"}`
    : status.upstream
      ? `${ref} → ${status.upstream}`
      : `${ref} (no upstream)`;
  const body: ReactNode = (
    <>
      <InfoTip label={canOpen ? `${refTip} · ${openLabel}` : refTip} side="bottom">
        <span data-testid="prompt-git-ref" className="inline-flex min-w-0 items-center gap-1">
          <RefIcon size={12} strokeWidth={2} aria-hidden className="shrink-0 opacity-70" />
          <span className={cn("min-w-0 truncate", status.detached && "opacity-80")}>{ref}</span>
        </span>
      </InfoTip>
      {counts.length > 0 && (
        <span className="inline-flex shrink-0 items-center gap-1.5 tabular-nums">
          {counts.map((c) => (
            <InfoTip key={c.symbol} label={c.tip} side="bottom">
              <span data-testid={`prompt-git-${c.symbol}`} className={COUNT_TONE[c.key]}>
                {c.symbol}
                {c.value}
              </span>
            </InfoTip>
          ))}
        </span>
      )}
    </>
  );
  const shape =
    "inline-flex h-6 min-w-0 max-w-[45%] shrink items-center gap-2 rounded px-1.5 font-mono text-[11.5px] leading-4";
  if (!onOpen) {
    return (
      <span
        data-testid={testId}
        role="group"
        aria-label={gitPromptSummary(status)}
        className={shape}
      >
        {body}
      </span>
    );
  }
  return (
    <button
      type="button"
      data-testid={testId}
      aria-label={canOpen ? `${gitPromptSummary(status)}. ${openLabel}` : gitPromptSummary(status)}
      aria-disabled={canOpen ? undefined : true}
      onClick={(e) => {
        e.stopPropagation();
        if (canOpen) onOpen();
      }}
      onDoubleClick={(e) => e.stopPropagation()}
      // Like the caption's icon cluster: the segment's own press and Enter /
      // Space never also reach the caption's cell handlers.
      onMouseDown={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") e.stopPropagation();
      }}
      className={cn(
        shape,
        canOpen
          ? "opacity-90 transition-colors hover:opacity-100 hover:bg-[color-mix(in_srgb,var(--muted)_25%,transparent)]"
          : "cursor-default",
        "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-[#e6edf3]",
      )}
    >
      {body}
    </button>
  );
}
