// SPDX-License-Identifier: MIT
import {
  useCallback,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { AlertTriangle, Check, ChevronDown, FolderGit2, Users } from "lucide-react";
import { toast } from "sonner";

import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import { resolveAgentFace } from "@/features/viewer/lib/agent-list-view";
import { RepoGlyph } from "@/shared/components/repo-glyph";
import { FloatingMenu } from "@/shared/components/floating-menu";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { getRepoColor } from "@/shared/lib/pipeline-graph/epic-colors";
import { cn } from "@/lib/utils";

import type { AgentContextPivot as PivotState, PivotAgent } from "../hooks/use-agent-context";
import {
  contextHeaderLabel,
  contextSummary,
  repoRequestLabel,
  TEAM_EVERYTHING_LABEL,
} from "../lib/agent-context-model";

// The nav dropdowns (org · team · this pivot) share one row shape.
const ROW =
  "flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-sm outline-none hover:bg-accent hover:text-accent-foreground focus-visible:bg-accent focus-visible:text-accent-foreground";
const TYPEAHEAD_RESET_MS = 600;

/** The listbox keyboard model: arrows, Home/End and type-ahead move focus between options;
 *  Enter/Space pick (the option is a button). Focus is roving: only the selected option is in
 *  the tab order, so Tab moves between the two lists. */
function useListboxKeys(): {
  register: (el: HTMLElement | null, index: number, label: string) => void;
  onKeyDown: (e: KeyboardEvent<HTMLElement>) => void;
} {
  const items = useRef<Array<{ el: HTMLElement; label: string }>>([]);
  const typed = useRef({ text: "", at: 0 });
  const register = useCallback((el: HTMLElement | null, index: number, label: string) => {
    if (el) items.current[index] = { el, label: label.toLowerCase() };
  }, []);
  const onKeyDown = useCallback((e: KeyboardEvent<HTMLElement>) => {
    const list = items.current.filter((i) => i && i.el.isConnected);
    if (list.length === 0) return;
    const at = list.findIndex((i) => i.el === document.activeElement);
    const focus = (n: number) => list[(n + list.length) % list.length]?.el.focus();
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        focus(at + 1);
        return;
      case "ArrowUp":
        e.preventDefault();
        focus(at <= 0 ? list.length - 1 : at - 1);
        return;
      case "Home":
        e.preventDefault();
        focus(0);
        return;
      case "End":
        e.preventDefault();
        focus(list.length - 1);
        return;
    }
    if (e.key.length !== 1 || e.metaKey || e.ctrlKey || e.altKey || e.key === " ") return;
    e.preventDefault();
    const now = Date.now();
    const text =
      (now - typed.current.at > TYPEAHEAD_RESET_MS ? "" : typed.current.text) + e.key.toLowerCase();
    typed.current = { text, at: now };
    // A repeated single letter cycles through the options starting with it.
    const cycling = text.length > 1 && [...text].every((c) => c === text[0]);
    const needle = cycling ? text[0] : text;
    const start = cycling || text.length === 1 ? at + 1 : Math.max(at, 0);
    for (let k = 0; k < list.length; k++) {
      const i = (start + k) % list.length;
      if (list[i].label.startsWith(needle)) {
        list[i].el.focus();
        return;
      }
    }
  }, []);
  return { register, onKeyDown };
}

function AgentFace({ agent, size }: { agent: PivotAgent; size: number }) {
  const { faces } = useAvatarPacks();
  const face = resolveAgentFace(
    {
      avatar: agent.avatar,
      agentRevision: `${agent.key}@0`,
      handle: agent.key,
      displayName: agent.name,
    },
    { name: agent.name, faces },
  );
  return (
    <span className="cs-agent-identity inline-flex flex-shrink-0">
      <AgentFaceView face={face} size={size} />
    </span>
  );
}

function Option({
  selected,
  onPick,
  register,
  index,
  label,
  testId,
  leading,
  children,
}: {
  selected: boolean;
  onPick: () => void;
  register: (el: HTMLElement | null, index: number, label: string) => void;
  index: number;
  label: string;
  testId: string;
  leading: ReactNode;
  children?: ReactNode;
}) {
  return (
    <button
      type="button"
      role="option"
      aria-selected={selected}
      tabIndex={selected ? 0 : -1}
      data-testid={testId}
      data-selected={selected || undefined}
      ref={(el) => register(el, index, label)}
      className={cn(ROW, "items-start", selected && "font-medium")}
      onClick={(e) => {
        e.stopPropagation();
        onPick();
      }}
    >
      <span className="mt-0.5 flex w-4 flex-shrink-0 justify-center">{leading}</span>
      <span className="min-w-0 flex-1">
        <span className="block truncate">{label}</span>
        {children}
      </span>
      {selected && <Check size={14} className="mt-0.5 ml-auto flex-shrink-0" aria-hidden="true" />}
    </button>
  );
}

function GroupLabel({ id, children }: { id: string; children: ReactNode }) {
  return (
    <div
      id={id}
      role="presentation"
      className="px-2 pb-0.5 pt-2 text-xs font-medium text-muted-foreground"
    >
      {children}
    </div>
  );
}

/** "Team (everything)", whose parenthetical yields first in a narrow container. */
function TeamEverythingLabel() {
  const cut = TEAM_EVERYTHING_LABEL.indexOf(" ");
  if (cut < 0) return <span className="truncate">{TEAM_EVERYTHING_LABEL}</span>;
  return (
    <span className="truncate">
      {TEAM_EVERYTHING_LABEL.slice(0, cut)}
      <span className="hidden @[18rem]:inline">{TEAM_EVERYTHING_LABEL.slice(cut)}</span>
    </span>
  );
}

/**
 * SW-XS-15 — the left-nav context pivot: **Team (everything)** (the raw context repo), or
 * **Context for ‹Agent›** with an optional **· in ‹Repo›**. The header under it summarizes
 * the agent's want set (`1,214 files · 18.2 MB`), the overlay below it shows which nav items
 * are in that set.
 *
 * Two listboxes in one popover: whose context (Team first, then the team's agents, the
 * selected one first) and, with an agent picked, which repo. Each has type-ahead.
 *
 * The root is `display: contents`: the trigger and the summary line lay out in the parent's
 * two-column grid, so the summary can span the full row under the trigger and its neighbour.
 */
export function AgentContextPivot({
  pivot,
  glyphMap,
  isLightTheme,
}: {
  pivot: PivotState;
  /** Source-repo slug → short code, shared with the nav's repo tint. */
  glyphMap: Map<string, string>;
  isLightTheme: boolean;
}) {
  const [open, setOpen] = useState(false);
  const whose = useListboxKeys();
  const where = useListboxKeys();
  const popoverRef = useRef<HTMLDivElement | null>(null);
  const agentsId = useId();
  const whereId = useId();
  const { agent, repo, agents, repos, view, loading, error } = pivot;

  // The selected agent first, then the rest by name.
  const ordered = useMemo(
    () =>
      agent
        ? [
            ...agents.filter((a) => a.key === agent.key),
            ...agents.filter((a) => a.key !== agent.key),
          ]
        : agents,
    [agents, agent],
  );

  const pick = useCallback(
    (next: Parameters<PivotState["select"]>[0]) => {
      setOpen(false);
      pivot.select(next);
    },
    [pivot],
  );

  const label = agent ? contextHeaderLabel(agent.name, repo?.repo) : TEAM_EVERYTHING_LABEL;
  const warnings = view?.warnings.filter((w) => w.code !== "skipped") ?? [];

  return (
    <div className="contents" data-testid="agent-context-header">
      <FloatingMenu
        open={open}
        onOpenChange={setOpen}
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          // Land on the selected option, so arrows and type-ahead start from it.
          requestAnimationFrame(() => {
            const root =
              popoverRef.current?.closest("[data-radix-popper-content-wrapper]") ?? document;
            const selected = root.querySelector<HTMLElement>(
              '[role="option"][aria-selected="true"]',
            );
            selected?.focus();
          });
        }}
        trigger={
          <button
            type="button"
            data-testid="agent-context-trigger"
            aria-haspopup="dialog"
            aria-expanded={open}
            title={label}
            aria-label={label}
            onClick={(e) => e.stopPropagation()}
            className="group -mx-1 flex min-w-0 max-w-full items-center gap-1.5 justify-self-start rounded px-1 py-0.5 hover:bg-accent hover:text-accent-foreground aria-expanded:bg-accent aria-expanded:text-accent-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {agent ? (
              <AgentFace agent={agent} size={16} />
            ) : (
              <Users
                size={13}
                className="flex-shrink-0 text-muted-foreground group-hover:text-current group-aria-expanded:text-current"
                aria-hidden="true"
              />
            )}
            {/* In a narrow container the qualifiers go first ("Context for", "(everything)",
                "· in repo"), so the agent's name or "Team" stays readable; the full label
                stays in the tooltip and the accessible name. */}
            <span
              className="flex min-w-0 items-baseline whitespace-pre"
              data-testid="agent-context-label"
            >
              {agent ? (
                <>
                  <span className="hidden text-muted-foreground group-hover:text-current group-aria-expanded:text-current @[18rem]:inline">
                    Context for{" "}
                  </span>
                  <span className="min-w-0 max-w-full flex-shrink-0 truncate font-medium">
                    {agent.name}
                  </span>
                  {repo && (
                    <span className="min-w-0 truncate text-muted-foreground group-hover:text-current group-aria-expanded:text-current">
                      {" "}
                      · in {repo.repo}
                    </span>
                  )}
                </>
              ) : (
                <TeamEverythingLabel />
              )}
            </span>
            <ChevronDown
              className="flex-shrink-0 text-muted-foreground group-hover:text-current group-aria-expanded:text-current"
              size={12}
              aria-hidden="true"
            />
          </button>
        }
        className="overflow-y-auto"
        style={{ minWidth: 232, maxHeight: 380 }}
      >
        <div ref={popoverRef}>
          <div role="listbox" aria-label="Whose context" onKeyDown={whose.onKeyDown}>
            <Option
              selected={!agent}
              onPick={() => pick(null)}
              register={whose.register}
              index={0}
              label={TEAM_EVERYTHING_LABEL}
              testId="agent-context-option-team"
              leading={<Users size={13} className="text-muted-foreground" aria-hidden="true" />}
            >
              <span className="block truncate text-xs font-normal text-muted-foreground">
                All of the context repo, not what an agent gets
              </span>
            </Option>
            {ordered.length > 0 && (
              <div role="group" aria-labelledby={agentsId}>
                <GroupLabel id={agentsId}>Agents</GroupLabel>
                {ordered.map((a, i) => (
                  <Option
                    key={a.key}
                    selected={agent?.key === a.key}
                    onPick={() =>
                      pick({
                        agentKey: a.key,
                        // Keep the repo when switching agents: the question stays "in codesense".
                        ...(repo ? { repoSlug: repoRequestLabel(repo) } : {}),
                      })
                    }
                    register={whose.register}
                    index={i + 1}
                    label={a.name}
                    testId={`agent-context-option-${a.key}`}
                    leading={<AgentFace agent={a} size={16} />}
                  />
                ))}
              </div>
            )}
          </div>
          {ordered.length === 0 && (
            <div
              className="px-2 pb-1.5 pt-2 text-xs text-muted-foreground"
              data-testid="agent-context-no-agents"
            >
              No team agents yet. Create one in Agents.
            </div>
          )}
          {agent && repos.length > 0 && (
            <>
              <div className="my-1 h-px bg-border" role="separator" />
              <GroupLabel id={whereId}>In repo</GroupLabel>
              <div role="listbox" aria-labelledby={whereId} onKeyDown={where.onKeyDown}>
                <Option
                  selected={!repo}
                  onPick={() => pick({ agentKey: agent.key })}
                  register={where.register}
                  index={0}
                  label="Team workspace"
                  testId="agent-context-repo-none"
                  leading={
                    <FolderGit2 size={13} className="text-muted-foreground" aria-hidden="true" />
                  }
                >
                  <span className="block truncate text-xs font-normal text-muted-foreground">
                    No repo: what a team-workspace launch gets
                  </span>
                </Option>
                {repos.map((r, i) => {
                  const slug = `${r.owner}/${r.repo}`;
                  return (
                    <Option
                      key={slug}
                      selected={!!repo && repoRequestLabel(repo) === repoRequestLabel(r)}
                      onPick={() => pick({ agentKey: agent.key, repoSlug: repoRequestLabel(r) })}
                      register={where.register}
                      index={i + 1}
                      label={r.repo}
                      testId={`agent-context-repo-${slug}`}
                      leading={
                        <RepoGlyph
                          glyph={glyphMap.get(slug)}
                          glyphColor={getRepoColor(slug, isLightTheme)}
                          slug={slug}
                        />
                      }
                    />
                  );
                })}
              </div>
            </>
          )}
        </div>
      </FloatingMenu>
      {agent && (
        <div
          className="col-span-2 flex min-w-0 items-center gap-1.5 pl-[22px] text-[11px] leading-4 text-muted-foreground tabular-nums"
          data-testid="agent-context-summary"
          aria-busy={loading || (!view && !error)}
        >
          {/* Read as one sentence: "Context for Barb · in codesense — 1,214 files · 18.2 MB". */}
          <span className="sr-only"> — </span>
          {error ? (
            <>
              <span className="truncate text-[var(--destructive)]" title={error} role="status">
                Couldn&apos;t resolve {agent.name}&apos;s context
              </span>
              <button
                type="button"
                className="flex-shrink-0 underline underline-offset-2 hover:text-foreground"
                onClick={pivot.reload}
              >
                Retry
              </button>
            </>
          ) : view ? (
            <>
              {/* Only the totals are announced, once per change; "updating…" is visual. */}
              <span className="truncate" aria-live="polite">
                {contextSummary(view.totals)}
              </span>
              {loading && (
                <span className="flex-shrink-0" aria-hidden="true">
                  · updating…
                </span>
              )}
              {warnings.length > 0 && (
                <button
                  type="button"
                  className="ml-auto flex flex-shrink-0 items-center gap-1 rounded hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  title={warnings.map((w) => w.message).join("\n")}
                  data-testid="agent-context-warnings"
                  onClick={() =>
                    toast(`${agent.name}'s context`, {
                      description: warnings.map((w) => w.message).join("\n"),
                    })
                  }
                >
                  <AlertTriangle size={11} aria-hidden="true" />
                  {warnings.length} {warnings.length === 1 ? "warning" : "warnings"}
                </button>
              )}
            </>
          ) : (
            <span>Resolving…</span>
          )}
        </div>
      )}
    </div>
  );
}
