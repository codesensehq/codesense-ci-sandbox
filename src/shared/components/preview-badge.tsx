// SPDX-License-Identifier: MIT
import { useId } from "react";

import { InfoTip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { isPreviewRuntime, PREVIEW_RUNTIME_NOTE } from "@/shared/lib/agent-runtimes";

/** The word a Preview CLI wears wherever it is offered. */
export const PREVIEW_WORD = "Preview";

/** The quiet badge's 18px pill, in foreground at 82% inside a full border, so the word holds
 *  4.5:1 in every bundled theme. A surface with its own ink overrides `--cs-preview-ink` and
 *  `--cs-preview-line`. */
const PREVIEW_CLASS =
  "cs-preview-badge inline-flex h-[18px] shrink-0 items-center rounded-full border px-1.5 text-[11px] leading-none font-medium";

/** The note a control naming a Preview `cli` points `aria-describedby` at, so the badge inside
 *  it needs no focus of its own; nothing for any other CLI. */
export function PreviewNote({ id, cli }: { id: string; cli: string | null | undefined }) {
  return isPreviewRuntime(cli) ? (
    <span id={id} hidden>
      {PREVIEW_RUNTIME_NOTE}
    </span>
  ) : null;
}

/** `id` when `cli` is a Preview (its {@link PreviewNote} is drawn), for `aria-describedby`. */
export function previewNoteRef(id: string, cli: string | null | undefined): string | undefined {
  return isPreviewRuntime(cli) ? id : undefined;
}

/**
 * The one Preview label, after a CLI's name wherever the CLI is offered; nothing for any other
 * CLI. Its name is its text. Inside a control (`standalone` false) it is a plain pill and the
 * control carries the note in its description ({@link PreviewNote}); standing alone it is a
 * button with no action, so the keyboard reaches it and its tooltip describes it.
 */
export function PreviewBadge({
  cli,
  standalone = false,
  className,
}: {
  cli: string | null | undefined;
  standalone?: boolean;
  className?: string;
}) {
  const noteId = useId();
  if (!isPreviewRuntime(cli)) return null;
  if (!standalone)
    return (
      <span
        data-testid="preview-badge"
        title={PREVIEW_RUNTIME_NOTE}
        className={cn(PREVIEW_CLASS, className)}
      >
        {PREVIEW_WORD}
      </span>
    );
  return (
    <InfoTip label={PREVIEW_RUNTIME_NOTE}>
      <button
        type="button"
        data-testid="preview-badge"
        aria-describedby={noteId}
        className={cn(
          PREVIEW_CLASS,
          "cursor-default bg-transparent outline-none focus-visible:ring-2 focus-visible:ring-ring",
          className,
        )}
      >
        {PREVIEW_WORD}
        <PreviewNote id={noteId} cli={cli} />
      </button>
    </InfoTip>
  );
}
