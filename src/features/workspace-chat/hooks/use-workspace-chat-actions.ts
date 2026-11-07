// SPDX-License-Identifier: MIT
import { useCallback, useMemo, useState } from "react";
import { toast } from "sonner";
import { useOrgStore } from "@/shared/stores/org-store";
import { workspaceReposForTeam } from "@/shared/lib/build-synthetic-repo";
import { unarchiveWorkspace, workspaceChatSend } from "@/tauri/commands";
import { exportTranscript, type ExportTranscriptOptions } from "../lib/export-transcript";
import type { WorkspaceChatHandle } from "./use-workspace-chat";

/** Shared actions for the room wherever the human opens it. */
export function useWorkspaceChatActions(
  workspaceId: string,
  displayName: string,
  teamKey: string,
  chat: WorkspaceChatHandle,
) {
  const [sendError, setSendError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);

  const send = useCallback(
    async (text: string) => {
      setSendError(null);
      setSending(true);
      try {
        await workspaceChatSend(workspaceId, text);
        // The engine's own `codesense:workspace-chat-updated` event will nudge
        // the hook to re-read and render the message — no optimistic insert.
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setSendError(message);
        throw err;
      } finally {
        setSending(false);
      }
    },
    [workspaceId],
  );

  const unarchive = useCallback(async () => {
    try {
      await unarchiveWorkspace(workspaceId);
      // Store mirror picks up the change via `codesense:workspace-archive-changed`.
    } catch (err) {
      toast(`Couldn't unarchive: ${err instanceof Error ? err.message : String(err)}`);
    }
  }, [workspaceId]);

  const workspaceRepos = useOrgStore((s) => s.workspaceRepos);
  const teamContentPath = useMemo(
    () => workspaceReposForTeam(workspaceRepos, teamKey)[0]?.teamContentPath ?? "",
    [workspaceRepos, teamKey],
  );

  const exportFn = useCallback(async () => {
    if (!chat.messages) return;
    if (!teamContentPath) {
      toast("Can't export: no team content path is active.");
      return;
    }
    const options: ExportTranscriptOptions = {
      workspaceId,
      displayName,
      teamContentPath,
      messages: chat.messages,
    };
    try {
      const path = await exportTranscript(options);
      toast(`Exported transcript to ${path}`);
    } catch (err) {
      toast(`Couldn't export: ${err instanceof Error ? err.message : String(err)}`);
    }
  }, [chat.messages, displayName, teamContentPath, workspaceId]);

  return { sendError, sending, send, unarchive, exportFn, dismissError: () => setSendError(null) };
}
