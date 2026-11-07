// SPDX-License-Identifier: MIT
import { useEffect, useState } from "react";
import { validateStoryFile, type StoryValidationReport } from "@/tauri/commands";
import { StoryFindings } from "./story-findings";

/**
 * `validate_story` findings for an on-disk file, shown in the metadata band.
 * Re-validates whenever `revision` changes (the band passes its freshly
 * loaded model, so every re-read re-checks). Renders only for a file the
 * validator reads as a story — including one that merely LOOKS like a story
 * (e.g. `story_id` without `type: story`), which the scanner would otherwise
 * drop without a word.
 */
export function StoryFileFindings({ filePath, revision }: { filePath: string; revision: unknown }) {
  const [report, setReport] = useState<{ filePath: string; report: StoryValidationReport } | null>(
    null,
  );

  useEffect(() => {
    let cancelled = false;
    validateStoryFile(filePath)
      .then((r) => {
        if (!cancelled) setReport({ filePath, report: r });
      })
      .catch(() => {
        // Validation is advisory chrome; a failure (no active team yet, file
        // gone) just leaves the band without a findings strip.
        if (!cancelled) setReport(null);
      });
    return () => {
      cancelled = true;
    };
  }, [filePath, revision]);

  const current = report?.filePath === filePath ? report.report : null;
  if (!current || current.detectedKind !== "pipeline_story" || current.findings.length === 0) {
    return null;
  }
  return (
    <div
      data-testid="metadata-band-story-findings"
      className="border-b px-3.5 py-2"
      style={{ borderColor: "var(--border)" }}
    >
      <StoryFindings report={current} />
    </div>
  );
}
