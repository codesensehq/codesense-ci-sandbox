// SPDX-License-Identifier: MIT
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { X } from "lucide-react";
import { AppButton as Button } from "@/shared/components/app-button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { PortaledSelect } from "@/components/ui/portaled-select";
import {
  createStory,
  previewNewStory,
  storyRepoChoices,
  type StoryDraft,
  type StoryValidationReport,
} from "@/tauri/commands";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { useStatusTaxonomyStore } from "@/shared/stores/status-taxonomy-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import {
  allStoryIds,
  defaultSourceRepo,
  listEpics,
  proposeStoryId,
  repoChoices,
  type EpicOption,
} from "../lib/story-draft";
import { useTeamRepoSlugs } from "../lib/use-team-repo-slugs";
import { StoryFindings } from "./story-findings";

const PREVIEW_DEBOUNCE_MS = 250;

export interface NewStoryDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The team's pipeline tree (epics + existing story ids). */
  tree: DerivedPipelineTree;
  /** Synthetic workspace key whose tree is refreshed after the write. */
  repoPath: string;
  /** Absolute epic dir to preselect (e.g. from an epic's context menu). */
  initialEpicDir?: string | null;
  /** Called with the new story's path after it is written. */
  onCreated?: (path: string) => void;
}

/**
 * "New story…" — a thin form over the canonical story format. Every edit is
 * rendered and run through `validate_story` (debounced) so blocking findings
 * show inline before anything is written; Create stays disabled until the
 * report is clean of blocking findings, and the backend re-validates on write.
 */
export function NewStoryDialog(props: NewStoryDialogProps) {
  // Remount per open so each session starts from a fresh proposal.
  return props.open ? <NewStoryForm {...props} /> : null;
}

function NewStoryForm({
  open,
  onOpenChange,
  tree,
  repoPath,
  initialEpicDir,
  onCreated,
}: NewStoryDialogProps) {
  const epics = useMemo(() => listEpics(tree), [tree]);
  const storyIds = useMemo(() => allStoryIds(tree), [tree]);
  const statuses = useStatusTaxonomyStore((s) => s.states);
  const teamRepos = useTeamRepoSlugs();

  const firstEpic: EpicOption | undefined =
    epics.find((e) => e.absoluteDirPath === initialEpicDir) ?? epics[0];

  const [epicDir, setEpicDir] = useState(firstEpic?.absoluteDirPath ?? "");
  const [title, setTitle] = useState("");
  const [storyId, setStoryId] = useState(() =>
    firstEpic ? proposeStoryId(tree, firstEpic.absoluteDirPath) : "",
  );
  // `null` until the author picks one: tracks the taxonomy's first state even
  // if the taxonomy hydrates after the form opens.
  const [pickedStatus, setStatus] = useState<string | null>(null);
  const status = pickedStatus ?? statuses[0]?.id ?? "";
  const [sourceRepo, setSourceRepo] = useState<string | null>(null);
  const [dependsOn, setDependsOn] = useState<string[]>([]);
  const [epicRepos, setEpicRepos] = useState<{ epicDir: string; repos: string[] } | null>(null);
  const [checked, setChecked] = useState<{
    draftKey: string;
    report: StoryValidationReport;
  } | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const epic = epics.find((e) => e.absoluteDirPath === epicDir);
  const reposForEpic = useMemo(
    () => (epicRepos?.epicDir === epicDir ? epicRepos.repos : []),
    [epicRepos, epicDir],
  );
  // The repo is preselected only when there is a single choice (see
  // `defaultSourceRepo`); with several, the author must pick one.
  const effectiveRepo = sourceRepo ?? defaultSourceRepo(reposForEpic, teamRepos);

  // Sync with the epic docs on disk: the epic's `repos:` (inherited from a
  // parent epic when it names none) drive the repo choices — the same lookup
  // the validator and the Set repo… fix use.
  const epicDocPath = epic?.mainDocPath;
  useEffect(() => {
    if (!epicDocPath) return;
    let cancelled = false;
    storyRepoChoices(epicDocPath)
      .then((repos) => {
        if (!cancelled) setEpicRepos({ epicDir, repos });
      })
      .catch(() => {
        if (!cancelled) setEpicRepos({ epicDir, repos: [] });
      });
    return () => {
      cancelled = true;
    };
  }, [epicDocPath, epicDir]);

  const draft: StoryDraft | null = epic
    ? {
        epicDir,
        title: title.trim(),
        storyId: storyId.trim(),
        status,
        sourceRepo: effectiveRepo,
        // A story never depends on itself (the id may be edited after picking).
        dependsOn: dependsOn.filter((d) => d !== storyId.trim()),
      }
    : null;
  // Nothing to check until the required fields are filled in.
  const draftKey = draft && draft.title && draft.storyId ? JSON.stringify(draft) : null;

  // Live validation: render + validate_story the would-be file, debounced.
  useEffect(() => {
    if (!draftKey) return;
    let cancelled = false;
    const handle = setTimeout(() => {
      previewNewStory(JSON.parse(draftKey) as StoryDraft)
        .then((preview) => {
          if (cancelled) return;
          setChecked({ draftKey, report: preview.validation });
          setPreviewError(null);
        })
        .catch((e: unknown) => {
          if (cancelled) return;
          setChecked(null);
          setPreviewError(e instanceof Error ? e.message : String(e));
        });
    }, PREVIEW_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      clearTimeout(handle);
    };
  }, [draftKey]);

  const repoOptions = useMemo(
    () => repoChoices(reposForEpic, teamRepos).map((r) => ({ value: r, label: r })),
    [reposForEpic, teamRepos],
  );

  const dependencyOptions = storyIds
    .filter((id) => id !== storyId.trim() && !dependsOn.includes(id))
    .map((id) => ({ value: id, label: id }));

  // Never create against a team-repo default the epic's repos would replace.
  const reposLoaded = !epicDocPath || epicRepos?.epicDir === epicDir;
  const report = checked?.report ?? null;
  // Create only against a report for exactly the current draft (never a stale
  // one from before the latest keystroke).
  const reportIsCurrent = !!checked && checked.draftKey === draftKey;
  const hasBlocking = report?.findings.some((f) => f.severity === "blocking") ?? true;
  const canCreate =
    !!draft &&
    !!draft.title &&
    !!draft.storyId &&
    !!draft.sourceRepo &&
    reposLoaded &&
    reportIsCurrent &&
    !hasBlocking &&
    !submitting;

  const changeEpic = (next: string) => {
    setEpicDir(next);
    setStoryId(proposeStoryId(tree, next));
    setSourceRepo(null);
  };

  const submit = async () => {
    if (!draft || !canCreate) return;
    setSubmitting(true);
    try {
      const created = await createStory(draft);
      useFileRefreshStore.getState().bumpFileRefresh(repoPath);
      toast.success(`Created ${draft.storyId}`);
      onCreated?.(created.path);
      onOpenChange(false);
    } catch (e) {
      setPreviewError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent data-testid="new-story-dialog" className="max-w-lg">
        <DialogHeader>
          <DialogTitle>New story</DialogTitle>
          <DialogDescription>
            Writes a story file into the epic. Problems are checked as you type.
          </DialogDescription>
        </DialogHeader>

        {epics.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            There are no epics yet. Create an epic (a folder with a README) first.
          </p>
        ) : (
          <form
            className="flex flex-col gap-3"
            onSubmit={(e) => {
              e.preventDefault();
              void submit();
            }}
          >
            <div className="flex flex-col gap-1">
              <Label htmlFor="new-story-title">Title</Label>
              <Input
                id="new-story-title"
                autoFocus
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder="What should this story deliver?"
              />
            </div>

            <div className="grid grid-cols-2 gap-3">
              <div className="flex flex-col gap-1">
                <Label id="new-story-epic-label">Epic</Label>
                <PortaledSelect
                  value={epicDir}
                  onValueChange={changeEpic}
                  ariaLabelledby="new-story-epic-label"
                  options={epics.map((e) => ({ value: e.absoluteDirPath, label: e.dirPath }))}
                />
              </div>
              <div className="flex flex-col gap-1">
                <Label htmlFor="new-story-id">Story id</Label>
                <Input
                  id="new-story-id"
                  value={storyId}
                  onChange={(e) => setStoryId(e.target.value)}
                />
              </div>
              <div className="flex flex-col gap-1">
                <Label id="new-story-repo-label">Repo</Label>
                <PortaledSelect
                  value={effectiveRepo}
                  onValueChange={setSourceRepo}
                  ariaLabelledby="new-story-repo-label"
                  placeholder="Choose a repo…"
                  options={repoOptions}
                />
              </div>
              <div className="flex flex-col gap-1">
                <Label id="new-story-status-label">Status</Label>
                <PortaledSelect
                  value={status}
                  onValueChange={setStatus}
                  ariaLabelledby="new-story-status-label"
                  options={statuses.map((s) => ({ value: s.id, label: s.label }))}
                />
              </div>
            </div>

            <div className="flex flex-col gap-1">
              <Label id="new-story-deps-label">Depends on</Label>
              {dependsOn.length > 0 && (
                <ul className="flex flex-wrap gap-1" aria-label="Dependencies">
                  {dependsOn.map((id) => (
                    <li
                      key={id}
                      className="flex items-center gap-1 rounded bg-muted px-1.5 py-0.5 text-xs"
                    >
                      {id}
                      <button
                        type="button"
                        aria-label={`Remove dependency ${id}`}
                        onClick={() => setDependsOn(dependsOn.filter((d) => d !== id))}
                      >
                        <X className="size-3" aria-hidden />
                      </button>
                    </li>
                  ))}
                </ul>
              )}
              <PortaledSelect
                value=""
                onValueChange={(id) => setDependsOn([...dependsOn, id])}
                ariaLabelledby="new-story-deps-label"
                placeholder={dependencyOptions.length ? "Add a dependency…" : "No other stories"}
                options={dependencyOptions}
                disabled={dependencyOptions.length === 0}
              />
            </div>

            <div aria-live="polite" className="min-h-5">
              {previewError ? (
                <p role="alert" className="text-xs text-destructive">
                  {previewError}
                </p>
              ) : report ? (
                <StoryFindings report={report} showWhenClean />
              ) : null}
            </div>

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
                disabled={submitting}
              >
                Cancel
              </Button>
              <Button type="submit" variant="default" disabled={!canCreate}>
                {submitting ? "Creating…" : "Create story"}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
