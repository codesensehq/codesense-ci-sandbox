// SPDX-License-Identifier: MIT
import { AgentFaceView } from "@/features/viewer/components/agent-identity";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { inboxSenderIdentity } from "@/features/weft/lib/inbox-sender-identity";

export function ViewerIdentityFace({
  size,
  className,
}: {
  size: number;
  className?: string;
}): React.JSX.Element | null {
  const currentUser = useCurrentUser();
  const { faces } = useAvatarPacks();
  if (!currentUser) return null;
  const identity = inboxSenderIdentity(
    "human",
    currentUser.display_name,
    undefined,
    {
      displayName: currentUser.display_name,
      face: currentUser.identity_avatar ?? null,
    },
    faces,
  );
  return <AgentFaceView face={identity.face} size={size} className={className} />;
}
