// SPDX-License-Identifier: MIT
import type { InboxTeam } from "@/tauri/commands";
import { NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { QueueRole } from "@/shared/components/queue-role";
import { useInboxView } from "./inbox-view-context";

export function InboxRole({
  role,
  iconOnly = false,
  nameOnly = false,
  size,
  team,
}: {
  role: string;
  iconOnly?: boolean;
  nameOnly?: boolean;
  size?: number;
  team?: InboxTeam | null;
}) {
  const { fallbackTeam, roleTeam, roles } = useInboxView();
  const owner = team !== undefined ? team : (roleTeam ?? roles.find((r) => r.role === role)?.team);
  const resolved = owner === undefined ? fallbackTeam : owner;
  const teamKey = resolved ? `${resolved.org}:${resolved.team}` : NO_TEAM_SENTINEL;
  return (
    <QueueRole role={role} teamKey={teamKey} iconOnly={iconOnly} nameOnly={nameOnly} size={size} />
  );
}
