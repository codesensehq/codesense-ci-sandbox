// SPDX-License-Identifier: MIT
import { useState } from "react";
import { useRoleConfigs } from "@/shared/hooks/use-role-configs";
import { useTeamAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { rolePresentation, defaultRoleIcon } from "@/shared/lib/role-presentation";
import { BUILTIN_FACE_SOURCE } from "@/shared/lib/avatar-packs";

export function QueueRole({
  role,
  teamKey,
  size = 16,
  iconOnly = false,
  nameOnly = false,
}: {
  role: string;
  teamKey?: string;
  size?: number;
  iconOnly?: boolean;
  nameOnly?: boolean;
}) {
  const { roles, teamKey: key } = useRoleConfigs(teamKey);
  const avatars = useTeamAvatarPacks([key])(key);
  const presentation = rolePresentation(role, roles, avatars.faces);
  const [failed, setFailed] = useState<string | null>(null);
  return (
    <span
      className="queue-role"
      title={presentation.title}
      aria-label={presentation.title}
      aria-hidden={iconOnly || undefined}
    >
      {!nameOnly && (
        <span className="queue-role-icon" style={{ width: size, height: size }} aria-hidden>
          <img
            src={
              failed === presentation.src
                ? BUILTIN_FACE_SOURCE.resolve(defaultRoleIcon(role))!
                : presentation.src
            }
            alt=""
            onError={() => setFailed(presentation.src)}
          />
        </span>
      )}
      {!iconOnly && (
        <span className={presentation.configured ? undefined : "font-mono"}>
          {presentation.name}
        </span>
      )}
    </span>
  );
}
