// SPDX-License-Identifier: MIT
import { useMemo } from "react";
import {
  numericPrincipal,
  useLocalProfileStore,
  type LocalProfile,
} from "@/shared/stores/local-profile-store";
import type { CurrentUser } from "@/shared/types/domain";

/** Legacy numeric fields are storage adapters, never evidence of provider authentication. */
export function buildCurrentUser(profile: LocalProfile | null): CurrentUser | null {
  if (!profile) return null;
  return {
    principal: profile.principal,
    github_user_id: numericPrincipal(profile.principal),
    github_username: profile.displayName,
    display_name: profile.displayName,
    avatar_url: null,
    identity_avatar: profile.identityAvatar ?? null,
  };
}
export function getCurrentUser(): CurrentUser | null {
  return buildCurrentUser(useLocalProfileStore.getState().status?.active ?? null);
}
export function currentGithubUserId(): number {
  return getCurrentUser()?.github_user_id ?? 0;
}
export function useCurrentUser(): CurrentUser | null {
  const profile = useLocalProfileStore((s) => s.status?.active ?? null);
  return useMemo(() => buildCurrentUser(profile), [profile]);
}
