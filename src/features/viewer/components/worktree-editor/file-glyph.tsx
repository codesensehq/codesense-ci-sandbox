// SPDX-License-Identifier: MIT
import {
  File,
  FileCode2,
  FileCog,
  FileImage,
  FileJson,
  FileTerminal,
  FileText,
} from "lucide-react";

import { detectLanguageId } from "@/shared/components/code-editor";
import { cn } from "@/lib/utils";

const IMAGE_EXT = /\.(png|jpe?g|gif|webp|ico|bmp|tiff?|avif|heic)$/i;

/** A file-type glyph for tabs, quick open and breadcrumbs. One stroke family,
 *  muted, so the file name — not the icon — carries the row. */
export function FileGlyph({ path, className }: { path: string; className?: string }) {
  const lang = detectLanguageId(path);
  const cls = cn("size-3.5 shrink-0 text-muted-foreground", className);
  if (IMAGE_EXT.test(path)) return <FileImage aria-hidden className={cls} />;
  switch (lang) {
    case "markdown":
      return <FileText aria-hidden className={cls} />;
    case "plain":
      return /\.(txt|log|csv|tsv)$/i.test(path) ? (
        <FileText aria-hidden className={cls} />
      ) : (
        <File aria-hidden className={cls} />
      );
    case "json":
      return <FileJson aria-hidden className={cls} />;
    case "yaml":
    case "toml":
    case "properties":
    case "dockerfile":
      return <FileCog aria-hidden className={cls} />;
    case "shell":
      return <FileTerminal aria-hidden className={cls} />;
    default:
      return <FileCode2 aria-hidden className={cls} />;
  }
}
