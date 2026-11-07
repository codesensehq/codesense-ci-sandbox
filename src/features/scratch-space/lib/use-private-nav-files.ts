// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";
import { listScratchSpace, type ScratchFile, type ScratchOverlayFile } from "@/tauri/commands";
import { parseTeamKey } from "@/shared/lib/team-key";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";

const NONE: ScratchFile[] = [];
export function usePrivateNavFiles(teamKey: string, contentPath: string | null) {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const refresh = useFileRefreshStore((s) =>
    contentPath ? (s.fileRefreshKeys[contentPath] ?? 0) : 0,
  );
  const [read, setRead] = useState<{ key: string; files: ScratchFile[] } | null>(null);
  const key = `${teamKey}:${contentPath}`;
  useEffect(() => {
    if (!contentPath) return;
    let live = true;
    listScratchSpace(orgSlug, teamName ?? undefined)
      .then((listing) => {
        if (live) setRead({ key, files: listing.files });
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [contentPath, key, orgSlug, teamName, refresh]);
  return useMemo(
    () =>
      new Map(
        (read?.key === key ? read.files : NONE).map((file) => [
          file.path,
          {
            ...file,
            origin: "scratch",
            replacesTeam: !!file.replacesTeam,
            usingTeam: false,
            orgSlug,
            team: teamName ?? "",
          } satisfies ScratchOverlayFile,
        ]),
      ),
    [read, key, orgSlug, teamName],
  );
}
