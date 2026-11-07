// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { InfoTip } from "@/components/ui/tooltip";
import { StatusMark } from "@/shared/components/status-mark";
import {
  type ScopeStory,
  type StoryTab,
  useScopeMark,
  useStoryTabs,
  useStoryWorkspaceIds,
} from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";

/** The tooltip's surface: a popover card rather than the one-line ink tooltip. */
export const NAV_TIP_CLASS =
  "flex-col items-stretch gap-0.5 border border-border bg-popover px-2.5 py-2 text-[11.5px] leading-[1.45] text-popover-foreground shadow-lg [&>span:has(>svg)]:hidden";

/**
 * Who works on a story, as one mark at the row's end: the most urgent agent's mark over its
 * workspace's tabs, read through the same scope the workspace pill draws, so the nav can never
 * disagree with the tabs, the pill or the tray. Nothing renders (and no gap is kept) while the
 * story has no workspace or its workspace runs no agent. Hovering names each agent, its state
 * and how long; clicking opens the workspace.
 */
export function StoryAgentMark({
  teamKey,
  story,
  storyLabel,
  onOpen,
}: {
  teamKey: string;
  story: ScopeStory;
  storyLabel: string;
  onOpen?: () => void;
}) {
  const scope = useMemo(
    () => ({ teamKey, story: { filePath: story.filePath, storyId: story.storyId } }),
    [teamKey, story.filePath, story.storyId],
  );
  const { mark, label, words } = useScopeMark(scope);
  const tabs = useStoryTabs(teamKey, scope.story);
  if (!mark) return null;
  const tip = (
    <>
      <span className="mb-0.5">
        <b className="font-semibold">{storyLabel}</b>{" "}
        <span className="text-muted-foreground">· {label}</span>
      </span>
      {tabs.map((t) => (
        <AgentLine key={t.sessionId} tab={t} />
      ))}
      {onOpen && (
        <span className="mt-1 text-muted-foreground">Click the mark to open the workspace</span>
      )}
    </>
  );
  return (
    <InfoTip label={tip} side="bottom" collisionPadding={8} contentClassName={NAV_TIP_CLASS}>
      <button
        type="button"
        className="grid h-4 w-4 flex-none place-items-center rounded-sm focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
        aria-label={`${storyLabel}: ${words}${onOpen ? ". Open the workspace" : ""}`}
        data-testid="nav-story-agent-mark"
        data-mark={mark}
        onClick={(e) => {
          e.stopPropagation();
          onOpen?.();
        }}
      >
        <StatusMark mark={mark} size={8} />
      </button>
    </InfoTip>
  );
}

/** One agent in a story's tooltip: its own mark and words, or nothing for a plain shell. */
function AgentLine({ tab }: { tab: StoryTab }) {
  const scope = useMemo(
    () => ({ members: [{ sessionId: tab.sessionId, exit: tab.exit }] }),
    [tab.sessionId, tab.exit],
  );
  const { mark, label } = useScopeMark(scope);
  if (!mark) return null;
  // "claude needs approval · 2m": who and the state in ink, what qualifies it muted. The
  // words are the tab's own, so the tooltip says what the tab says.
  const [state, ...rest] = label.split(" · ");
  const detail = rest.join(" · ");
  return (
    <span className="flex items-center gap-[7px]">
      <StatusMark mark={mark} size={8} />
      <span className="min-w-0 truncate">
        <b className="font-semibold">{tab.name}</b> {state.toLowerCase()}
        {detail && <span className="text-muted-foreground"> · {detail}</span>}
      </span>
    </span>
  );
}

/**
 * An epic's roll-up beside its done/total: the most urgent mark over every story workspace in
 * it, named in words ("1 needs you · 2 working"). Nothing while no story in it has an agent.
 */
export function EpicAgentMark({
  teamKey,
  stories,
  epicLabel,
}: {
  teamKey: string;
  stories: readonly ScopeStory[];
  epicLabel: string;
}) {
  const wsIds = useStoryWorkspaceIds(teamKey, stories);
  const scope = useMemo(() => ({ teamKey, wsIds }), [teamKey, wsIds]);
  const { mark, label } = useScopeMark(scope);
  if (!mark || wsIds.length === 0) return null;
  return (
    <StatusMark
      mark={mark}
      size={8}
      label={`${epicLabel}: ${label}`}
      data-testid="nav-epic-agent-mark"
    />
  );
}
