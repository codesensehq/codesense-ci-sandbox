// SPDX-License-Identifier: MIT
import { useMemo, useState } from "react";
import { CircleCheck } from "lucide-react";

import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Segmented } from "@/components/ui/segmented";
import { cn } from "@/lib/utils";
import { Spinner } from "@/shared/components/spinner";
import type { LaunchPhase, LaunchTarget, LaunchTargetKind } from "@/shared/lib/launch-target";

/**
 * The target picker and progress pill every "launch this agent" dialog shares: pick where
 * the agent works (an existing story, a repo branch, a new worktree or an existing
 * worktree) and watch it start. Launching itself is the dialog's job.
 */

/** The four target kinds, in picker order, with a short label. */
const TARGET_KINDS: readonly { kind: LaunchTargetKind; label: string }[] = [
  { kind: "story", label: "Story" },
  { kind: "repoBranch", label: "Repo + branch" },
  { kind: "newWorktree", label: "New worktree" },
  { kind: "existingWorktree", label: "Existing worktree" },
];

/** The per-kind field values a picker edits. */
export interface LaunchTargetFields {
  storyId: string;
  repo: string;
  branch: string;
  path: string;
}

/** Build the target from the picker's kind and fields (trimming is the pure layer's job). */
function buildTarget(kind: LaunchTargetKind, fields: LaunchTargetFields): LaunchTarget {
  switch (kind) {
    case "story":
      return { kind, storyId: fields.storyId };
    case "repoBranch":
    case "newWorktree":
      return { kind, repo: fields.repo, branch: fields.branch };
    case "existingWorktree":
      return { kind, path: fields.path };
  }
}

/** The picker's state: the chosen kind, its fields and the target they make. Story is the
 *  default kind. */
export function useLaunchTarget(initialKind: LaunchTargetKind = "story") {
  const [kind, setKind] = useState<LaunchTargetKind>(initialKind);
  const [fields, setFields] = useState<LaunchTargetFields>({
    storyId: "",
    repo: "",
    branch: "",
    path: "",
  });
  const target = useMemo(() => buildTarget(kind, fields), [kind, fields]);
  const setField = (key: keyof LaunchTargetFields, value: string) =>
    setFields((prev) => ({ ...prev, [key]: value }));
  return { kind, setKind, fields, setField, target };
}

/** A visible, non-frozen launch pill: a spinner while hydrating or starting, a check once
 *  it runs. `idle` and `error` render nothing (the dialog shows the error). */
export function LaunchProgress({ phase }: { phase: LaunchPhase }) {
  if (phase === "idle" || phase === "error") return null;
  const running = phase === "running";
  const label =
    phase === "hydrating"
      ? "Hydrating worktree…"
      : phase === "starting"
        ? "Starting session…"
        : "Running — see the agents list";
  return (
    <span
      data-testid="launch-progress"
      data-phase={phase}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs",
        running
          ? "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
          : "bg-muted text-muted-foreground",
      )}
    >
      {running ? <CircleCheck className="size-3.5" aria-hidden /> : <Spinner decorative />}
      {label}
    </span>
  );
}

/** One labelled text field for a target's input. */
function TargetField({
  id,
  label,
  placeholder,
  value,
  onChange,
  disabled,
}: {
  id: string;
  label: string;
  placeholder: string;
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
}) {
  return (
    <div className="space-y-1">
      <Label htmlFor={id} className="text-xs text-muted-foreground">
        {label}
      </Label>
      <Input
        id={id}
        data-testid={`launch-field-${id}`}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        className="h-8"
      />
    </div>
  );
}

/** The target kind radio and the chosen kind's fields. `newBranchBase` names the branch a
 *  new worktree starts from, once the dialog has looked it up for the chosen repo. */
export function LaunchTargetPicker({
  kind,
  fields,
  onKindChange,
  onFieldChange,
  disabled,
  newBranchBase,
  kinds = TARGET_KINDS.map((t) => t.kind),
}: {
  /** The kinds offered, in picker order; all four by default. */
  kinds?: readonly LaunchTargetKind[];
  kind: LaunchTargetKind;
  fields: LaunchTargetFields;
  onKindChange: (kind: LaunchTargetKind) => void;
  onFieldChange: (key: keyof LaunchTargetFields, value: string) => void;
  disabled: boolean;
  newBranchBase?: string | null;
}) {
  return (
    <>
      <div className="space-y-1.5">
        <Label className="text-xs text-muted-foreground">Target</Label>
        <Segmented
          aria-label="Target"
          value={kind}
          onValueChange={onKindChange}
          options={TARGET_KINDS.filter((t) => kinds.includes(t.kind)).map((t) => ({
            value: t.kind,
            label: t.label,
            testId: `launch-target-kind-${t.kind}`,
            disabled,
          }))}
        />
      </div>

      {kind === "story" && (
        <TargetField
          id="storyId"
          label="Story id"
          placeholder="TR-1"
          value={fields.storyId}
          onChange={(v) => onFieldChange("storyId", v)}
          disabled={disabled}
        />
      )}
      {(kind === "repoBranch" || kind === "newWorktree") && (
        <div className="grid grid-cols-2 gap-2">
          <TargetField
            id="repo"
            label="Repo (owner/repo)"
            placeholder="acme/api"
            value={fields.repo}
            onChange={(v) => onFieldChange("repo", v)}
            disabled={disabled}
          />
          <TargetField
            id="branch"
            label={kind === "newWorktree" ? "New branch" : "Branch"}
            placeholder="feat/my-branch"
            value={fields.branch}
            onChange={(v) => onFieldChange("branch", v)}
            disabled={disabled}
          />
          {kind === "newWorktree" && (
            <p
              data-testid="launch-new-branch-base"
              className="col-span-2 text-xs text-muted-foreground"
            >
              {newBranchBase ? (
                <>
                  from <span className="font-mono text-foreground">{newBranchBase}</span>
                </>
              ) : (
                "from the repo's local default branch"
              )}
              {" — nothing is fetched"}
            </p>
          )}
        </div>
      )}
      {kind === "existingWorktree" && (
        <TargetField
          id="path"
          label="Worktree path"
          placeholder="/Users/me/codesense-wt/…"
          value={fields.path}
          onChange={(v) => onFieldChange("path", v)}
          disabled={disabled}
        />
      )}
    </>
  );
}
