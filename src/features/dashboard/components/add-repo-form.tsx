// SPDX-License-Identifier: MIT
import { useState } from "react";
import { toast } from "sonner";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { FolderOpen, Link2 } from "lucide-react";
import { AppButton as Button } from "@/shared/components/app-button";
import { Input } from "@/components/ui/input";
import { InlineAlert } from "@/components/ui/inline-alert";
import { Label } from "@/components/ui/label";
import { cn } from "@/lib/utils";
import { useOrgStore } from "@/shared/stores/org-store";
import { getCurrentUser } from "@/shared/lib/get-current-user";
import {
  addSourceRepoToTeam,
  isAddRepoError,
  type AddRepoInput,
  type TrackedSourceRepo,
} from "@/tauri/commands";
import { AttachSuccessNotice } from "@/features/attachment/components/attach-success-notice";

type Mode = "local" | "url";

/**
 * SW-UX (app-flow redesign) — the reusable "add a source repo" body (single
 * column, no Dialog wrapper), so the SAME flow serves both the standalone attach
 * dialog and the one-modal Manage-repos surface (rendered in its reserve area) —
 * no modal-over-modal, no duplicated logic.
 *
 * Two dead-simple paths: attach a local folder (no URL), or add from a URL (pick a
 * code-host account via the shared `ConnectionPicker`). SW-UX: to author /
 * authenticate a NEW connection, the picker's "Add" opens the **Connections tab** —
 * no inline device-flow panel. The picker reads the auth store reactively, so a
 * newly-connected account appears here on return; selection is controlled via
 * `chosenAccountKey` / `onChosenAccountChange`.
 */
export function AddRepoForm({
  target,
  onDone,
}: {
  /** The team this repo attaches to. */
  target: { orgSlug: string; teamName: string };
  /** Called after a repo is successfully tracked/attached. `local` means a
   *  folder was attached and the form now shows its success state (SW-LN-29),
   *  so a host dialog should stay open for it. */
  onDone: (added?: "local" | "url") => void;
}) {
  const [mode, setMode] = useState<Mode>("local");
  const [folder, setFolder] = useState<string | null>(null);
  const [url, setUrl] = useState("");
  const [submitting, setSubmitting] = useState(false);
  // What failed and why: the reason is read through the user-facing path when shown.
  const [error, setError] = useState<{ title: string; reason: unknown } | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  // SW-LN-29 (AC-1): the local folder just attached (`owner/repo`); shows
  // where its context lands.
  const [attached, setAttached] = useState<string | null>(null);

  /** Add `input` to the target team. The rules (URL forms, origin read,
   *  `local/<folder>` fallback, duplicate refusal, binding the folder) all live
   *  in the Rust core; this only reflects the result. Returns null when the team
   *  already tracks the repo. */
  const addToTeam = async (input: AddRepoInput): Promise<TrackedSourceRepo | null> => {
    const currentUser = getCurrentUser();
    if (!currentUser) {
      setError({
        title: "Couldn't add the repo",
        reason: "Choose a local profile and try again.",
      });
      return null;
    }
    try {
      const added = await addSourceRepoToTeam({
        githubUserId: currentUser.github_user_id,
        orgSlug: target.orgSlug,
        teamName: target.teamName,
        input,
      });
      useOrgStore.getState().updateTeamSourceRepos(target.orgSlug, target.teamName, {
        owner: added.owner,
        repo: added.repo,
        provider: added.provider,
        host: added.host,
      });
      return added;
    } catch (e) {
      if (isAddRepoError(e) && e.kind === "alreadyTracked") {
        setInfo("Already tracked in this team.");
        return null;
      }
      if (isAddRepoError(e) && e.kind === "unknownTeam") {
        setError({
          title: "Couldn't add the repo",
          reason: "The team no longer exists. Close this tab and try again.",
        });
        return null;
      }
      throw e;
    }
  };

  const handlePickFolder = async () => {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title: "Choose a repo folder",
      });
      if (typeof picked === "string") {
        setFolder(picked);
        setError(null);
        setInfo(null);
        setAttached(null);
      }
    } catch (e) {
      setError({ title: "Couldn't open the folder picker", reason: e });
    }
  };

  const handleAttachLocal = async () => {
    if (!folder) return;
    setError(null);
    setInfo(null);
    setAttached(null);
    setSubmitting(true);
    try {
      const added = await addToTeam({ kind: "localPath", path: folder });
      if (!added) return;
      useOrgStore.getState().bumpAttachmentsVersion();
      const label = `${added.owner}/${added.repo}`;
      toast.success(`Attached ${label}`);
      setAttached(label);
      setFolder(null);
      onDone("local");
    } catch (e) {
      setError({ title: "Couldn't attach the folder", reason: e });
    } finally {
      setSubmitting(false);
    }
  };

  const handleAddUrl = async () => {
    const input = url.trim();
    if (!input || info) return;
    setError(null);
    setInfo(null);
    setSubmitting(true);
    try {
      const added = await addToTeam({ kind: "url", url: input });
      if (!added) return;
      toast.success(`Tracked ${added.owner}/${added.repo}`);
      onDone("url");
    } catch (e) {
      setError({ title: "Couldn't add the repo", reason: e });
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div
        role="tablist"
        aria-label="How to add a repo"
        className="grid grid-cols-2 gap-2 rounded-lg bg-muted p-1"
      >
        <button
          type="button"
          role="tab"
          aria-selected={mode === "local"}
          onClick={() => {
            setMode("local");
            setError(null);
            setInfo(null);
          }}
          className={cn(
            "flex items-center justify-center gap-1.5 rounded-md px-3 py-1.5 text-sm",
            mode === "local"
              ? "bg-background font-medium shadow-sm"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          <FolderOpen size={14} />
          Local folder
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={mode === "url"}
          onClick={() => {
            setMode("url");
            setError(null);
            setInfo(null);
            setAttached(null);
          }}
          className={cn(
            "flex items-center justify-center gap-1.5 rounded-md px-3 py-1.5 text-sm",
            mode === "url"
              ? "bg-background font-medium shadow-sm"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          <Link2 size={14} />
          From a URL
        </button>
      </div>

      {mode === "local" ? (
        <div className="flex flex-col gap-3">
          <p className="text-xs text-muted-foreground">
            Choose a folder that's already on your machine. No URL needed — CodeSense reads its git
            origin if there is one.
          </p>
          <div className="flex items-center gap-2">
            <Button variant="outline" size="sm" onClick={handlePickFolder} disabled={submitting}>
              <FolderOpen size={14} className="mr-1.5" />
              Choose folder…
            </Button>
            {folder && (
              <code
                className="min-w-0 flex-1 truncate rounded bg-muted px-2 py-1 text-xs"
                title={folder}
              >
                {folder}
              </code>
            )}
          </div>
          {info && <p className="text-xs text-blue-600 dark:text-blue-400">{info}</p>}
          {error && <InlineAlert severity="error" title={error.title} error={error.reason} />}
          {attached && <AttachSuccessNotice label={attached} managed={false} />}
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-2">
            <Label htmlFor="github-url">Repository URL or owner/repo</Label>
            <Input
              id="github-url"
              placeholder="https://github.com/owner/repo"
              value={url}
              onChange={(e) => {
                setUrl(e.target.value);
                setInfo(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleAddUrl();
              }}
              disabled={submitting}
              autoFocus
            />
          </div>
          <p className="text-xs text-muted-foreground">
            This records the repository identity locally. Prepare its checkout with your agent or
            CLI, then attach the folder.
          </p>
          {info && <p className="text-xs text-blue-600 dark:text-blue-400">{info}</p>}
          {error && <InlineAlert severity="error" title={error.title} error={error.reason} />}
        </div>
      )}

      <div className="flex justify-end">
        {mode === "local" ? (
          <Button onClick={handleAttachLocal} disabled={!folder || submitting}>
            {submitting ? "Attaching…" : "Attach folder"}
          </Button>
        ) : (
          <Button onClick={handleAddUrl} disabled={!url.trim() || submitting || !!info}>
            {submitting ? "Adding…" : "Add repo"}
          </Button>
        )}
      </div>
    </div>
  );
}
