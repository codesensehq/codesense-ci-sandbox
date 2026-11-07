// SPDX-License-Identifier: MIT
import { useCallback } from "react";
import { ExternalLink } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import type { Org } from "@/shared/types/domain";
import { commitRegistryEdit } from "@/shared/lib/commit-registry-edit";
import { rowMatches } from "@/features/settings/lib/row-matches";
import { useFieldDraft } from "@/features/settings/lib/use-field-draft";
import { SettingRow } from "@/features/settings/components/setting-row";
import { PermissionGate } from "@/features/settings/components/permission-gate";
import { useCurrentUser } from "@/shared/lib/get-current-user";
import { findRowSpec } from "./lib/workspace-row-specs";

interface ConfigRowsProps {
  org: Org;
  query: string;
}

export function ConfigRows({ org, query }: ConfigRowsProps) {
  return (
    <>
      <NameRow org={org} query={query} />
      <RemoteRow org={org} query={query} />
    </>
  );
}

// ---------- Display name ----------

function NameRow({ org, query }: { org: Org; query: string }) {
  const spec = findRowSpec("workspace-name");
  const githubUserId = useCurrentUser()?.github_user_id ?? null;
  const commit = useCallback(
    async (next: string) => {
      const trimmed = next.trim();
      if (trimmed === "") {
        throw new Error("Workspace name required");
      }
      if (trimmed === org.display_name) {
        return;
      }
      if (githubUserId === null) {
        throw new Error("Not signed in");
      }
      // SW-OB-8: local-only commit regardless of `org.remote_url`.
      await commitRegistryEdit(githubUserId, org.slug, {
        type: "RenameWorkspace",
        new_display_name: trimmed,
      });
      toast.success("Workspace renamed.");
    },
    [org.display_name, org.slug, githubUserId],
  );

  const { draft, onChange, onBlur, onKeyDown, isCommitting } = useFieldDraft({
    committed: org.display_name,
    commit,
    label: "workspace name",
  });

  if (!rowMatches(spec, query)) return null;

  return (
    <SettingRow label={spec.label}>
      <PermissionGate
        required="admin"
        fallback={
          <p className="py-1 text-xs" data-testid="settings-workspace-name-readonly">
            {org.display_name}
          </p>
        }
      >
        <input
          type="text"
          value={draft}
          onChange={onChange}
          onBlur={onBlur}
          onKeyDown={onKeyDown}
          disabled={isCommitting}
          placeholder="Workspace name"
          aria-label="Workspace name"
          data-testid="settings-workspace-name-input"
          className="h-7 w-full rounded border border-border bg-background px-2 text-xs outline-none focus:border-ring disabled:opacity-60"
        />
      </PermissionGate>
    </SettingRow>
  );
}

// ---------- Remote URL ----------

function RemoteRow({ org, query }: { org: Org; query: string }) {
  const spec = findRowSpec("workspace-remote");
  if (!rowMatches(spec, query)) return null;

  if (org.remote_url === null) {
    return (
      <SettingRow label={spec.label}>
        <p className="text-xs text-muted-foreground">Local repository</p>
      </SettingRow>
    );
  }

  const display = displayRemote(org.remote_url);
  // Only http(s) origins open in a browser; an SSH origin is shown as text.
  if (!/^https?:\/\//i.test(org.remote_url)) {
    return (
      <SettingRow label={spec.label}>
        <p
          title={org.remote_url}
          data-testid="settings-workspace-remote-text"
          className="select-text text-xs text-foreground"
        >
          {display}
        </p>
      </SettingRow>
    );
  }
  return (
    <SettingRow label={spec.label}>
      <button
        type="button"
        onClick={() => {
          openUrl(org.remote_url!).catch(() => {});
        }}
        data-testid="settings-workspace-remote-link"
        className="inline-flex items-center gap-1 text-xs text-primary hover:underline"
      >
        {display}
        <ExternalLink className="h-3 w-3" />
      </button>
    </SettingRow>
  );
}

/** Pretty-print `https://github.com/owner/repo.git` (or
 *  `git@github.com:owner/repo.git`) → `github.com/owner/repo`; the raw URL when
 *  it doesn't parse. Purely cosmetic; never used to parse owner/repo (that's
 *  `parse_git_remote`'s job). */
function displayRemote(remoteUrl: string): string {
  const scp = /^(?:[^@/:]+@)?([^@/:[\]]{2,}):(?!\/)(.+)$/.exec(remoteUrl);
  if (scp) return `${scp[1]}/${scp[2].replace(/\.git$/i, "")}`;
  try {
    const u = new URL(remoteUrl);
    const path = u.pathname.replace(/\.git$/i, "").replace(/^\//, "");
    return `${u.host}/${path}`;
  } catch {
    return remoteUrl;
  }
}
