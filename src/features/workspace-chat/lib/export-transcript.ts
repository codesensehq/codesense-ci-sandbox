// SPDX-License-Identifier: MIT
import type { WorkspaceChatMessage } from "@/tauri/commands";
import { writeContentFile } from "@/tauri/commands";

/**
 * SW-WC-3 (AC-8) — write a workspace chat's transcript to a markdown file in
 * the team's content tree. v1 blocking: the archive banner's "Export
 * transcript" button awaits this.
 *
 * The transcript is a plain-markdown render of the log — the inbox renderer
 * handles rich bodies in the pane, but a transcript must be portable (opened
 * in any editor). We fence code identically, keep author handles on their own
 * line, and stamp `[system]` / `[you]` prefixes so a reader can scan quickly.
 */
export interface ExportTranscriptOptions {
  workspaceId: string;
  displayName?: string | null;
  teamContentPath: string;
  messages: readonly WorkspaceChatMessage[];
  /** The HUMAN sentinel from the engine (`"human"`). */
  humanSentinel?: string;
}

export const DEFAULT_HUMAN_SENTINEL = "human";

/** Build the markdown body. Pure — testable without Tauri. */
export function renderTranscript(options: ExportTranscriptOptions): string {
  const human = options.humanSentinel ?? DEFAULT_HUMAN_SENTINEL;
  const name = options.displayName?.trim() || options.workspaceId;
  const now = new Date().toISOString();
  const lines: string[] = [];
  lines.push(`# Workspace chat — ${name}`);
  lines.push("");
  lines.push(`- Workspace id: \`${options.workspaceId}\``);
  lines.push(`- Exported: ${now}`);
  lines.push(`- Messages: ${options.messages.length}`);
  lines.push("");
  lines.push("---");
  lines.push("");

  for (const msg of options.messages) {
    const timestamp = new Date(msg.createdAtMs).toISOString();
    const isSystem = msg.kind === "system";
    const isHuman = msg.from === human;
    const prefix = isSystem ? "[system]" : isHuman ? "[you]" : `@${msg.fromName || msg.from}`;
    const header = isSystem
      ? `**${prefix}** · _${timestamp}_`
      : `**${prefix}** (\`${msg.from}\`) · _${timestamp}_`;
    lines.push(header);
    lines.push("");
    // Body is already markdown (default) or plain/html — preserve as-is; the
    // transcript is a text artifact, not a sanitized render surface.
    lines.push(msg.text.trim().length === 0 ? "_(empty)_" : msg.text);
    if (msg.primaryFile) {
      lines.push("");
      lines.push(`_attachment:_ \`${msg.primaryFile}\``);
    }
    lines.push("");
  }

  return lines.join("\n");
}

/** Build a safe filename for the exported transcript. Mirrors the inbox's
 *  conservative set — alphanumerics + `-` + `_`, with a timestamp suffix so
 *  repeated exports never collide. */
export function transcriptFilename(workspaceId: string, now: Date = new Date()): string {
  const safeId = workspaceId.replace(/[^A-Za-z0-9_-]/g, "-").replace(/-+/g, "-");
  const stamp = now.toISOString().replace(/[-:T]/g, "").split(".")[0];
  return `workspace-chat-${safeId}-${stamp}.md`;
}

/** Export: build the markdown, write it to the team content root. Returns
 *  the final path on success; throws on IO error so the UI can toast. */
export async function exportTranscript(options: ExportTranscriptOptions): Promise<string> {
  const body = renderTranscript(options);
  const filename = transcriptFilename(options.workspaceId);
  // The content root is the team's content path (e.g. codesense-context/.../content).
  // Join with "/" — paths on both macOS and Windows accept forward slashes under
  // writeContentFile's path validation.
  const path = `${options.teamContentPath.replace(/\/+$/, "")}/transcripts/${filename}`;
  await writeContentFile(path, body);
  return path;
}
