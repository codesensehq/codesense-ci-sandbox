// SPDX-License-Identifier: MIT
import { cn } from "@/lib/utils";
import {
  extensionOf,
  getViewerFileType,
  KNOWN_BINARY_EXTENSIONS,
  VIEWER_EXTENSIONS,
} from "@/shared/lib/viewer-file-type";

/**
 * What kind of file a nav row is, told by shape alone (colour never carries file type):
 * a PAGE is text you read, a WINDOW is something that renders, a bare GLYPH is code and data.
 */
export type FileKind =
  | "markdown"
  | "text"
  | "pdf"
  | "html"
  | "dashboard"
  | "slides"
  | "canvas"
  | "image"
  | "video"
  | "code"
  | "json"
  | "config"
  | "table"
  | "shell"
  | "archive"
  | "folder"
  | "folderOpen"
  | "epic"
  | "repo"
  | "agentDef";

const CONFIG_EXTENSIONS = new Set(["yaml", "yml", "toml", "ini", "env", "conf", "properties"]);
const TABLE_EXTENSIONS = new Set(["csv", "tsv"]);
const SHELL_EXTENSIONS = new Set(["sh", "bash", "zsh", "fish"]);
const TEXT_EXTENSIONS = new Set(["txt", "text", "log", "rst"]);
const DOCUMENT_EXTENSIONS = new Set(["pdf", "doc", "docx", "xls", "xlsx", "rtf", "odt", "ods"]);
const SLIDE_EXTENSIONS = new Set(["ppt", "pptx", "key", "odp"]);
const MEDIA_EXTENSIONS = new Set([
  "mp4",
  "webm",
  "mov",
  "avi",
  "mkv",
  "mp3",
  "wav",
  "flac",
  "ogg",
  "m4a",
]);
const RASTER_EXTENSIONS = new Set(["ico", "bmp", "tiff", "tif", "heic"]);

/** Finite extension vocabulary used to generate the pane kit's equivalent rule. */
export const FILE_KIND_EXTENSIONS = new Set([
  ...TEXT_EXTENSIONS,
  ...CONFIG_EXTENSIONS,
  ...TABLE_EXTENSIONS,
  ...SHELL_EXTENSIONS,
  ...DOCUMENT_EXTENSIONS,
  ...SLIDE_EXTENSIONS,
  ...MEDIA_EXTENSIONS,
  ...RASTER_EXTENSIONS,
  "json",
  "jsonc",
  "json5",
  "xml",
  ...Object.keys(VIEWER_EXTENSIONS),
  ...KNOWN_BINARY_EXTENSIONS,
]);

/** The path rules of fileKindOf, exported so the pane kit's equivalent is generated from them. */
export const AGENT_DEFINITION_PATTERNS = [
  /(^|\/)\.claude\/agents\/[^/]+\.md$/i,
  /(^|\/)skills\/[^/]+\/SKILL\.md$/,
] as const;
export const DASHBOARD_DIR_PATTERN = /(^|\/)dashboards\//;

/** An agent or skill definition: `.claude/agents/…`, `…/skills/<name>/SKILL.md`, `.agents/skills/…`. */
function isAgentDefinition(path: string): boolean {
  return AGENT_DEFINITION_PATTERNS.some((pattern) => pattern.test(path));
}

/**
 * The kind of a file by its path. Derived from the viewer's own extension map
 * (`VIEWER_EXTENSIONS`, via `getViewerFileType`) and its known-binary set, so the icon
 * always agrees with how the file opens; a dashboard is an HTML page under `dashboards/`.
 */
export function fileKindOf(path: string): FileKind {
  const ext = extensionOf(path.slice(path.lastIndexOf("/") + 1));
  if (isAgentDefinition(path)) return "agentDef";
  if (ext === null || TEXT_EXTENSIONS.has(ext)) return "text";
  if (KNOWN_BINARY_EXTENSIONS.has(ext) || SLIDE_EXTENSIONS.has(ext)) {
    if (DOCUMENT_EXTENSIONS.has(ext)) return "pdf";
    if (SLIDE_EXTENSIONS.has(ext)) return "slides";
    if (MEDIA_EXTENSIONS.has(ext)) return "video";
    if (RASTER_EXTENSIONS.has(ext)) return "image";
    return "archive";
  }
  switch (getViewerFileType(path)) {
    case "markdown":
      return "markdown";
    case "html":
      return DASHBOARD_DIR_PATTERN.test(path) ? "dashboard" : "html";
    case "excalidraw":
      return "canvas";
    case "image":
      return "image";
    case "code":
      if (ext === "json" || ext === "jsonc" || ext === "json5") return "json";
      if (CONFIG_EXTENSIONS.has(ext)) return "config";
      if (TABLE_EXTENSIONS.has(ext)) return "table";
      if (SHELL_EXTENSIONS.has(ext)) return "shell";
      return "code";
  }
}

/** The drawings, on a 16-unit grid. Outlines stroke `currentColor`; solid parts fill it. */
const PAGE = (
  <>
    <path d="M4 1.75h5.4L12.75 5.1v9.15H4z" />
    <path d="M9.25 1.85V5.25h3.4" />
  </>
);
const WINDOW_FRAME = <rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.75" />;
const SOLID = { fill: "currentColor", stroke: "none" } as const;
const FOLDER = (
  <path d="M1.75 4.25c0-.8.6-1.5 1.5-1.5h3l1.5 1.75h5c.8 0 1.5.6 1.5 1.5v6.75c0 .8-.7 1.5-1.5 1.5H3.25c-.9 0-1.5-.7-1.5-1.5z" />
);

const DRAWINGS: Readonly<Record<FileKind, React.ReactNode>> = {
  markdown: (
    <>
      {PAGE}
      <path d="M6.25 8h4.25M6.25 10.25h4.25M6.25 12.25h2.5" />
    </>
  ),
  text: PAGE,
  pdf: (
    <>
      {PAGE}
      <path d="M4 10.5h8.75v3.75H4z" {...SOLID} />
    </>
  ),
  html: (
    <>
      {WINDOW_FRAME}
      <path d="M1.75 5.75h12.5" />
      <circle cx="3.75" cy="4.25" r=".5" {...SOLID} />
      <circle cx="5.4" cy="4.25" r=".5" {...SOLID} />
      <path d="M4.5 8.25h7M4.5 10.5h4.5" />
    </>
  ),
  dashboard: (
    <>
      {WINDOW_FRAME}
      <path d="M1.75 5.75h12.5" />
      <rect x="3.75" y="7.5" width="3.5" height="4" rx=".6" {...SOLID} />
      <rect x="8.9" y="7.6" width="3.35" height="1.5" rx=".5" strokeWidth={1} />
      <rect x="8.9" y="10.1" width="3.35" height="1.35" rx=".5" {...SOLID} opacity={0.75} />
    </>
  ),
  slides: (
    <>
      <rect x="1.75" y="2.5" width="12.5" height="8.5" rx="1.25" />
      <path d="M8 11v2.75M5.5 13.75h5" />
      <path d="M4.5 5.25h4v1.25h-4z" {...SOLID} />
    </>
  ),
  canvas: (
    <>
      <path d="M5 2.6c1.6-.2 2.7.9 2.6 2.4-.1 1.6-1.3 2.5-2.8 2.4C3.4 7.3 2.4 6.2 2.5 4.8 2.6 3.6 3.6 2.7 5 2.6z" />
      <rect x="9" y="9.25" width="5.25" height="4.5" rx=".9" transform="rotate(-3 11.6 11.5)" />
      <path d="M7.4 6.6c1.4.6 2.3 1.4 2.9 2.4" />
      <path d="M9.2 8.9l1.2.2.1-1.2" />
    </>
  ),
  image: (
    <>
      {WINDOW_FRAME}
      <circle cx="5.5" cy="6.25" r="1.15" />
      <path d="M1.9 11.75l3.6-3.25 2.75 2.5 2.25-1.75 3.6 2.9" />
    </>
  ),
  video: (
    <>
      {WINDOW_FRAME}
      <path d="M6.5 5.75v4.5L10.25 8z" {...SOLID} />
    </>
  ),
  code: (
    <>
      <path d="M5.5 4.5L2 8l3.5 3.5M10.5 4.5L14 8l-3.5 3.5" />
      <path d="M9 3.25L7 12.75" opacity={0.55} />
    </>
  ),
  json: (
    <>
      <path d="M5.5 2.5c-1.6 0-2 .7-2 2v1.6c0 1-.5 1.6-1.5 1.9 1 .3 1.5.9 1.5 1.9v1.6c0 1.3.4 2 2 2M10.5 2.5c1.6 0 2 .7 2 2v1.6c0 1 .5 1.6 1.5 1.9-1 .3-1.5.9-1.5 1.9v1.6c0 1.3-.4 2-2 2" />
      <circle cx="8" cy="8" r=".75" {...SOLID} />
    </>
  ),
  config: (
    <>
      {/* The track breaks around each knob, so the drawing needs no ground fill. */}
      <path d="M2 4.75h6.65M11.85 4.75H14M2 11.25h2.15M7.35 11.25H14" />
      <circle cx="10.25" cy="4.75" r="1.6" />
      <circle cx="5.75" cy="11.25" r="1.6" />
    </>
  ),
  table: (
    <>
      <path d="M2.25 5.25h11.5M2.25 8.25h11.5M2.25 11.25h11.5M6.5 2.75v11" />
      <path d="M2.25 2.75h11.5" opacity={0.5} />
    </>
  ),
  shell: (
    <>
      <path d="M2.5 4.5L6 8l-3.5 3.5" />
      <path d="M8 11.75h5.5" />
    </>
  ),
  archive: (
    <>
      <path d="M2.25 5l5.75-3 5.75 3v6.25L8 14.25 2.25 11.25z" />
      <path d="M2.25 5L8 8l5.75-3M8 8v6.25" />
    </>
  ),
  folder: FOLDER,
  folderOpen: (
    <>
      <path d="M1.75 12.25V4.25c0-.8.6-1.5 1.5-1.5h3l1.5 1.75h4.5c.8 0 1.5.6 1.5 1.5v1.25" />
      <path d="M1.75 12.25l1.6-4.5c.2-.5.6-.75 1.1-.75h9.8l-1.75 5.25c-.2.5-.6.75-1.1.75H1.75z" />
    </>
  ),
  epic: (
    <>
      {FOLDER}
      <rect x="4.25" y="8.75" width="7.5" height="2.25" rx="1.1" {...SOLID} opacity={0.35} />
      <rect x="4.25" y="8.75" width="4" height="2.25" rx="1.1" {...SOLID} />
    </>
  ),
  repo: (
    <>
      <circle cx="4.5" cy="3.75" r="1.6" />
      <circle cx="4.5" cy="12.25" r="1.6" />
      <circle cx="11.5" cy="5.5" r="1.6" />
      <path d="M4.5 5.35v5.3M11.5 7.1c0 2.4-2.5 2.6-5.5 4.2" />
    </>
  ),
  agentDef: (
    <>
      <rect x="2.5" y="4.75" width="11" height="8.5" rx="2.5" />
      <path d="M8 4.75V2.5" />
      <circle cx="8" cy="2.1" r=".8" {...SOLID} />
      <circle cx="5.9" cy="8.75" r="1" {...SOLID} />
      <circle cx="10.1" cy="8.75" r="1" {...SOLID} />
    </>
  ),
};

export const FILE_KINDS = Object.keys(DRAWINGS) as FileKind[];

/** The stroke, in CSS px, at a drawn size: heavier when small so it stays legible. */
function strokeAt(size: number): number {
  return size <= 10 ? 1.5 : size >= 20 ? 1.15 : 1.25;
}

/**
 * One file drawing, in ink (`currentColor`; the nav sets it with `cs-sidebar-icon`). Drawn on
 * a 16-unit grid and stroked so the line stays the same weight in CSS px at every size.
 */
export function FileKindIcon({
  kind,
  size = 14,
  className,
}: {
  kind: FileKind;
  size?: number;
  className?: string;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={(strokeAt(size) * 16) / size}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
      data-file-kind={kind}
      className={cn("cs-file-kind-icon flex-none", className)}
    >
      {DRAWINGS[kind]}
    </svg>
  );
}
