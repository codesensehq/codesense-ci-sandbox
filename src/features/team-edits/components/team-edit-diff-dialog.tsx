// SPDX-License-Identifier: MIT
import { useEffect, useState, type RefObject } from "react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { CodeDiffReview } from "@/shared/components/code-editor";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  readFile,
  sourceRemoteFileRead,
  type TeamFileEdit,
  type TeamFileEditsWorkspace,
} from "@/tauri/commands";

import { baseName, dirName } from "../lib/team-edits";

type Side = { text: string } | { missing: true } | { binary: true } | { error: string };

function looksBinary(text: string): boolean {
  return text.includes("\u0000");
}

async function readSide(read: () => Promise<string>): Promise<Side> {
  try {
    const text = await read();
    return looksBinary(text) ? { binary: true } : { text };
  } catch (e) {
    const message = getErrorMessage(e);
    // A file that isn't there is a side of the comparison, not a failure.
    return /no such file|not found|does not exist|ENOENT/i.test(message)
      ? { missing: true }
      : { error: message };
  }
}

function decodeBase64(base64: string): string {
  const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

/** The workspace's copy of `file`: on This Mac, or read on its machine. */
function readWorkspaceSide(ws: TeamFileEditsWorkspace, file: TeamFileEdit): Promise<Side> {
  if (file.kind === "removed") return Promise.resolve({ missing: true });
  if (ws.target.kind === "local") {
    const root = ws.target.codeRepoPath.replace(/\/+$/, "");
    return readSide(() => readFile(`${root}/${file.path}`));
  }
  const { machineId, workspaceHandle } = ws.target;
  return readSide(async () =>
    decodeBase64((await sourceRemoteFileRead(machineId, workspaceHandle, file.path)).base64),
  );
}

function sideText(side: Side | null): string {
  return side && "text" in side ? side.text : "";
}

/** Tall enough for the longer side (about 20px a line), never past the dialog. */
function diffHeight(team: Side | null, mine: Side | null): string {
  const lines = Math.max(sideText(team).split("\n").length, sideText(mine).split("\n").length);
  return `min(60vh, ${Math.max(120, lines * 20 + 48)}px)`;
}

function sideNote(side: Side | null, which: "team" | "workspace"): string | null {
  if (!side) return null;
  if ("missing" in side)
    return which === "team"
      ? "The team has no copy of this file."
      : "This workspace no longer has the file.";
  if ("binary" in side) return "Not a text file: no line-by-line view.";
  if ("error" in side)
    return `Couldn't read the ${which === "team" ? "team's" : "workspace's"} copy: ${side.error}`;
  return null;
}

/**
 * SW-XS-27 "View": the team's copy beside this workspace's, side by side. Read
 * only — it never writes; Publish and Discard live in the list. For a file
 * changed here AND in the team, the team side is the team's version now.
 */
export function TeamEditDiffDialog({
  workspace,
  file,
  onClose,
  returnFocusTo,
}: {
  workspace: TeamFileEditsWorkspace;
  file: TeamFileEdit | null;
  onClose: () => void;
  /** Where focus goes when the dialog closes (the control that opened it may be gone). */
  returnFocusTo?: RefObject<HTMLElement | null>;
}) {
  const [team, setTeam] = useState<Side | null>(null);
  const [mine, setMine] = useState<Side | null>(null);

  useEffect(() => {
    if (!file) return;
    // A new file remounts the dialog (its `key`), so both sides start unread.
    let cancelled = false;
    void readSide(() => readFile(file.teamPath)).then((s) => !cancelled && setTeam(s));
    void readWorkspaceSide(workspace, file).then((s) => !cancelled && setMine(s));
    return () => {
      cancelled = true;
    };
  }, [workspace, file]);

  const ready = team !== null && mine !== null;
  const notes = [sideNote(team, "team"), sideNote(mine, "workspace")].filter(Boolean);
  const conflict = file?.kind === "conflict";
  return (
    <Dialog open={!!file} onOpenChange={(open) => (open ? undefined : onClose())}>
      <DialogContent
        className="flex max-h-[min(80vh,720px)] w-[min(92vw,1100px)] max-w-none flex-col gap-3 sm:max-w-none"
        data-testid="team-edit-diff"
        onCloseAutoFocus={(e) => {
          if (!returnFocusTo?.current) return;
          e.preventDefault();
          returnFocusTo.current.focus();
        }}
      >
        <DialogHeader className="pr-8">
          <DialogTitle className="truncate text-[13px] font-semibold" title={file?.path}>
            {file ? baseName(file.path) : ""}
            {file && dirName(file.path) && (
              <span className="ml-2 font-normal text-muted-foreground">{dirName(file.path)}</span>
            )}
          </DialogTitle>
          <DialogDescription className="text-[12px]">
            {conflict
              ? "Both changed since the file was copied."
              : "What changed in this workspace."}
          </DialogDescription>
        </DialogHeader>
        {notes.length > 0 && (
          <p className="text-[12px] text-muted-foreground" role="status">
            {notes.join(" ")}
          </p>
        )}
        <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-md border border-border">
          <div
            className="grid flex-none grid-cols-2 border-b border-border text-[11.5px] font-semibold"
            data-testid="team-edit-diff-sides"
          >
            <span className="px-3 py-1.5">
              {conflict ? "Team's version now" : "Team's version"}
            </span>
            <span className="border-l border-border px-3 py-1.5">
              {workspace.name}&rsquo;s version
            </span>
          </div>
          {ready ? (
            <CodeDiffReview
              before={sideText(team)}
              after={sideText(mine)}
              path={file?.path}
              mode="split"
              height={diffHeight(team, mine)}
              ariaLabel={`Team and workspace versions of ${file?.path ?? "the file"}`}
            />
          ) : (
            <div
              className="flex h-full items-center justify-center text-[12px] text-muted-foreground"
              role="status"
            >
              Reading both versions…
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
