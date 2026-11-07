// SPDX-License-Identifier: MIT
/** Recognize private files wherever opened; this never constructs a private target. */
export function privateFile(
  path: string,
): { repoRoot: string; relative: string; contentRoot: string | null } | null {
  const match = /^(.*\/users\/[^/]+\/scratch)\/(orgs\/[^/]+\/[^/]+\/(.*)|all-orgs\/(.*))$/.exec(
    path,
  );
  if (!match) return null;
  const relative = match[3] ?? match[4];
  const content = /^(orgs\/[^/]+\/[^/]+\/content)(?:\/|$)/.exec(match[2]);
  return {
    repoRoot: match[1],
    relative,
    contentRoot: content ? `${match[1]}/${content[1]}` : null,
  };
}
