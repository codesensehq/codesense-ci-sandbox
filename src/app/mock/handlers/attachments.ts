// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "../mock-router";
import type { ParsedGitRemote } from "@/tauri/commands";
import type { Attachment } from "@/shared/types/domain";
import { mockSourceRepoSlug } from "../fixtures";
import type { MockScenario } from "../scenario";

/** Where the mock pretends the team's single source repo is cloned. Any
 *  non-empty path flips `isCloneAttached` (localPath != null), so the exact
 *  value only needs to be stable and plausible. */
const MOCK_CLONE_PATH = "/mock/clones/acme/widgets";
const MOCK_REMOTE_URL = "https://github.com/acme/widgets.git";

/** Parse a git remote URL into its host/provider/owner/repo. Handles the
 *  https and scp-style ssh forms; unrecognized input falls back to the mock
 *  slug so the attach dialogs never hard-fail in mock mode. */
function parseRemote(url: string): ParsedGitRemote {
  const trimmed = url.trim().replace(/\.git$/, "");
  let host = "github.com";
  let path = trimmed;

  const ssh = trimmed.match(/^git@([^:]+):(.+)$/);
  const https = trimmed.match(/^https?:\/\/([^/]+)\/(.+)$/);
  if (ssh) {
    [, host, path] = ssh;
  } else if (https) {
    [, host, path] = https;
  }

  const segments = path.split("/").filter(Boolean);
  const owner = segments.length >= 2 ? segments[segments.length - 2] : mockSourceRepoSlug.owner;
  const repo = segments.length >= 1 ? segments[segments.length - 1] : mockSourceRepoSlug.repo;
  const provider = host.includes("gitlab")
    ? "gitlab"
    : host.includes("bitbucket")
      ? "bitbucket"
      : "github";
  return { host, provider, owner, repo };
}

/**
 * Source-repo attachment probes (W.2 / F-6 / SW-OB-3). The scenario's
 * `cloneAttached` decides whether the team's one source repo resolves to an
 * on-disk clone: when true, `list_attachments_by_remote` returns an attachment
 * keyed by lowercase `"owner/repo"` whose `repo_path` becomes the synthetic
 * repo's `localPath` (past the NoAttachedClone rung → the viewer opens); when
 * false, the map is empty (the rung renders). `parse_git_remote` and
 * `read_source_repo_origin_url` back the attach dialogs regardless.
 */
export function attachmentHandlers(scenario: MockScenario): HandlerRegistry {
  const remoteKey = `${mockSourceRepoSlug.owner}/${mockSourceRepoSlug.repo}`.toLowerCase();
  const attachment: Attachment = {
    repo_path: MOCK_CLONE_PATH,
    remote_url: MOCK_REMOTE_URL,
    attached_at: "2026-01-01T00:00:00Z",
    tracking_prompt_dismissed_at: null,
    credential_ref: null,
  };
  const attached = scenario.cloneAttached;

  return {
    parse_git_remote: (args) => parseRemote(String(args.url ?? MOCK_REMOTE_URL)),
    read_source_repo_origin_url: () => MOCK_REMOTE_URL,
    list_attachments_by_remote: () => (attached ? { [remoteKey]: attachment } : {}),
    resolve_source_repo_attachment: () => (attached ? attachment : null),
    save_source_repo_attachment: () => undefined,
    // The folder picker (`@tauri-apps/plugin-dialog` open) is inert by default in
    // mock mode; return a plausible folder so the "attach a local folder" path is
    // clickable end-to-end without a native dialog.
    "plugin:dialog|open": () => MOCK_CLONE_PATH,
  };
}
