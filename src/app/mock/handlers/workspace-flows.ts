// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { Org } from "@/shared/types/domain";
import type { LoadedOrg } from "@/shared/types/registry";
import type {
  ConnectOrgResult,
  CreateWorkspaceResult,
  JoinRemoteClassification,
} from "@/tauri/commands";
import { MOCK_USER } from "../fixtures";

/**
 * SW-UX (app-flow redesign) — mock the create / join WORKSPACE contracts so the
 * Workspaces tab's flow (create local/shared, classify a join URL, pick a
 * connection, and complete a join) works end-to-end in `pnpm dev:mock` ahead of a
 * real backend. Returns realistic fixture shapes; no persistence.
 */

/** A minimal-but-valid LoadedOrg for a freshly created / joined workspace. */
function makeLoadedOrg(slug: string, displayName: string): LoadedOrg {
  const org: Org = {
    slug,
    display_name: displayName,
    created_at: "2026-01-01T00:00:00Z",
    remote_url: null,
    config_flags: {},
    people: [],
    // A freshly created workspace has ZERO teams (SW-OB-2 — no phantom default);
    // a joined one carries whatever the remote had — a single "docs" team reads fine.
    teams: [],
    org_root: `/mock/.codesense/users/${MOCK_USER.userId}/orgs/${slug}`,
  };
  return {
    slug,
    root: org.org_root,
    org,
    partial: { org_yml: null, people_yml: null, teams_yml: null },
    file_errors: [],
  };
}

/** Slugify a workspace display name into an `owner/name`-style org slug. */
function slugify(name: string): string {
  const base = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return `${MOCK_USER.username}/${base || "workspace"}`;
}

// ---------------------------------------------------------------------------
// SW-OC-B3 — mock the frozen `classify_join_remote_cmd` contract.
//
// SW-CR-2 retired the Rust `normalize_context_repo_url` + `classify_join_remote`
// path (CodeSense no longer clones context repos itself — SW-CR-1 adopted
// attach-from-anywhere). The FE still dispatches `classify_join_remote_cmd`
// via `retiredRepositoryAction`, so this mock preserves the pre-retirement
// classification shape (`JoinRemoteClassification` + `ConnectOrgResult`) so
// join-org UX built on `pnpm dev:mock` keeps compiling and exercising the
// flow. The shared conformance battery (`join-remote-conformance.ts`) pins
// the mock's accept/reject decisions. The forge table + normalization
// pipeline below are MINIMAL FE-local reimplementations — the Rust
// originals they used to shadow are gone.
// ---------------------------------------------------------------------------

/** FE-local git-forge transport table used by the mock classifier. */
const GIT_FORGE_HOSTS = [
  { host: "github.com", provider: "github", displayName: "GitHub", support: "supported" },
  { host: "gitlab.com", provider: "gitlab", displayName: "GitLab", support: "supported" },
  // bitbucket.org transports but has no OAuth provider yet → comingSoon (SW-OC-B2).
  { host: "bitbucket.org", provider: "bitbucket", displayName: "Bitbucket", support: "comingSoon" },
] as const;

const SUPPORTED_HOSTS_CSV = GIT_FORGE_HOSTS.map((f) => f.host).join(", ");

/** Typed rejection reasons used by the mock classifier. */
type NormalizeError =
  | { kind: "empty" }
  | { kind: "unsupportedForge"; url: string }
  | { kind: "missingOwnerRepo" }
  | { kind: "unexpectedPathSegments" }
  | { kind: "slugRejected"; field: "owner" | "repo" };

interface NormalizedRemote {
  /** Canonical `host/owner/repo`, lowercased. */
  normalizedUrl: string;
  host: string;
  owner: string;
  repo: string;
  /** Filesystem-safe `owner/repo` slug. */
  orgSlug: string;
}

/** Strip a single leading, case-insensitive `www.` from a host. */
function stripWwwPrefix(host: string): string {
  return host.length > 4 && host.slice(0, 4).toLowerCase() === "www." ? host.slice(4) : host;
}

/** The self-hosted forge family for a non-cloud host, if any. */
function selfHostedForgeFamily(host: string): string | null {
  const label = stripWwwPrefix(host).split(".")[0]?.toLowerCase() ?? "";
  if (label === "github") return "GitHub";
  if (label === "gitlab") return "GitLab";
  if (label === "bitbucket") return "Bitbucket";
  return null;
}

/** Build the self-hosted-aware / generic unsupported-forge copy. */
function unsupportedForgeMessage(rawUrl: string): string {
  const host = hostFromRawUrl(rawUrl);
  const family = selfHostedForgeFamily(host);
  if (family) return `Self-hosted ${family} Enterprise isn't supported yet.`;
  return `Only these git forges are supported: ${SUPPORTED_HOSTS_CSV}.`;
}

/** Map a NormalizeError to the user-facing message. */
function mapNormalizeError(err: NormalizeError): string {
  switch (err.kind) {
    case "empty":
      return "Workspace URL is required.";
    case "unsupportedForge":
      return unsupportedForgeMessage(err.url);
    case "missingOwnerRepo":
      return "URL is missing the owner/repo segments (expected: host/owner/repo).";
    case "unexpectedPathSegments":
      return "URL has extra path segments — paste the repository root URL, not a branch or file URL.";
    case "slugRejected":
      return `URL ${err.field} segment is invalid.`;
  }
}

const PROTOCOL_PREFIXES = ["git+ssh://", "https://", "http://", "ssh://", "git://"];

/** Strip leading protocol prefixes, looping (mirror of `strip_protocol`). */
function stripProtocol(s: string): string {
  let out = s;
  for (;;) {
    const match = PROTOCOL_PREFIXES.find((p) => out.slice(0, p.length).toLowerCase() === p);
    if (!match) break;
    out = out.slice(match.length);
  }
  return out;
}

/** Strip leading userinfo if `@` precedes first `/` (or no `/`) — mirror of Rust. */
function stripUserinfo(s: string): string {
  const at = s.indexOf("@");
  if (at === -1) return s;
  const slash = s.indexOf("/");
  const shouldStrip = slash === -1 || at < slash;
  return shouldStrip ? s.slice(at + 1) : s;
}

/** SSH shorthand `:` → `/` when `:` precedes first `/` (or no `/`) — mirror of Rust. */
function replaceSshColon(s: string): string {
  const colon = s.indexOf(":");
  if (colon === -1) return s;
  const slash = s.indexOf("/");
  const shouldReplace = slash === -1 || colon < slash;
  return shouldReplace ? `${s.slice(0, colon)}/${s.slice(colon + 1)}` : s;
}

/** Strip `?query` and `#fragment` (mirror of `strip_query_and_fragment`). */
function stripQueryAndFragment(s: string): string {
  let end = s.length;
  const q = s.indexOf("?");
  if (q !== -1) end = Math.min(end, q);
  const h = s.indexOf("#");
  if (h !== -1) end = Math.min(end, h);
  return s.slice(0, end);
}

/** Extract the bare host from a raw URL for the mock classifier. */
function hostFromRawUrl(url: string): string {
  let s = stripProtocol(url.trim());
  s = stripUserinfo(s);
  s = replaceSshColon(s);
  s = stripQueryAndFragment(s);
  const end = s.indexOf("/") === -1 ? s.length : s.indexOf("/");
  return stripWwwPrefix(s.slice(0, end)).toLowerCase();
}

/** Sanitize an owner/repo segment into a slug for the mock classifier. */
function sanitizeSlugSegment(segment: string): string | null {
  if (segment === "" || segment === "." || segment === "..") return null;
  if (!/^[A-Za-z0-9._-]+$/.test(segment)) return null;
  const first = segment[0];
  const last = segment[segment.length - 1];
  if (first === "." || first === "-" || last === "." || last === "-") return null;
  return segment.toLowerCase();
}

/** Normalize a context-repo remote URL for the mock classifier. */
function normalizeContextRepoUrl(url: string): NormalizedRemote | NormalizeError {
  const original = url;
  let s = url.trim();
  if (s === "") return { kind: "empty" };
  s = stripProtocol(s);
  s = stripUserinfo(s);
  s = replaceSshColon(s);
  s = stripQueryAndFragment(s);
  s = s.replace(/\/+$/, ""); // strip trailing slashes (before .git strip)
  s = s.replace(/\.git$/i, ""); // strip trailing .git

  const hostEnd = s.indexOf("/") === -1 ? s.length : s.indexOf("/");
  const rawHost = s.slice(0, hostEnd);
  const host = stripWwwPrefix(rawHost);
  const forge = GIT_FORGE_HOSTS.find((f) => f.host.toLowerCase() === host.toLowerCase());
  if (!forge) return { kind: "unsupportedForge", url: original };

  const segments = s.split("/");
  if (segments.length < 3) return { kind: "missingOwnerRepo" };
  if (segments.length > 3) return { kind: "unexpectedPathSegments" };
  const owner = segments[1];
  const repo = segments[2];
  if (owner === "" || repo === "") return { kind: "missingOwnerRepo" };

  const sanitizedOwner = sanitizeSlugSegment(owner);
  if (sanitizedOwner === null) return { kind: "slugRejected", field: "owner" };
  const sanitizedRepo = sanitizeSlugSegment(repo);
  if (sanitizedRepo === null) return { kind: "slugRejected", field: "repo" };

  return {
    normalizedUrl: `${host.toLowerCase()}/${owner.toLowerCase()}/${repo.toLowerCase()}`,
    host: host.toLowerCase(),
    owner,
    repo,
    orgSlug: `${sanitizedOwner}/${sanitizedRepo}`,
  };
}

/** Classify a pasted join URL — mock classifier for the retired contract. */
function classifyJoinRemote(remoteUrl: string): JoinRemoteClassification {
  const normalized = normalizeContextRepoUrl(remoteUrl);
  if ("kind" in normalized) {
    return { kind: "unsupported", message: mapNormalizeError(normalized) };
  }
  const forge = GIT_FORGE_HOSTS.find((f) => f.host === normalized.host);
  if (!forge) {
    return {
      kind: "unsupported",
      message: `No connection provider is registered for ${normalized.host}.`,
    };
  }
  if (forge.support === "comingSoon") {
    return { kind: "comingSoon", provider: forge.provider, provider_name: forge.displayName };
  }
  return {
    kind: "supported",
    provider: forge.provider,
    provider_name: forge.displayName,
    canonical_url: normalized.normalizedUrl,
    clone_url: `https://${normalized.host}/${normalized.owner}/${normalized.repo}`,
  };
}

/**
 * Parse `owner/repo` from a join URL for the mock join-completion path.
 * Reuses the mock normalizer so a joined workspace slug matches what the
 * classifier produced; falls back to a placeholder only for URLs the
 * classifier would already have rejected (the join UI never reaches
 * completion for those).
 */
function parseRemote(remoteUrl: string): { owner: string; repo: string } {
  const normalized = normalizeContextRepoUrl(remoteUrl);
  if ("kind" in normalized) return { owner: "owner", repo: "context-repo" };
  return { owner: normalized.owner, repo: normalized.repo };
}

export function workspaceFlowHandlers(): HandlerRegistry {
  return {
    // SW-XS-10: Stop on the operation chip. Nothing runs in the mock, so the
    // operation is reported as already finished.
    cancel_context_operation: () => false,
    // "Discard my edits": every named file is replaced with the team's version.
    discard_context_edits: (args) => ({
      status: "ready",
      written: Array.isArray(args.paths) ? args.paths.length : 0,
      keptModifiedCount: 0,
      failedCount: 0,
      failed: [],
    }),
    // Classify a pasted join URL (SW-OC-B3): strip a leading www.; accept
    // ONLY exact supported hosts; reject org-only / extra-segment /
    // self-hosted / unknown hosts; classify bitbucket.org as comingSoon.
    // The Rust `classify_join_remote` the mock used to shadow was retired by
    // SW-CR-2; the FE now dispatches this cmd via `retiredRepositoryAction`,
    // so the mock preserves the pre-retirement classification shape here.
    classify_join_remote_cmd: (args): JoinRemoteClassification =>
      classifyJoinRemote(String(args.remoteUrl ?? "")),

    // Complete a join — clone the context repo + load its registry. SW-FU-B1:
    // like the real clone, refuse a URL with no scheme (the classifier's
    // comparison key), so a form that sends the key fails here as it would
    // have in the shipped app.
    connect_to_org_cmd: (args): ConnectOrgResult => {
      const remoteUrl = String(args.remoteUrl ?? "");
      if (!/^(https?:\/\/|ssh:\/\/|git@|file:\/\/)/i.test(remoteUrl)) {
        throw {
          kind: "joinFailure",
          stage: "clone",
          message: "Invalid repository URL: unsupported scheme",
        };
      }
      const { owner, repo } = parseRemote(remoteUrl);
      const slug = `${owner}/${repo}`;
      return {
        org_slug: slug,
        org_root: `/mock/.codesense/users/${MOCK_USER.userId}/orgs/${slug}`,
        clone_outcome: "cloned",
        loaded_org: makeLoadedOrg(slug, repo),
      };
    },

    // SW-FU-B4 "Use my clone": adopt the picked folder as the team repo.
    adopt_org_clone_cmd: (args): ConnectOrgResult => {
      const { owner, repo } = parseRemote(String(args.remoteUrl ?? ""));
      const slug = `${owner}/${repo}`;
      return {
        org_slug: slug,
        org_root: `/mock/.codesense/users/${MOCK_USER.userId}/orgs/${slug}`,
        clone_outcome: "adopted",
        loaded_org: makeLoadedOrg(slug, repo),
      };
    },

    // SW-FU-B4: nothing is ever in the way in the mock.
    set_aside_org_target_cmd: (): string => "/mock/.codesense/set-aside/owner/repo-0",

    // Create a local/shared workspace — returns the loaded new org so the picker
    // registers + activates it (landing in the no-team state, SW-OB-2).
    create_workspace: (args): CreateWorkspaceResult => {
      const name = String(args.name ?? "Workspace");
      return { org: makeLoadedOrg(slugify(name), name) };
    },

    // No in-flight shared-create recovery records in the mock.
    list_pending_org_creates: () => ({}),
  };
}
