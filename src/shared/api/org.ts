// SPDX-License-Identifier: MIT
import type { CredentialRef, OrgSummary } from "@/shared/types/domain";
import {
  adoptOrgCloneCmd,
  connectToOrgCmd,
  setAsideOrgTargetCmd,
  createWorkspaceCmd,
  discoverLocalOrgsCmd,
  type ConnectOrgResult,
  type CreateWorkspaceResult,
  type SharedRemoteIntent,
} from "@/tauri/commands";

export async function discoverLocalOrgs(githubUserId: number): Promise<OrgSummary[]> {
  return discoverLocalOrgsCmd(githubUserId);
}

/**
 * Create a workspace. `name` is the user-chosen workspace display name
 * (drives both `org.yml:display_name` and the on-disk slug via Rust
 * `sanitize_name`). `githubDisplayName` is a separate concept — it's
 * the current user's display name seeded into `people.yml[0]`. These
 * two fields must not be conflated.
 *
 * Remote-backed creation requires `sharedRemote`, which always names the
 * exact connection used for forge calls and git authentication.
 */
export async function createWorkspace(
  githubUserId: number,
  name: string,
  githubUsername: string,
  githubDisplayName: string,
  avatarUrl: string | null,
  remoteUrl: string | null = null,
  sharedRemote: SharedRemoteIntent | null = null,
): Promise<CreateWorkspaceResult> {
  const result = await createWorkspaceCmd(
    githubUserId,
    name,
    githubUsername,
    githubDisplayName,
    avatarUrl,
    remoteUrl,
    sharedRemote,
  );
  return result;
}

/**
 * Connect to an existing workspace by cloning its context repo.
 * The Rust command resolves GitHub credentials internally from
 * AuthManagerState — the token never crosses the IPC boundary.
 */
export async function connectToOrg(
  remoteUrl: string,
  githubUserId: number,
  credentialRef: CredentialRef,
): Promise<ConnectOrgResult> {
  return connectToOrgCmd(remoteUrl, githubUserId, credentialRef);
}

/** "Use my clone": join from the user's existing clone at `clonePath` instead of cloning. */
export async function adoptOrgClone(
  remoteUrl: string,
  clonePath: string,
  githubUserId: number,
  credentialRef: CredentialRef,
): Promise<ConnectOrgResult> {
  return adoptOrgCloneCmd(remoteUrl, clonePath, githubUserId, credentialRef);
}

/** Move whatever is in the way of a team's folder aside so the join can proceed. */
export async function setAsideOrgTarget(remoteUrl: string, githubUserId: number): Promise<string> {
  return setAsideOrgTargetCmd(remoteUrl, githubUserId);
}
