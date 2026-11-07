// SPDX-License-Identifier: MIT
import { SquareArrowOutUpRight } from "lucide-react";

import { useViewerStore } from "@/shared/stores/viewer-store";
import { openInEditorFromCwd } from "@/shared/lib/open-in-editor";
import { useDetectedEditors } from "@/shared/hooks/use-detected-editors";

import { editorLaunchLabel } from "../lib/editor-launch-label";
import { ChromeIconButton } from "./overlay-shell";

/**
 * SLW-WDE-5 — the "Open in <your editor>" escape hatch: a chrome control that
 * launches the user's configured EXTERNAL editor at the worktree root. Hosted in
 * BOTH the source-diff expanded header and the WDE-1 editor-inset header (§D),
 * beside — but visually distinct from — WDE-1's in-app "Open editor" control:
 * a "leave-the-app" glyph (`SquareArrowOutUpRight`, not `Code`) and an
 * editor-named label ("Open in VS Code"), never bare "Open editor" (§A).
 *
 * The whole action reuses the existing `openInEditorFromCwd` seam, which already
 * owns the unconfigured-editor guard AND the launch-failure toast (§C / AC-2/
 * AC-4) and returns `void` (fire-and-forget — never blocks the terminal). The
 * control renders even when no editor is configured — the seam's "set one in
 * Settings" toast is the discoverable onboarding nudge (RD-4). Expects a
 * `TooltipProvider` ancestor (both host headers already supply one).
 */
export function OpenInExternalEditorButton({ cwd, wsId }: { cwd: string; wsId: string }) {
  const editor = useViewerStore((s) => s.settings.editor);
  const editorCustomCmd = useViewerStore((s) => s.settings.editorCustomCmd);

  // §B — the friendly label lives in the async detect result, not settings. R1
  // gate: only the concrete-id case needs the id→label map, so skip the backend
  // detect for the unset ("" → generic) and custom (cmd string is the label)
  // paths. Shared session-cached hook → detect runs once, not per overlay-open.
  // Purely cosmetic + off the launch path, so a pending/failed detect never
  // blocks the action.
  const detectedEditors = useDetectedEditors(!!editor && editor !== "custom");
  const label = editorLaunchLabel(editor, editorCustomCmd, detectedEditors);

  return (
    <ChromeIconButton
      testId={`source-open-external-editor-${wsId}`}
      ariaLabel={label}
      tooltip={label}
      onClick={() => openInEditorFromCwd(cwd)}
    >
      <SquareArrowOutUpRight size={13} />
    </ChromeIconButton>
  );
}
