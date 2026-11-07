// SPDX-License-Identifier: MIT
import { listBuiltinPanes } from "@/tauri/commands";
import { parseTeamKey } from "@/shared/lib/parse-team-key";

export async function openStoryTrail({
  storyId,
  teamKey,
  navigate,
}: {
  storyId: string;
  teamKey: string;
  navigate: (path: string, options?: { openInNewTab?: boolean; hash?: string }) => void;
}) {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  if (!teamName) throw new Error("Select a team to open its story trail.");
  const listing = await listBuiltinPanes(orgSlug, teamName);
  const trail = listing.panes.find((pane) => pane.id === "story-trail");
  if (!trail) throw new Error("The bundled story trail could not be loaded. Try again.");
  navigate(trail.path, { openInNewTab: true, hash: `#story/${encodeURIComponent(storyId)}` });
}
