// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { FileWarning } from "lucide-react";

import { RowAction } from "@/features/shell/components/connections-reserve";
import { getErrorMessage } from "@/shared/lib/error-message";
import { parseTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import {
  listUnreadableMachineFiles,
  openUnreadableMachineFile,
  removeUnreadableMachineFile,
  type MachineSummary,
  type UnreadableMachineFile,
} from "@/tauri/commands";

import { reloadMachines } from "../stores/machines-store";

function fileName(path: string): string {
  return path.split("/").pop() || path;
}

/** The team's machine files that can't be read: named, so a machine never
 *  vanishes silently, with the file to repair or remove. Re-read whenever the
 *  team's machine list is (`machines` changes identity on every read). */
export function UnreadableMachineFiles({
  teamKey,
  machines,
}: {
  teamKey: string;
  machines: MachineSummary[];
}) {
  const [files, setFiles] = useState<UnreadableMachineFile[]>([]);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!teamKeyHasTeam(teamKey)) return;
    let live = true;
    const { orgSlug, teamName } = parseTeamKey(teamKey);
    listUnreadableMachineFiles(orgSlug, teamName).then(
      (next) => {
        if (live) setFiles(next);
      },
      () => {
        if (live) setFiles([]);
      },
    );
    return () => {
      live = false;
    };
  }, [teamKey, machines, revision]);

  if (files.length === 0) return null;
  return (
    <>
      {files.map((file) => (
        <UnreadableRow
          key={`${file.path}`}
          file={file}
          teamKey={teamKey}
          onRemoved={() => {
            setRevision((r) => r + 1);
            void reloadMachines(teamKey);
          }}
        />
      ))}
    </>
  );
}

function UnreadableRow({
  file,
  teamKey,
  onRemoved,
}: {
  file: UnreadableMachineFile;
  teamKey: string;
  onRemoved: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const name = fileName(file.path);
  const id = file.machineId;
  return (
    <li
      data-testid={`machine-unreadable-${id}`}
      className="flex items-center gap-3 rounded-md border border-amber-500/40 px-3 py-2"
    >
      <FileWarning size={14} aria-hidden="true" className="shrink-0 text-amber-500" />
      <div className="min-w-0 flex-1">
        <p className="text-sm">
          Can't read {name}: {file.error}
        </p>
        <p className="truncate text-xs text-muted-foreground" title={file.path}>
          {file.scope.kind === "team" ? `Shared with ${file.scope.team}` : "Only me"} · {file.path}
        </p>
      </div>
      {confirming ? (
        <div role="group" aria-label="Confirm remove" className="flex shrink-0 items-center gap-1">
          <span className="max-w-64 text-xs">
            {file.scope.kind === "team"
              ? `Delete ${name}? Its machine goes for everyone on ${file.scope.team}.`
              : `Delete ${name}? Its machine goes from all your teams.`}
          </span>
          <RowAction
            testid={`machine-unreadable-remove-yes-${id}`}
            label="Delete"
            destructive
            disabled={busy}
            onClick={() => {
              setBusy(true);
              removeUnreadableMachineFile(orgSlug, teamName, id).then(
                () => {
                  setBusy(false);
                  setConfirming(false);
                  onRemoved();
                },
                (e: unknown) => {
                  setBusy(false);
                  toast.error(`Couldn't remove ${name}: ${getErrorMessage(e)}`);
                },
              );
            }}
          />
          <RowAction
            testid={`machine-unreadable-remove-no-${id}`}
            label="Cancel"
            onClick={() => setConfirming(false)}
          />
        </div>
      ) : (
        <div className="flex shrink-0 items-center gap-1">
          <RowAction
            testid={`machine-unreadable-open-${id}`}
            label="Open file"
            onClick={() =>
              void openUnreadableMachineFile(orgSlug, teamName, id).catch((e: unknown) =>
                toast.error(`Couldn't open ${name}: ${getErrorMessage(e)}`),
              )
            }
          />
          <RowAction
            testid={`machine-unreadable-remove-${id}`}
            label="Remove"
            destructive
            onClick={() => setConfirming(true)}
          />
        </div>
      )}
    </li>
  );
}
