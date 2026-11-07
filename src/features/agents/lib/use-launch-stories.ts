// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";
import { useTeamContentContext } from "@/shared/hooks/use-team-content-context";
import { subscribeContext } from "@/shared/lib/context-query/subscribe-context";
import type { ContextQuery, ContextRow } from "@/shared/lib/context-query/types";
import { sourceRepoRemoteKey } from "@/shared/lib/source-repo-key";
import { parseTeamKey } from "@/shared/lib/team-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { queryContext } from "@/tauri/commands";

export type LaunchStory = { storyId: string; title: string };
type StoryRead = { key: string; stories: LaunchStory[]; loading: boolean; error?: string };

/** Read titles and all configured sources from the team's canonical content index. */
export function useLaunchStories(teamKey: string) {
  const context = useTeamContentContext(teamKey);
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const org = useOrgStore((s) => s.orgs.find((o) => o.slug === orgSlug));
  const sources = org?.teams.find((t) => t.name === teamName)?.source_repos;
  const sourceSlugs = useMemo(
    () =>
      sources?.flatMap((r) => [`${r.owner}/${r.repo}`.toLowerCase(), sourceRepoRemoteKey(r)]) ?? [],
    [sources],
  );
  const page = context.kind === "ready" ? context.source.teamContentPath : "";
  const key = JSON.stringify([teamKey, page, sourceSlugs]);
  const [read, setRead] = useState<StoryRead>({ key: "", stories: [], loading: true });
  useEffect(() => {
    if (!page) return;
    let disposed = false;
    let generation = 0;
    const query: ContextQuery = {
      where: {
        orgs: [orgSlug!],
        teams: [teamName!],
        layers: ["team"],
        kinds: ["story", "bug", "spike"],
      },
      select: ["story_id", "source_repo", "team"],
      limit: 10_000,
    };
    const stop = subscribeContext(
      query,
      async (update) => {
        const mine = ++generation;
        const commit = (stories: LaunchStory[], loading: boolean, error?: string) => {
          if (!disposed && mine === generation) setRead({ key, stories, loading, error });
        };
        if (!update.ok) {
          commit([], false, update.error);
          return;
        }
        commit([], true);
        if (update.result.indexing) return;
        try {
          let result = update.result;
          const rows: ContextRow[] = [...result.rows];
          while (result.nextCursor) {
            result = await queryContext({ ...query, cursor: result.nextCursor }, page);
            if (disposed || mine !== generation) return;
            if (result.indexing || result.generation !== update.result.generation) return;
            rows.push(...result.rows);
          }
          const seen = new Set<string>();
          const stories = rows.flatMap((row) => {
            const id = row.fields.story_id;
            if (
              typeof id !== "string" ||
              !id ||
              seen.has(id) ||
              !sourceSlugs.includes(String(row.fields.source_repo).trim().toLowerCase())
            )
              return [];
            seen.add(id);
            return [{ storyId: id, title: row.title ?? "" }];
          });
          commit(stories, false);
        } catch (error) {
          commit([], false, String(error));
        }
      },
      page,
    );
    return () => {
      disposed = true;
      stop();
    };
  }, [key, page, orgSlug, teamName, sourceSlugs]);
  if (context.kind === "unavailable") return { stories: [], loading: false, error: context.error };
  if (!page || read.key !== key) return { stories: [], loading: true };
  return read;
}
