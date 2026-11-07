// SPDX-License-Identifier: MIT
import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";

import { listen } from "@tauri-apps/api/event";

import { useCurrentUser } from "@/shared/lib/get-current-user";
import { formatKeystrokeDisplay } from "@/shared/lib/shortcuts/keystroke";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { useEffectiveShortcut } from "@/shared/stores/shortcuts-store";
import { useAgentRows } from "@/features/agents/lib/agent-names";
import {
  openStoryLaunchSheet,
  orderStoryLaunchAgents,
  probeAgentCli,
  toastLaunchError,
  type LaunchAvailability,
  type StoryAgentLaunchActions,
  type StoryLaunchAgent,
} from "./story-agent-launch";

/** Re-check cadence while a CLI's startup PATH probe is still running. */
const PROBE_RETRY_MS = 1500;
const PROBE_RETRY_LIMIT = 20;

/** The registry id of the "launch the default agent on this story" shortcut. */
export const STORY_LAUNCH_SHORTCUT_ID = "story.launch-agent";

/**
 * Build the "Launch agent" actions for a team's story surfaces: the agents you can see
 * (the team's first), each marked with whether its CLI is on the login-shell PATH, and a
 * `launch` that runs the one-action story launch and toasts a failure. Returns `null`
 * until the user and agents are known, and when there is no agent.
 */
export function useStoryAgentLaunch(teamKey: string): StoryAgentLaunchActions | null {
  const userId = useCurrentUser()?.github_user_id ?? null;
  // Read once and again only when agents change: session churn never re-reads it.
  const { rows } = useAgentRows();
  const shortcut = useEffectiveShortcut(STORY_LAUNCH_SHORTCUT_ID);

  const ordered = useMemo(
    () => (userId !== null ? orderStoryLaunchAgents(rows, teamKey) : []),
    [rows, teamKey, userId],
  );

  // One dependency-cache read per distinct agent CLI.
  const probeKey = useMemo(
    () => JSON.stringify([...new Set(ordered.map((agent) => agent.binary))]),
    [ordered],
  );
  const [availability, setAvailability] = useState<Record<string, LaunchAvailability>>({});
  // Bumped when the backend re-resolves dependencies (a re-check, or a CLI
  // appearing on PATH), so the menu re-probes instead of staying stale.
  const [depsRevision, setDepsRevision] = useState(0);

  useEffect(() => {
    const group = createListenerGroup();
    group.track(listen("dep-status-changed", () => setDepsRevision((n) => n + 1)));
    return () => group.dispose();
  }, []);

  useEffect(() => {
    const binaries = JSON.parse(probeKey) as string[];
    let cancelled = false;
    const timers: ReturnType<typeof setTimeout>[] = [];
    const probe = (binary: string, attempt: number) => {
      void probeAgentCli({ binary }).then((state) => {
        if (cancelled) return;
        setAvailability((prev) => ({ ...prev, [binary]: state }));
        // A CLI whose PATH probe is still running is not known yet: check again.
        if (state === "checking" && attempt < PROBE_RETRY_LIMIT) {
          timers.push(setTimeout(() => probe(binary, attempt + 1), PROBE_RETRY_MS));
        }
      });
    };
    for (const binary of binaries) probe(binary, 0);
    return () => {
      cancelled = true;
      timers.forEach(clearTimeout);
    };
  }, [probeKey, depsRevision]);

  return useMemo(() => {
    if (ordered.length === 0) return null;
    const withAvailability: StoryLaunchAgent[] = ordered.map((agent) => ({
      ...agent,
      availability: availability[agent.binary] ?? "checking",
    }));
    return {
      agents: withAvailability,
      shortcutLabel: shortcut ? formatKeystrokeDisplay(shortcut) : undefined,
      // A pick opens the launch sheet on the story with that agent: the person names it and
      // picks its manager, role, face and machine before it starts.
      launch: (target, agent) => {
        openStoryLaunchSheet(teamKey, target, agent).catch((err: unknown) => toastLaunchError(err));
      },
    };
  }, [ordered, availability, shortcut, teamKey]);
}

const StoryAgentLaunchContext = createContext<StoryAgentLaunchActions | null>(null);

/** Provide a team's "Launch agent" actions to the story surfaces below. */
export function StoryAgentLaunchProvider({
  teamKey,
  children,
}: {
  teamKey: string;
  children: ReactNode;
}) {
  const actions = useStoryAgentLaunch(teamKey);
  return (
    <StoryAgentLaunchContext.Provider value={actions}>{children}</StoryAgentLaunchContext.Provider>
  );
}

/** The nearest provider's "Launch agent" actions, or `null` (no menu shown). */
export function useStoryAgentLaunchActions(): StoryAgentLaunchActions | null {
  return useContext(StoryAgentLaunchContext);
}

/** Test/advanced seam: provide a fixed actions value. */
export const StoryAgentLaunchActionsProvider = StoryAgentLaunchContext.Provider;
