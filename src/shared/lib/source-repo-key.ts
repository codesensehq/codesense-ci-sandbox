// SPDX-License-Identifier: MIT
/**
 * The canonical key for a source repo's attachment record: `host/owner/repo`,
 * lowercased. This is exactly how `attachmentsByRemote` is keyed (and how the
 * server tie-breaks one attachment per remote), so EVERY lookup must normalize
 * identically — a mismatched-case key silently misses. Kept in one place so the
 * normalization can never drift between call sites (SW-HB-3).
 */
export function sourceRepoRemoteKey(slug: {
  owner: string;
  repo: string;
  provider?: string;
  host?: string;
}): string {
  const provider = slug.provider?.trim().toLowerCase() || "github";
  const host = slug.host?.trim().replace(/\.+$/, "").toLowerCase();
  validateRepoSegment(slug.owner, "owner");
  validateRepoSegment(slug.repo, "repo");
  if (!host && provider !== "github") {
    throw new Error(`source repo host is required for provider '${provider}'`);
  }
  if (host && !isValidRepoHost(host)) {
    throw new Error(`source repo host is invalid for provider '${provider}'`);
  }
  return `${host || "github.com"}/${slug.owner.toLowerCase()}/${slug.repo.toLowerCase()}`;
}

function validateRepoSegment(segment: string, field: "owner" | "repo"): void {
  if (
    segment.length === 0 ||
    segment !== segment.trim() ||
    segment === "." ||
    segment === ".." ||
    segment.includes("/") ||
    segment.includes("\\") ||
    /\p{Cc}/u.test(segment)
  ) {
    throw new Error(`source repo ${field} is invalid`);
  }
}

function isValidRepoHost(host: string): boolean {
  if (host.length > 253 || /^\d+(?:\.\d+){3}$/.test(host)) return false;
  return host
    .split(".")
    .every(
      (label) =>
        label.length > 0 && label.length <= 63 && /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(label),
    );
}
