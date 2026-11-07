// SPDX-License-Identifier: MIT
import { AGENT_RUNTIMES, registeredCliOfBinary } from "@/shared/lib/agent-runtimes";
import { PreviewBadge } from "@/shared/components/preview-badge";
import { missingCliRow } from "@/shared/lib/agent-cli-names";
import { buildTeamKey } from "@/shared/lib/team-key";
import { useFieldDraft } from "../lib/use-field-draft";
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { dashboardSelected, resolveSlot, servesSurface } from "@/features/shell/slots/reveal-slot";
import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";

import {
  useViewerStore,
  defaultSourceDiffInline,
  DEFAULT_SUMMARY_HEADINGS,
  type ContentWidth,
  type SourceDiffLayout,
  type ViewerSettings,
  type FontSize,
  type TreeFontSize,
  type TerminalFontSize,
} from "@/shared/stores/viewer-store";
import type { EditorLineWrap } from "@/shared/components/code-editor/line-wrap";
import {
  type EditorFolding,
  type EditorTabSize,
  EDITOR_TAB_SIZES,
  coerceTabSize,
} from "@/shared/components/code-editor/editor-affordances";
import { SCROLLBACK_OPTIONS, STARSHIP_PRESET_OPTIONS } from "@/shared/constants/settings-options";
import { DEFAULT_SCROLLBACK } from "@/shared/terminal/terminal-constants";
import { DEFAULT_BACKGROUND_AGENT_LIMIT } from "@/shared/lib/background-agent-limit";
import {
  MAX_BACKGROUND_SETUP_CAP,
  readMachineCores,
  resolveBackgroundSetupCap,
} from "@/shared/lib/worktree/background-setup-fanout";
import { useDetectedEditors } from "@/shared/hooks/use-detected-editors";
import {
  checkForUpdate,
  getDependencyStatus,
  recheckDependencies,
  setTerminalShellOverride,
  terminalShellInfo,
  type AgentBinaryStatus,
  type DependencyStatus,
  type TerminalShellInfo,
  type UpdateError,
} from "@/tauri/commands";
import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { logger } from "@/shared/lib/logger";
import { isMac } from "@/shared/lib/shortcuts/keystroke";
import { rowMatches, type SettingRowSpec } from "@/features/settings/lib/row-matches";
import {
  useDashboardProfileStore,
  builtinAgentListRef,
  setSurfaceDashboard,
  setSlotDashboard,
} from "@/features/dashboard/lib/dashboard-profile";
import { dashboardRefKey } from "@/features/dashboard/lib/dashboard-ref";
import { dockChoice } from "@/features/shell/slots/dock-slot-host";
import { dashboardIdentity } from "@/features/shell/slots/slot-fallback-strip";
import { usePaneStore } from "@/features/shell/right-pane/panes/pane-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { commitFontPxOverride } from "@/features/settings/lib/font-px";
import { resolveWorkspaceBarSize, type WorkspaceBarSize } from "@/shared/lib/workspace-bar-size";
import {
  FONT_PX_MIN,
  FONT_PX_MAX,
  getEffectiveViewerFontPx,
  getEffectiveTreeFontPx,
  getEffectiveTerminalFontPx,
} from "@/shared/lib/font-sizes";
import { runUpdateCheck } from "@/app/update-toast";
import { useSummaryHeadings } from "@/shared/hooks/use-summary-headings";

import { PUBLIC_DOWNLOAD_URL, isBetaChannel, readBuildChannel } from "@/features/license/channel";
import { UpdatesSection } from "@/features/license/UpdatesSection";
import { SectionHeader } from "./section-header";
import { SettingRow } from "./setting-row";
import { TerminalServerSection } from "./terminal-server-section";

// ---------- Row specs ----------

const GENERAL_ROW_SPECS: Record<string, SettingRowSpec> = {
  editor: {
    key: "editor",
    label: "Editor",
    keywords: ["external", "cursor", "vscode", "code", "vim", "nvim", "emacs", "custom"],
  },

  terminalPrompt: {
    key: "terminalPrompt",
    label: "Terminal prompt",
    keywords: ["terminal", "prompt", "starship"],
  },
  terminalShell: {
    key: "terminalShell",
    label: "Terminal shell",
    keywords: ["terminal", "shell", "zsh", "bash", "fish", "login", "$SHELL"],
  },
  terminalServer: {
    key: "terminalServer",
    label: "Terminal server",
    keywords: [
      "terminal server",
      "session server",
      "sessions",
      "agents",
      "stop",
      "start",
      "restart",
      "running",
      "stray",
    ],
  },
  agentClis: {
    key: "agentClis",
    label: "Agent CLIs",
    keywords: [
      "dependencies",
      "claude",
      "codex",
      "opencode",
      "hermes",
      "agent cli",
      "binary",
      "path",
      "version",
      "install",
      "re-check",
    ],
  },
  contentWidth: {
    key: "contentWidth",
    label: "Content width",
    keywords: ["content", "width", "narrow", "normal", "wide", "full", "viewer", "layout"],
  },
  defaultDiffLayout: {
    key: "defaultDiffLayout",
    label: "Default diff layout",
    keywords: [
      "diff",
      "layout",
      "inline",
      "split",
      "side-by-side",
      "side by side",
      "source",
      "code",
    ],
  },

  editorLineWrap: {
    key: "editorLineWrap",
    label: "Editor line wrapping",
    keywords: [
      "editor",
      "line",
      "wrap",
      "wrapping",
      "word",
      "soft",
      "prose",
      "markdown",
      "code",
      "scroll",
      "auto",
    ],
  },
  editorFolding: {
    key: "editorFolding",
    label: "Editor code folding",
    keywords: ["editor", "fold", "folding", "collapse", "gutter", "code", "auto"],
  },
  editorWhitespace: {
    key: "editorWhitespace",
    label: "Editor whitespace",
    keywords: ["editor", "whitespace", "tabs", "spaces", "invisible", "show", "render", "dots"],
  },
  editorTabSize: {
    key: "editorTabSize",
    label: "Editor tab size",
    keywords: ["editor", "tab", "size", "indent", "indentation", "spaces", "width", "2", "4", "8"],
  },
  editorProseFont: {
    key: "editorProseFont",
    label: "Editor prose font",
    keywords: [
      "editor",
      "prose",
      "font",
      "reading",
      "serif",
      "markdown",
      "proportional",
      "typeface",
    ],
  },
  viewerFontSize: {
    key: "viewerFontSize",
    label: "Viewer font size",
    keywords: ["viewer", "font", "size", "text", "px", "pixels"],
  },
  treeFontSize: {
    key: "treeFontSize",
    label: "File tree font size",
    keywords: ["tree", "sidebar", "font", "size", "text", "px"],
  },
  workspaceBarSize: {
    key: "workspaceBarSize",
    label: "Workspace bar size",
    keywords: ["workspace", "bottom", "bar", "tray", "pills", "chips", "size", "scale"],
  },
  terminalFontSize: {
    key: "terminalFontSize",
    label: "Terminal font size",
    keywords: ["terminal", "font", "size", "text", "px"],
  },
  terminalScrollback: {
    key: "terminalScrollback",
    label: "Terminal scrollback",
    keywords: ["terminal", "scrollback", "buffer", "lines", "infinite", "history"],
  },
  shellIntegration: {
    key: "shellIntegration",
    label: "Shell integration",
    keywords: ["terminal", "shell", "integration", "osc", "cwd", "prompt", "starship", "history"],
  },
  warmPoolSize: {
    key: "warmPoolSize",
    label: "Warm terminal pool size",
    keywords: [
      "terminal",
      "warm",
      "pool",
      "performance",
      "startup",
      "instant",
      "preload",
      "latency",
    ],
  },
  worktreeSetupParallelism: {
    key: "worktreeSetupParallelism",
    label: "Worktrees set up in parallel",
    keywords: [
      "worktree",
      "setup",
      "parallel",
      "concurrency",
      "npm",
      "install",
      "background",
      "performance",
      "cpu",
      "cores",
    ],
  },
  backgroundAgentLimit: {
    key: "backgroundAgentLimit",
    label: "Background agent limit",
    keywords: [
      "background",
      "agent",
      "limit",
      "unattended",
      "concurrency",
      "detach",
      "supervise",
      "cap",
    ],
  },
  terminalServerMenuBar: {
    key: "terminalServerMenuBar",
    label: "Show terminal server in the menu bar",
    keywords: ["menu bar", "tray", "status", "terminal server", "agents", "stop", "restart"],
  },
  quitOnClose: {
    key: "quitOnClose",
    label: "Quit on window close",
    keywords: [
      "quit",
      "close",
      "background",
      "tray",
      "dock",
      "window",
      "mcp",
      "agent",
      "routine",
      "keep running",
    ],
  },
  summaryHeadings: {
    key: "summaryHeadings",
    label: "Summary section headings",
    keywords: [
      "summary",
      "heading",
      "headings",
      "section",
      "purpose",
      "story",
      "overview",
      "card",
      "pipeline",
      "epic",
      "hud",
    ],
  },

  focusWorkspaceDoc: {
    key: "focusWorkspaceDoc",
    label: "Focus workspace doc on activation",
    keywords: [
      "workspace",
      "doc",
      "document",
      "focus",
      "epic",
      "story",
      "readme",
      "viewer",
      "activation",
      "switch",
      "cli",
      "auto",
    ],
  },
  fileNavPreview: {
    key: "fileNavPreview",
    label: "Use the classic file list",
    keywords: ["files", "sidebar", "nav", "classic", "dashboard"],
  },
  notesPanePreview: {
    key: "notesPanePreview",
    label: "Use the classic Notes",
    keywords: ["notes", "classic", "pane", "old", "preview"],
  },
  dockDashboard: {
    key: "dockDashboard",
    label: "Agents list",
    keywords: ["agents", "list", "dock", "dashboard", "cli", "built-in", "html", "sidebar"],
  },
  osNotifications: {
    key: "osNotifications",
    label: "Notify when an agent needs you",
    keywords: [
      "notification",
      "notifications",
      "notify",
      "alert",
      "dock",
      "badge",
      "permission",
      "question",
      "crash",
      "background",
      "agent",
    ],
  },
  notifyOnFinish: {
    key: "notifyOnFinish",
    label: "Notify when an agent finishes while you're away",
    keywords: [
      "notification",
      "notifications",
      "notify",
      "finished",
      "finish",
      "done",
      "away",
      "background",
      "agent",
    ],
  },
  notificationSound: {
    key: "notificationSound",
    label: "Notification sound",
    keywords: ["notification", "notifications", "sound", "audio", "chime", "alert", "mute"],
  },
  inboxNotifications: {
    key: "inboxNotifications",
    label: "Notify when a new Inbox item arrives",
    keywords: [
      "inbox",
      "notification",
      "notifications",
      "notify",
      "alert",
      "post",
      "attention",
      "background",
      "away",
    ],
  },
  about: {
    key: "about",
    label: "About",
    keywords: ["about", "version", "update", "updates", "check", "release", "codesense"],
  },
};

export const GENERAL_ROW_SPEC_LIST: SettingRowSpec[] = Object.values(GENERAL_ROW_SPECS);

// ---------- General section ----------

export function GeneralSection({ query }: { query: string }) {
  // Substring-only (no fuzzy fallback). With 11 rows, fuzzy walking lets
  // queries like "theme" false-match unrelated rows (e.g. "Content width"
  // via t-h-e-m-e through "con[t]ent wid[t][h]... [e]... nor[m]al... wid[e]").
  // Each row's label + keywords is curated for predictable substring hits.
  const visibleKeys = useMemo(
    () => GENERAL_ROW_SPEC_LIST.filter((spec) => rowMatches(spec, query)).map((spec) => spec.key),
    [query],
  );
  const visibleCount = visibleKeys.length;
  const visibleSet = new Set(visibleKeys);
  const announcement = query.trim().length > 0 ? `${visibleCount} settings shown` : "";

  return (
    <section className="pt-6">
      <SectionHeader id="general">General</SectionHeader>
      <div aria-live="polite" className="sr-only" data-testid="settings-search-count">
        {announcement}
      </div>
      {visibleCount === 0 ? (
        <p data-testid="settings-general-empty" className="py-4 text-sm text-muted-foreground">
          No matching settings.
        </p>
      ) : (
        <>
          {visibleSet.has("editor") && <EditorRow />}
          {visibleSet.has("terminalPrompt") && <TerminalPromptRow />}
          {visibleSet.has("terminalShell") && <TerminalShellRow />}
          {visibleSet.has("terminalServer") && (
            <SettingRow label={GENERAL_ROW_SPECS.terminalServer.label}>
              <TerminalServerSection />
            </SettingRow>
          )}
          {visibleSet.has("agentClis") && <AgentClisRow />}
          {visibleSet.has("contentWidth") && <ContentWidthRow />}
          {visibleSet.has("defaultDiffLayout") && <DefaultDiffLayoutRow />}
          {visibleSet.has("editorLineWrap") && <EditorLineWrapRow />}
          {visibleSet.has("editorFolding") && <EditorFoldingRow />}
          {visibleSet.has("editorWhitespace") && <EditorWhitespaceRow />}
          {visibleSet.has("editorTabSize") && <EditorTabSizeRow />}
          {visibleSet.has("editorProseFont") && <EditorProseFontRow />}
          {visibleSet.has("viewerFontSize") && <ViewerFontSizeRow />}
          {visibleSet.has("treeFontSize") && <TreeFontSizeRow />}
          {visibleSet.has("workspaceBarSize") && <WorkspaceBarSizeRow />}
          {visibleSet.has("terminalFontSize") && <TerminalFontSizeRow />}
          {visibleSet.has("terminalScrollback") && <TerminalScrollbackRow />}
          {visibleSet.has("shellIntegration") && <ShellIntegrationRow />}
          {visibleSet.has("warmPoolSize") && <WarmPoolSizeRow />}
          {visibleSet.has("worktreeSetupParallelism") && <WorktreeParallelismRow />}
          {visibleSet.has("backgroundAgentLimit") && <BackgroundAgentLimitRow />}
          {/* SW-AU-34: background-on-close is macOS-only (dock-resident model); hide
              the toggle off-macOS so it never promises behavior the platform lacks. */}
          {visibleSet.has("quitOnClose") && isMac() && <QuitOnCloseRow />}
          {/* SV-8: the menu-bar item is macOS-only. */}
          {visibleSet.has("terminalServerMenuBar") && isMac() && <TerminalServerMenuBarRow />}
          {visibleSet.has("osNotifications") && <OsNotificationsRow />}
          {visibleSet.has("notifyOnFinish") && <NotifyOnFinishRow />}
          {visibleSet.has("notificationSound") && <NotificationSoundRow />}
          {visibleSet.has("inboxNotifications") && <InboxNotificationsRow />}
          {visibleSet.has("fileNavPreview") && (
            <ClassicSurfaceRow
              slot="file-nav"
              surface="files-tree"
              label="Use the classic file list"
              footer="Turn off to use the file list dashboard."
            />
          )}
          {visibleSet.has("notesPanePreview") && (
            <ClassicSurfaceRow
              surface="notes"
              label={GENERAL_ROW_SPECS.notesPanePreview.label}
              footer="Use the old Notes panel instead of the Notes pane. The classic Notes will be removed soon."
            />
          )}
          {visibleSet.has("dockDashboard") && <AgentsListRow />}
          {visibleSet.has("summaryHeadings") && <SummaryHeadingsRow />}
          {visibleSet.has("focusWorkspaceDoc") && <FocusWorkspaceDocRow />}
          {visibleSet.has("about") && <AboutRow />}
          {visibleSet.has("about") && <UpdatesSection />}
        </>
      )}
    </section>
  );
}

// ---------- Radio group helper ----------

interface RadioGroupRowProps<T extends string> {
  label: string;
  ariaLabel: string;
  testId: string;
  value: T;
  options: readonly { value: T; label: React.ReactNode; title?: string }[];
  onChange: (value: T) => void;
  gridClassName: string;
  buttonPaddingClassName?: string;
  footer?: React.ReactNode;
}

/**
 * Radiogroup row primitive. Renders a labeled group of mutually-exclusive
 * button options with WAI-ARIA radiogroup semantics and keyboard navigation
 * (Arrow keys move + select, Home/End jump to first/last). Uses roving
 * tabindex so Tab enters/exits the group once.
 */
function RadioGroupRow<T extends string>({
  label,
  ariaLabel,
  testId,
  value,
  options,
  onChange,
  gridClassName,
  buttonPaddingClassName = "px-2 py-1",
  footer,
}: RadioGroupRowProps<T>) {
  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const isNext = e.key === "ArrowRight" || e.key === "ArrowDown";
    const isPrev = e.key === "ArrowLeft" || e.key === "ArrowUp";
    const isHome = e.key === "Home";
    const isEnd = e.key === "End";
    if (!isNext && !isPrev && !isHome && !isEnd) return;
    e.preventDefault();
    const currentIdx = options.findIndex((o) => o.value === value);
    const last = options.length - 1;
    let nextIdx = currentIdx;
    if (isNext) nextIdx = currentIdx < 0 ? 0 : currentIdx >= last ? 0 : currentIdx + 1;
    else if (isPrev) nextIdx = currentIdx <= 0 ? last : currentIdx - 1;
    else if (isHome) nextIdx = 0;
    else if (isEnd) nextIdx = last;
    const nextOption = options[nextIdx];
    if (!nextOption) return;
    onChange(nextOption.value);
    const container = e.currentTarget;
    const nextButton = container.querySelector<HTMLButtonElement>(
      `[data-testid="${testId}-${nextOption.value}"]`,
    );
    nextButton?.focus();
  };

  return (
    <SettingRow label={label}>
      <div
        role="radiogroup"
        aria-label={ariaLabel}
        data-testid={testId}
        className={`grid gap-1 ${gridClassName}`}
        onKeyDown={onKeyDown}
      >
        {options.map((opt) => {
          const isActive = value === opt.value;
          return (
            <button
              key={opt.value}
              type="button"
              role="radio"
              aria-checked={isActive}
              tabIndex={isActive ? 0 : -1}
              data-testid={`${testId}-${opt.value}`}
              title={opt.title}
              onClick={() => onChange(opt.value)}
              className={`rounded ${buttonPaddingClassName} text-xs transition-colors ${
                isActive
                  ? "bg-accent text-accent-foreground"
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              {opt.label}
            </button>
          );
        })}
      </div>
      {footer}
    </SettingRow>
  );
}

// ---------- Rows ----------

function EditorRow() {
  const editor = useViewerStore((s) => s.settings.editor);
  const editorCustomCmd = useViewerStore((s) => s.settings.editorCustomCmd);
  const updateSettings = useViewerStore((s) => s.updateSettings);
  // Shared detect hook (SLW-WDE-5 R2) — the same source the WDE-5 external-editor
  // button reads. `refresh` = re-detect on each Settings open so a newly-installed
  // editor shows up here (this config surface remounts per open); the fresh result
  // also repopulates the shared cache the button reads.
  const detectedEditors = useDetectedEditors(true, true);

  return (
    <SettingRow label={GENERAL_ROW_SPECS.editor.label}>
      <div className="space-y-2">
        <div className="grid grid-cols-3 gap-1" data-testid="settings-editor-grid">
          {detectedEditors.map((t) => (
            <button
              key={t.id}
              type="button"
              data-testid={`settings-editor-${t.id}`}
              onClick={() => updateSettings({ editor: t.id, editorCustomCmd: "" })}
              className={`rounded px-2 py-1 text-xs transition-colors ${
                editor === t.id
                  ? "bg-accent text-accent-foreground"
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              {t.label}
            </button>
          ))}
          <button
            type="button"
            data-testid="settings-editor-custom"
            onClick={() => updateSettings({ editor: "custom" })}
            className={`rounded px-2 py-1 text-xs transition-colors ${
              editor === "custom"
                ? "bg-accent text-accent-foreground"
                : "bg-muted text-muted-foreground hover:text-foreground"
            }`}
          >
            Custom
          </button>
        </div>
        {editor === "custom" && (
          <input
            type="text"
            data-testid="settings-editor-custom-cmd"
            aria-label="Custom editor command"
            placeholder="e.g., nvim, emacs"
            value={editorCustomCmd ?? ""}
            onChange={(e) => updateSettings({ editorCustomCmd: e.target.value })}
            className="h-7 w-full rounded border border-border bg-background px-2 text-xs font-mono outline-none focus:border-ring"
          />
        )}
      </div>
    </SettingRow>
  );
}

// SW-EW-32: the ordered, comma-separated list of section headings the summary
// extraction keys off (first match wins) — applied to the nav story cards and
// the story HUD. A local draft string lets the user type commas/spaces freely;
// it parses + commits on blur (trim, drop blanks, fall back to defaults if
// empty) so the backend never receives a list that blanks every doc.
function SummaryHeadingsRow() {
  const summaryHeadings = useSummaryHeadings();
  const updateSettings = useViewerStore((s) => s.updateSettings);
  const joined = summaryHeadings.join(", ");
  const field = useFieldDraft({
    committed: joined,
    syncCommitted: true,
    label: "summary headings",
    commit: async (text) => {
      const parsed = text
        .split(",")
        .map((value) => value.trim())
        .filter(Boolean);
      const headings = parsed.length ? parsed : [...DEFAULT_SUMMARY_HEADINGS];
      updateSettings({ summaryHeadings: headings });
      return headings.join(", ");
    },
  });

  return (
    <SettingRow label={GENERAL_ROW_SPECS.summaryHeadings.label}>
      <div className="space-y-1.5">
        <input
          type="text"
          data-testid="settings-summary-headings"
          aria-label="Summary section headings"
          placeholder="e.g., Purpose, Story, Overview"
          value={field.draft}
          onChange={field.onChange}
          onBlur={field.onBlur}
          onKeyDown={field.onKeyDown}
          className="h-7 w-full rounded border border-border bg-background px-2 text-xs font-mono outline-none focus:border-ring"
        />
        <p className="text-[11px] leading-snug text-muted-foreground">
          Ordered, comma-separated. The story card and epic banner show the first section that
          matches; falls back to the document title.
        </p>
      </div>
    </SettingRow>
  );
}

// TerminalPromptRow has custom two-line button content (label + description)
// that RadioGroupRow can't render. Intentionally stays manual.
function TerminalPromptRow() {
  const starshipPreset = useViewerStore((s) => s.settings.starshipPreset);
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <SettingRow label={GENERAL_ROW_SPECS.terminalPrompt.label}>
      <div className="grid grid-cols-2 gap-1" data-testid="settings-starship-grid">
        {STARSHIP_PRESET_OPTIONS.map((opt) => {
          const isActive = starshipPreset === opt.value;
          return (
            <button
              key={opt.value}
              type="button"
              data-testid={`settings-starship-${opt.value}`}
              onClick={() => updateSettings({ starshipPreset: opt.value })}
              className={`rounded px-2 py-1.5 text-left text-xs transition-colors ${
                isActive
                  ? "bg-accent text-accent-foreground"
                  : "bg-muted text-muted-foreground hover:text-foreground"
              }`}
            >
              <span className="font-medium">{opt.label}</span>
              <span className="block text-[10px] opacity-70">{opt.description}</span>
            </button>
          );
        })}
      </div>
    </SettingRow>
  );
}

// SW-LN-35: the shell terminals run. Shows the detected login shell and takes
// an optional override (an absolute path to zsh, bash, or fish), committed on
// blur. The Rust calls go through `Promise.resolve().then` so a missing backend
// (tests, mock mode) just leaves the detected line blank.
function TerminalShellRow() {
  const terminalShell = useViewerStore((s) => s.settings.terminalShell);
  const updateSettings = useViewerStore((s) => s.updateSettings);
  const [info, setInfo] = useState<TerminalShellInfo | null>(null);
  useEffect(() => {
    let cancelled = false;
    Promise.resolve()
      .then(() => terminalShellInfo())
      .then((next) => {
        if (!cancelled && next) setInfo(next);
      })
      .catch((e) => logger.debug("settings", `terminalShellInfo failed: ${e}`));
    return () => {
      cancelled = true;
    };
  }, []);

  const field = useFieldDraft({
    committed: terminalShell ?? "",
    syncCommitted: true,
    label: "terminal shell",
    commit: async (text) => {
      const next = text.trim();
      if (next === (terminalShell ?? "")) return;
      updateSettings({ terminalShell: next || undefined });
      Promise.resolve()
        .then(() => setTerminalShellOverride(next || undefined))
        .then((updated) => {
          if (updated) setInfo(updated);
        })
        .catch((error) => logger.debug("settings", `setTerminalShellOverride failed: ${error}`));
    },
  });

  return (
    <SettingRow label={GENERAL_ROW_SPECS.terminalShell.label}>
      <div className="space-y-1.5">
        <input
          type="text"
          data-testid="settings-terminal-shell"
          aria-label="Terminal shell override"
          placeholder={info ? `Detected: ${info.detectedPath}` : "Your login shell"}
          value={field.draft}
          onChange={field.onChange}
          onBlur={field.onBlur}
          onKeyDown={field.onKeyDown}
          className="h-7 w-full rounded border border-border bg-background px-2 text-xs font-mono outline-none focus:border-ring"
        />
        <p
          data-testid="settings-terminal-shell-status"
          className="text-[11px] leading-snug text-muted-foreground"
        >
          {terminalShellStatus(info)}
        </p>
      </div>
    </SettingRow>
  );
}

function terminalShellStatus(info: TerminalShellInfo | null): string {
  if (!info) return "New terminals run your login shell. Leave blank to keep it.";
  if (info.overrideRejected) {
    return `${info.overridePath} isn't an executable zsh, bash, or fish. New terminals use ${info.path}.`;
  }
  if (info.source === "override") return `New terminals use ${info.path}.`;
  if (info.unsupportedShell) {
    return `Your login shell (${info.unsupportedShell}) isn't supported yet. New terminals use ${info.path}.`;
  }
  return `Detected ${info.detectedPath}. Leave blank to keep it, or enter a path to zsh, bash, or fish.`;
}

// ---------- Agent CLIs ----------

// Which install of each agent CLI launches use, where it came from, and any other
// installs found and not used. Re-check re-probes the login shell's PATH. The Rust
// calls go through `Promise.resolve().then` so a missing backend leaves it empty.
function AgentClisRow() {
  const [status, setStatus] = useState<DependencyStatus | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState(false);
  const mounted = useRef(true);
  const latestRequest = useRef(0);

  useEffect(() => {
    mounted.current = true;
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    const refresh = () => {
      const request = ++latestRequest.current;
      return Promise.resolve()
        .then(() => getDependencyStatus())
        .then((next) => {
          if (!cancelled && request === latestRequest.current && next) {
            setStatus(next);
            setError(false);
          }
        })
        .catch((e) => {
          logger.debug("settings", `getDependencyStatus failed: ${e}`);
          if (!cancelled && request === latestRequest.current) setError(true);
        });
    };
    Promise.resolve()
      .then(() => listen("dep-status-changed", () => void refresh()))
      .then((fn) => {
        if (cancelled) safeUnlisten(fn);
        else {
          unlisten = fn;
          void refresh();
        }
      })
      .catch((e) => {
        logger.debug("settings", `dep-status-changed listen failed: ${e}`);
        if (!cancelled) void refresh();
      });
    return () => {
      mounted.current = false;
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, []);

  const recheck = () => {
    const request = ++latestRequest.current;
    setChecking(true);
    Promise.resolve()
      .then(() => recheckDependencies())
      .then((next) => {
        if (next && mounted.current && request === latestRequest.current) {
          setStatus(next);
          setError(false);
        }
      })
      .catch((e) => {
        logger.debug("settings", `recheckDependencies failed: ${e}`);
        if (mounted.current && request === latestRequest.current) setError(true);
      })
      .finally(() => {
        if (mounted.current) setChecking(false);
      });
  };

  const order: readonly string[] = AGENT_RUNTIMES;
  const rank = (binary: string) => {
    const runtime = registeredCliOfBinary(binary)?.runtime;
    return runtime && order.includes(runtime) ? order.indexOf(runtime) : order.length;
  };
  const agents = Object.entries(status?.agents ?? {}).sort(([a], [b]) => rank(a) - rank(b));
  return (
    <SettingRow label={GENERAL_ROW_SPECS.agentClis.label} target="agentClis">
      <div className="space-y-3" data-testid="settings-agent-clis">
        {error && (
          <p role="alert" className="text-xs text-destructive">
            Could not check agent CLIs. Try Re-check.
          </p>
        )}
        {agents.length === 0 && !error && (
          <p className="text-[11px] leading-snug text-muted-foreground">
            {status ? "No agent CLIs checked yet." : "Checking…"}
          </p>
        )}
        {agents.map(([basename, agent]) => (
          <AgentCliEntry key={basename} basename={basename} agent={agent} />
        ))}
        <button
          type="button"
          data-testid="settings-agent-clis-recheck"
          onClick={recheck}
          disabled={checking}
          className="h-7 rounded border border-border px-2 text-xs hover:bg-muted disabled:opacity-50"
        >
          {checking ? "Checking…" : "Re-check"}
        </button>
      </div>
    </SettingRow>
  );
}

function AgentCliEntry({ basename, agent }: { basename: string; agent: AgentBinaryStatus }) {
  const shadowed = agent.shadowed ?? [];
  return (
    <div className="space-y-1" data-testid={`settings-agent-cli-${basename}`}>
      <div className="flex flex-wrap items-baseline gap-x-2 text-xs">
        <span className="font-medium">{basename}</span>
        <PreviewBadge cli={registeredCliOfBinary(basename)?.runtime} standalone />
        {agent.resolving ? (
          <span className="text-muted-foreground">checking…</span>
        ) : agent.available && agent.path ? (
          <>
            <span className="font-mono break-all">{agent.path}</span>
            {agent.version && <span className="text-muted-foreground">{agent.version}</span>}
            {agent.source && (
              <span
                data-testid={`settings-agent-cli-${basename}-source`}
                className="rounded bg-muted px-1.5 text-[10px] text-muted-foreground"
              >
                {agent.source === "loginPath" ? "from your shell PATH" : "fallback location"}
              </span>
            )}
          </>
        ) : (
          <span className="text-muted-foreground">
            {agent.version ? `not usable (found ${agent.version})` : "not found"}
          </span>
        )}
      </div>
      {!agent.available && !agent.resolving && (
        <p className="text-[11px] text-muted-foreground">{missingCliRow(basename)}</p>
      )}
      {agent.available && agent.qualified !== null && (
        <p
          role="note"
          data-testid={`settings-agent-cli-${basename}-version-note`}
          className={
            agent.qualified
              ? "text-[11px] text-muted-foreground"
              : "text-[11px] text-[var(--cs-warning-text)]"
          }
        >
          {agent.qualified
            ? "Supported version"
            : `Not a supported version (supported: ${agent.supported})`}
        </p>
      )}
      {shadowed.length > 0 && (
        <div
          role="note"
          data-testid={`settings-agent-cli-${basename}-shadowed`}
          className="rounded border border-amber-500/40 bg-amber-500/10 px-2 py-1 text-[11px] leading-snug"
        >
          <p>Other installs found and not used:</p>
          <ul className="list-disc pl-4">
            {shadowed.map((install) => (
              <li key={install.path} className="font-mono break-all">
                {install.path} ({install.version ?? "unknown version"})
                {!install.onPath && <span className="font-sans"> (not on PATH)</span>}
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

// ---------- Content width ----------

const CONTENT_WIDTH_OPTIONS: { value: ContentWidth; label: string }[] = [
  { value: "narrow", label: "Narrow" },
  { value: "normal", label: "Normal" },
  { value: "wide", label: "Wide" },
  { value: "full", label: "Full" },
];

function ContentWidthRow() {
  const contentWidth = useViewerStore((s) => s.settings.contentWidth);
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow<ContentWidth>
      label={GENERAL_ROW_SPECS.contentWidth.label}
      ariaLabel="Content width"
      testId="settings-content-width"
      value={contentWidth}
      options={CONTENT_WIDTH_OPTIONS}
      onChange={(v) => updateSettings({ contentWidth: v })}
      gridClassName="grid-cols-4"
    />
  );
}

// ---------- Default diff layout (SLW-WD-7) ----------

const DIFF_LAYOUT_OPTIONS: { value: SourceDiffLayout; label: string }[] = [
  { value: "inline", label: "Inline" },
  { value: "split", label: "Side-by-side" },
];

function DefaultDiffLayoutRow() {
  // Seeds the per-tile fallback for a freshly-opened source diff (an explicit
  // per-tile toggle still wins). Resolve the displayed value through the SAME
  // `defaultSourceDiffInline` fail-safe the diff uses, so the row and the diff
  // always agree — including on a corrupted persisted value (→ inline), which
  // `?? "inline"` alone would leave as an unrecognized string with no radio
  // selected.
  const layout: SourceDiffLayout = useViewerStore((s) =>
    defaultSourceDiffInline(s.settings) ? "inline" : "split",
  );
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow<SourceDiffLayout>
      label={GENERAL_ROW_SPECS.defaultDiffLayout.label}
      ariaLabel="Default diff layout"
      testId="settings-default-diff-layout"
      value={layout}
      options={DIFF_LAYOUT_OPTIONS}
      onChange={(v) => updateSettings({ defaultSourceDiffLayout: v })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          How a worktree source diff opens by default. Inline is a single-column unified diff;
          side-by-side shows the before/after panes. A per-diff toggle overrides this.
        </p>
      }
    />
  );
}

// ---------- Font-size rows ----------

const FONT_GRID_COLS_BY_COUNT: Record<number, string> = {
  3: "grid-cols-3",
  4: "grid-cols-4",
};

type FontPresetField = "viewerFontSize" | "treeFontSize" | "terminalFontSize";
type FontPxField = "viewerFontSizePx" | "treeFontSizePx" | "terminalFontSizePx";

interface FontSizeRowConfig<T extends string> {
  label: string;
  testId: string;
  ariaLabel: string;
  options: { value: T; label: string }[];
  preset: T;
  presetPx: number;
  effectivePx: number;
  customPx: number;
  presetField: FontPresetField;
  pxField: FontPxField;
}

function FontSizeRow<T extends string>(cfg: FontSizeRowConfig<T>) {
  const updateSettings = useViewerStore((s) => s.updateSettings);
  const isCustom = cfg.customPx > 0;
  const [resetCounter, setResetCounter] = useState(0);

  const applyPreset = (value: T) => {
    updateSettings({ [cfg.presetField]: value, [cfg.pxField]: 0 } as Partial<ViewerSettings>);
  };
  const applyCustomPx = (px: number) => {
    updateSettings({ [cfg.pxField]: px } as Partial<ViewerSettings>);
  };

  return (
    <SettingRow label={cfg.label}>
      <div className="space-y-2">
        <div
          role="radiogroup"
          aria-label={cfg.ariaLabel}
          data-testid={cfg.testId}
          className={`grid gap-1 ${FONT_GRID_COLS_BY_COUNT[cfg.options.length] ?? "grid-cols-3"}`}
        >
          {cfg.options.map((opt) => {
            const isActive = !isCustom && cfg.preset === opt.value;
            return (
              <button
                key={opt.value}
                type="button"
                role="radio"
                aria-checked={isActive}
                tabIndex={isActive ? 0 : -1}
                data-testid={`${cfg.testId}-${opt.value}`}
                onClick={() => applyPreset(opt.value)}
                className={`rounded px-2 py-1 text-xs transition-colors ${
                  isActive
                    ? "bg-accent text-accent-foreground"
                    : "bg-muted text-muted-foreground hover:text-foreground"
                }`}
              >
                {opt.label}
              </button>
            );
          })}
        </div>
        <div className="flex items-center gap-2">
          <input
            key={`${cfg.testId}-${cfg.effectivePx}-${resetCounter}`}
            type="number"
            min={FONT_PX_MIN}
            max={FONT_PX_MAX}
            defaultValue={cfg.effectivePx}
            onBlur={(e) => {
              const field = e.currentTarget;
              commitTextEditOnBlur(field, () => {
                const next = commitFontPxOverride(field.value, cfg.presetPx);
                if (next === null) {
                  setResetCounter((c) => c + 1);
                  return;
                }
                applyCustomPx(next);
              });
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
              if (e.key === "Escape") {
                e.currentTarget.value = String(cfg.effectivePx);
                e.currentTarget.blur();
              }
            }}
            data-testid={`${cfg.testId}-px`}
            aria-label={`${cfg.ariaLabel} in pixels`}
            className="h-7 w-20 rounded border border-border bg-background px-2 text-xs font-mono outline-none focus:border-ring"
          />
          <p className="text-xs text-muted-foreground">
            px — {isCustom ? "custom" : "from preset"}
          </p>
        </div>
      </div>
    </SettingRow>
  );
}

const VIEWER_FONT_OPTIONS: { value: FontSize; label: string }[] = [
  { value: "small", label: "Small" },
  { value: "normal", label: "Normal" },
  { value: "large", label: "Large" },
];

function ViewerFontSizeRow() {
  const preset = useViewerStore((s) => s.settings.viewerFontSize);
  const customPx = useViewerStore((s) => s.settings.viewerFontSizePx ?? 0);
  const effectivePx = getEffectiveViewerFontPx(preset, customPx);
  const presetPx = getEffectiveViewerFontPx(preset, 0);
  return (
    <FontSizeRow<FontSize>
      label={GENERAL_ROW_SPECS.viewerFontSize.label}
      testId="settings-viewer-font-size"
      ariaLabel="Viewer font size"
      options={VIEWER_FONT_OPTIONS}
      preset={preset}
      presetPx={presetPx}
      effectivePx={effectivePx}
      customPx={customPx}
      presetField="viewerFontSize"
      pxField="viewerFontSizePx"
    />
  );
}

const TREE_FONT_OPTIONS: { value: TreeFontSize; label: string }[] = [
  { value: "small", label: "Small" },
  { value: "normal", label: "Normal" },
  { value: "large", label: "Large" },
];

function TreeFontSizeRow() {
  const preset = useViewerStore((s) => s.settings.treeFontSize);
  const customPx = useViewerStore((s) => s.settings.treeFontSizePx ?? 0);
  const effectivePx = getEffectiveTreeFontPx(preset, customPx);
  const presetPx = getEffectiveTreeFontPx(preset, 0);
  return (
    <FontSizeRow<TreeFontSize>
      label={GENERAL_ROW_SPECS.treeFontSize.label}
      testId="settings-tree-font-size"
      ariaLabel="File tree font size"
      options={TREE_FONT_OPTIONS}
      preset={preset}
      presetPx={presetPx}
      effectivePx={effectivePx}
      customPx={customPx}
      presetField="treeFontSize"
      pxField="treeFontSizePx"
    />
  );
}

// ---------- Workspace bar size ----------

const WORKSPACE_BAR_SIZE_OPTIONS: { value: WorkspaceBarSize; label: string }[] = [
  { value: "small", label: "Small" },
  { value: "normal", label: "Normal" },
  { value: "large", label: "Large" },
];

/** The bottom workspace bar's size: one factor for its pills, avatars, badges,
 *  controls and height. A size step, not a px value — the bar is chrome, and
 *  three steps keep it in proportion with the rows around it. */
function WorkspaceBarSizeRow() {
  // Resolved, so a corrupted persisted value shows Normal selected, not a blank row.
  const size = useViewerStore((s) => resolveWorkspaceBarSize(s.settings.workspaceBarSize));
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow<WorkspaceBarSize>
      label={GENERAL_ROW_SPECS.workspaceBarSize.label}
      ariaLabel="Workspace bar size"
      testId="settings-workspace-bar-size"
      value={size}
      options={WORKSPACE_BAR_SIZE_OPTIONS}
      onChange={(v) => updateSettings({ workspaceBarSize: v })}
      gridClassName="grid-cols-3"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          The bar along the bottom of the window. Its pills, avatars and buttons scale together.
        </p>
      }
    />
  );
}

const TERMINAL_FONT_OPTIONS: { value: TerminalFontSize; label: string }[] = [
  { value: "small", label: "Small" },
  { value: "normal", label: "Normal" },
  { value: "large", label: "Large" },
  { value: "xl", label: "XL" },
];

function TerminalFontSizeRow() {
  const preset = useViewerStore((s) => s.settings.terminalFontSize);
  const customPx = useViewerStore((s) => s.settings.terminalFontSizePx ?? 0);
  const effectivePx = getEffectiveTerminalFontPx(preset, customPx);
  const presetPx = getEffectiveTerminalFontPx(preset, 0);
  return (
    <FontSizeRow<TerminalFontSize>
      label={GENERAL_ROW_SPECS.terminalFontSize.label}
      testId="settings-terminal-font-size"
      ariaLabel="Terminal font size"
      options={TERMINAL_FONT_OPTIONS}
      preset={preset}
      presetPx={presetPx}
      effectivePx={effectivePx}
      customPx={customPx}
      presetField="terminalFontSize"
      pxField="terminalFontSizePx"
    />
  );
}

// ---------- Terminal scrollback ----------

function TerminalScrollbackRow() {
  const raw = useViewerStore((s) => s.settings.terminalScrollback);
  const active = raw ?? DEFAULT_SCROLLBACK;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.terminalScrollback.label}
      ariaLabel="Terminal scrollback"
      testId="settings-scrollback"
      value={String(active)}
      options={SCROLLBACK_OPTIONS.map((o) => ({ value: String(o.value), label: o.label }))}
      onChange={(v) => updateSettings({ terminalScrollback: Number(v) })}
      gridClassName="grid-cols-3"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Lines of history each terminal keeps. Default 5,000. Changes apply to terminals opened
          afterwards.
        </p>
      }
    />
  );
}

// ---------- Shell integration (PT-5) ----------

function ShellIntegrationRow() {
  // Defaults on; undefined means "use default (on)".
  const raw = useViewerStore((s) => s.settings.shellIntegration);
  const active = raw ?? true;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.shellIntegration.label}
      ariaLabel="Shell integration"
      testId="settings-shell-integration"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ shellIntegration: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          cwd tracking, prompt markers &amp; instant prompt. Applies to new terminals.
        </p>
      }
    />
  );
}

// ---------- Publish mode (SW-GS-17) ----------

function QuitOnCloseRow() {
  // SW-AU-34 (AC-5): default OFF → closing the window hides the app (macOS
  // dock-resident) and keeps the MCP host + engine alive so an agent can
  // keep driving codesense with no window on screen. On → a window close quits.
  // undefined means "use default (off / background-on-close)".
  const raw = useViewerStore((s) => s.settings.quitOnClose);
  const active = raw ?? false;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.quitOnClose.label}
      ariaLabel="Quit on window close"
      testId="settings-quit-on-close"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ quitOnClose: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Off keeps codesense running in the background on close (dock icon reopens it) so routines
          keep firing. On makes closing the window quit.
        </p>
      }
    />
  );
}

// ---------- Terminal server in the menu bar (SV-8) ----------

function TerminalServerMenuBarRow() {
  // Off by default; undefined reads as off. The push to Rust lives in
  // `useTerminalServerMenuBarSetting` (mounted by App), so this row only
  // writes the setting.
  const active = useViewerStore((s) => s.settings.terminalServerMenuBar) ?? false;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.terminalServerMenuBar.label}
      ariaLabel="Show terminal server in the menu bar"
      testId="settings-terminal-server-menu-bar"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ terminalServerMenuBar: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Shows whether the terminal server is running and how many agents it runs, with Restart,
          Stop and Start, while CodeSense runs (also in the background). Quitting CodeSense removes
          it.
        </p>
      }
    />
  );
}

// ---------- Focus workspace doc on activation (SW-CW-5) ----------

function FocusWorkspaceDocRow() {
  // Opt-in; default OFF (undefined reads as false). When on, activating an
  // epic/story workspace focuses its own doc in the viewer.
  const active = useViewerStore((s) => s.settings.focusWorkspaceDocOnActivation) ?? false;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.focusWorkspaceDoc.label}
      ariaLabel="Focus workspace doc on activation"
      testId="settings-focus-workspace-doc"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ focusWorkspaceDocOnActivation: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          When you open or switch to an epic or story workspace, its own doc (the epic README or
          story file) opens in the viewer. Off keeps whatever you&apos;re currently viewing.
        </p>
      }
    />
  );
}

function EditorLineWrapRow() {
  // SW-VE-8: tri-state; `auto` (default) wraps prose (markdown/text) but not
  // code — the `resolveEditorLineWrap` resolver (shared with the editor) owns
  // what each mode means, so this row only picks the mode. Coerce any
  // unrecognized/corrupted in-memory value back to `auto` so exactly one option
  // is selected (mirrors the load-time coercion in the store).
  const raw = useViewerStore((s) => s.settings.editorLineWrap);
  const mode: EditorLineWrap = raw === "on" || raw === "off" ? raw : "auto";
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.editorLineWrap.label}
      ariaLabel="Editor line wrapping"
      testId="settings-editor-line-wrap"
      value={mode}
      options={[
        { value: "auto", label: "Auto" },
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ editorLineWrap: v })}
      gridClassName="grid-cols-3"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Soft-wrap long lines in the editor. Auto wraps prose (markdown, text) but not code; On/Off
          force it.
        </p>
      }
    />
  );
}

// ---------- Editor power-config (SW-VE-11) ----------

function EditorFoldingRow() {
  // Tri-state; `auto` (default) folds code / not prose — mirrors EditorLineWrapRow.
  // Coerce any unrecognized/corrupted in-memory value back to `auto` so exactly
  // one option is selected (mirrors the load-time coercion in the store).
  const raw = useViewerStore((s) => s.settings.editorFolding);
  const mode: EditorFolding = raw === "on" || raw === "off" ? raw : "auto";
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.editorFolding.label}
      ariaLabel="Editor code folding"
      testId="settings-editor-folding"
      value={mode}
      options={[
        { value: "auto", label: "Auto" },
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ editorFolding: v })}
      gridClassName="grid-cols-3"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Collapsible fold gutter for code. Auto shows it on code files but not prose; On/Off force
          it.
        </p>
      }
    />
  );
}

function EditorWhitespaceRow() {
  const active = useViewerStore((s) => s.settings.editorShowWhitespace) ?? false;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.editorWhitespace.label}
      ariaLabel="Editor whitespace rendering"
      testId="settings-editor-whitespace"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ editorShowWhitespace: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Render tabs and spaces as faint glyphs while editing.
        </p>
      }
    />
  );
}

function EditorTabSizeRow() {
  // Numeric 2/4/8; coerce any corrupted in-memory value back to a valid size so
  // the radio always has one selection (mirrors the load-time coercion).
  const raw = useViewerStore((s) => s.settings.editorTabSize);
  const active: EditorTabSize = raw === 4 || raw === 8 ? raw : 2;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.editorTabSize.label}
      ariaLabel="Editor tab size"
      testId="settings-editor-tab-size"
      value={String(active)}
      options={EDITOR_TAB_SIZES.map((n) => ({ value: String(n), label: String(n) }))}
      onChange={(v) => updateSettings({ editorTabSize: coerceTabSize(Number(v)) })}
      gridClassName="grid-cols-3"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Indent width and the display width of a tab character.
        </p>
      }
    />
  );
}

function EditorProseFontRow() {
  const active = useViewerStore((s) => s.settings.editorProseFont) ?? false;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.editorProseFont.label}
      ariaLabel="Editor prose reading font"
      testId="settings-editor-prose-font"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ editorProseFont: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Edit markdown and text in a proportional reading font. Code always stays monospace.
        </p>
      }
    />
  );
}

// ---------- Warm terminal pool (PT-8) ----------

const WARM_POOL_DEFAULT = 2;

function WarmPoolSizeRow() {
  // Defaults to 2; undefined means "use default".
  const raw = useViewerStore((s) => s.settings.terminalWarmPoolSize);
  const active = raw ?? WARM_POOL_DEFAULT;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.warmPoolSize.label}
      ariaLabel="Warm terminal pool size"
      testId="settings-warm-pool-size"
      value={String(active)}
      options={[0, 1, 2, 3, 4, 5].map((n) => ({ value: String(n), label: String(n) }))}
      onChange={(v) => updateSettings({ terminalWarmPoolSize: Number(v) })}
      gridClassName="grid-cols-6"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Pre-spawn this many shells so new terminals open instantly. Set 0 to disable.
        </p>
      }
    />
  );
}

// ---------- Worktree setup parallelism (RF-perf-14d → RF-perf-23) ----------

/** The numeric presets the row offers as buttons — the single source of truth for
 *  both the preset buttons and "is this value a preset?". Any OTHER positive
 *  integer is a valid "custom" value, reached via the "Custom…" reveal. */
const WORKTREE_PARALLELISM_PRESETS: readonly number[] = [1, 2, 3, 4, 6, 8, 12];
const isParallelismPreset = (n: number): boolean => WORKTREE_PARALLELISM_PRESETS.includes(n);
/** Sanitize a user-entered custom cap: floor, clamp to [1, MAX]. `Off` is the
 *  unbounded path, so a specific custom value is deliberately bounded. */
const clampCustomCap = (n: number): number =>
  Math.min(MAX_BACKGROUND_SETUP_CAP, Math.max(1, Math.floor(n)));

function WorktreeParallelismRow() {
  // "auto" (default) sizes the background-setup gate to the machine; "off" = no
  // cap (all at once); an explicit integer is a manual override — a preset button
  // or a custom value. `resolveBackgroundSetupCap` (shared with the wiring hook
  // `use-background-setup-cap`) owns "what the setting means as a number" so the
  // Auto hint here and the applied cap can never disagree.
  const raw = useViewerStore((s) => s.settings.worktreeSetupParallelism);
  const updateSettings = useViewerStore((s) => s.updateSettings);
  // Cheap synchronous read; stable per machine. Shown so the user sees what Auto
  // picked on THIS computer.
  const autoResolved = resolveBackgroundSetupCap("auto", readMachineCores());

  // A persisted non-preset number (e.g. 10, 999) means the user is on a custom
  // value — derived, no state. `customOpen` additionally keeps "Custom…" selected
  // when the committed value happens to equal a preset (you typed 8), so the
  // radiogroup always has exactly one selection (a11y). It's initialized from the
  // persisted value and thereafter written ONLY by the click handlers — no effect
  // syncing state to props ("you might not need an effect").
  const settingIsCustomNumber = typeof raw === "number" && !isParallelismPreset(raw);
  const [customOpen, setCustomOpen] = useState(settingIsCustomNumber);

  const active: string =
    customOpen || settingIsCustomNumber
      ? "custom"
      : raw === undefined || raw === "auto"
        ? "auto"
        : raw === "off"
          ? "off"
          : String(raw); // a preset number

  // The custom numeric input's draft text (commit-on-blur/Enter). Seeded from the
  // current numeric value, or the resolved Auto cap as a sensible starting point.
  const seedCustom = typeof raw === "number" ? raw : autoResolved;
  const [customDraft, setCustomDraft] = useState(String(seedCustom));

  const commitCustom = (draft = customDraft) => {
    const text = draft.trim();
    const n = Number(text);
    if (!text || !Number.isFinite(n)) {
      // Blank / non-numeric → keep the last valid value (never empty, never a
      // silent revert to Auto).
      const fallback = typeof raw === "number" ? raw : seedCustom;
      setCustomDraft(String(fallback));
      updateSettings({ worktreeSetupParallelism: fallback });
      return;
    }
    const v = clampCustomCap(n); // 0 / negative / fractional / absurd → [1, MAX]
    setCustomDraft(String(v));
    updateSettings({ worktreeSetupParallelism: v });
  };

  const onChange = (v: string) => {
    if (v === "custom") {
      // Selecting Custom… seeds the reveal from the CURRENT committed value
      // (seedCustom = the live numeric cap, or Auto's resolved value) — never a
      // stale mount-time draft — and applies it so the radio stays selected.
      setCustomOpen(true);
      const seed = clampCustomCap(seedCustom);
      setCustomDraft(String(seed));
      updateSettings({ worktreeSetupParallelism: seed });
      return;
    }
    setCustomOpen(false);
    updateSettings({
      worktreeSetupParallelism: v === "auto" ? "auto" : v === "off" ? "off" : Number(v),
    });
  };

  const footerCopy =
    active === "off"
      ? "Off — no limit. Every queued worktree sets up immediately. Your Mac's own scheduler is the only backstop, so on a slower machine expect a brief burst of disk activity."
      : active === "custom"
        ? "Blank or invalid keeps your last value; 0 or less becomes 1 (use Off for no limit)."
        : `How many worktrees set up at once in the background. Setup is now a light copy — not a full reinstall — so higher values are cheap. Auto sizes to your CPU (${autoResolved} on this machine). Off = no limit — all set up at once.`;

  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.worktreeSetupParallelism.label}
      ariaLabel="Worktrees set up in parallel"
      testId="settings-worktree-parallelism"
      value={active}
      options={[
        { value: "auto", label: `Auto (${autoResolved})` },
        ...WORKTREE_PARALLELISM_PRESETS.map((n) => ({ value: String(n), label: String(n) })),
        { value: "off", label: "Off" },
        { value: "custom", label: "Custom…" },
      ]}
      onChange={onChange}
      gridClassName="grid-cols-5"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <>
          {active === "custom" && (
            <div className="mt-1 flex items-center gap-1.5 text-[10px] text-muted-foreground">
              <label htmlFor="settings-worktree-parallelism-custom-input">Custom limit:</label>
              <input
                id="settings-worktree-parallelism-custom-input"
                data-testid="settings-worktree-parallelism-custom-input"
                type="number"
                min={1}
                inputMode="numeric"
                value={customDraft}
                onChange={(e) => setCustomDraft(e.target.value)}
                onBlur={(event) => {
                  const field = event.currentTarget;
                  commitTextEditOnBlur(field, () => commitCustom(field.value));
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    commitCustom();
                    e.currentTarget.blur();
                  }
                }}
                className="w-14 rounded border border-border bg-background px-1 py-0.5 text-xs"
              />
              <span>worktrees at once</span>
            </div>
          )}
          <p className="mt-1 text-[10px] text-muted-foreground">{footerCopy}</p>
        </>
      }
    />
  );
}

// ---------- OS notifications when an agent needs you ----------

function OsNotificationsRow() {
  // undefined means "use default (on)".
  const raw = useViewerStore((s) => s.settings.osNotifications);
  const active = raw ?? true;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.osNotifications.label}
      ariaLabel="Notify when an agent needs you"
      testId="settings-os-notifications"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ osNotifications: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <>
          <p className="mt-1 text-[10px] text-muted-foreground">
            While codesense is in the background, a system notification tells you when an agent asks
            for permission, asks you a question, crashes, or stops on an error, including agents
            with no tab open, such as one on another machine. Click it right away to go straight to
            that agent; otherwise a Jump button waits for you in the app. While you're in the app on
            another tab, the Jump button appears instead of a notification. The dock badge counts
            those agents either way.
          </p>
          <TestNotificationButton />
        </>
      }
    />
  );
}

/**
 * Self-serve diagnostic: fires a known-good notification through the same
 * `tauriOsAttention` path a real alert uses. If permission is denied (silent
 * macOS state) or the plugin fails, the toast tells the user how to recover
 * (System Settings → Notifications). The one place to prove notifications work
 * without waiting for a real agent to need you.
 */
function TestNotificationButton() {
  const [busy, setBusy] = useState(false);
  const onClick = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const { tauriOsAttention } = await import("@/features/viewer/lib/os-attention");
      const ok = await tauriOsAttention.notify(
        "CodeSense test notification",
        "If you can see this, notifications are working.",
      );
      if (!ok) {
        toast.error("Notification could not be shown", {
          description:
            "macOS may have denied notifications for CodeSense. Open System Settings → Notifications → CodeSense and allow them, then try again.",
          duration: 10_000,
        });
      } else {
        toast.success("Test notification sent", {
          description:
            "If nothing appeared, your window may be focused — macOS suppresses banners for focused apps. Try again while CodeSense is in the background.",
          duration: 8_000,
        });
      }
    } finally {
      setBusy(false);
    }
  };
  return (
    <button
      type="button"
      data-testid="settings-test-notification"
      onClick={onClick}
      disabled={busy}
      className="mt-2 rounded border border-border bg-muted px-2 py-1 text-[10px] text-muted-foreground transition-colors hover:text-foreground disabled:opacity-50"
    >
      {busy ? "Sending…" : "Send test notification"}
    </button>
  );
}

function NotifyOnFinishRow() {
  // undefined means "use default (on)".
  const active = useViewerStore((s) => s.settings.notifyOnFinish) ?? true;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.notifyOnFinish.label}
      ariaLabel="Notify when an agent finishes while you're away"
      testId="settings-notify-on-finish"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ notifyOnFinish: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          When an agent you started finishes work that took 30 seconds or more and you never had its
          tab in view while it worked: a notification if codesense is in the background, else a Jump
          pill in the app. It includes the agent&apos;s summary when it left one.
        </p>
      }
    />
  );
}

function NotificationSoundRow() {
  // undefined means "use default (off)".
  const active = useViewerStore((s) => s.settings.notificationSound) ?? "off";
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.notificationSound.label}
      ariaLabel="Notification sound"
      testId="settings-notification-sound"
      value={active}
      options={[
        { value: "off", label: "Off" },
        { value: "needs-you", label: "Needs you" },
        { value: "all", label: "All" },
      ]}
      onChange={(v) => updateSettings({ notificationSound: v })}
      gridClassName="grid-cols-3"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Plays your system&apos;s notification sound. Needs you: questions, permission prompts,
          crashes and errors. All: also when an agent finishes. Focus and Do Not Disturb still
          silence it.
        </p>
      }
    />
  );
}

// ---------- Inbox arrival notifications (SLW-TL-4, opt-in) ----------

function InboxNotificationsRow() {
  // undefined means "use default (off)" — Inbox notifications are OPT-IN (§2.3).
  const raw = useViewerStore((s) => s.settings.inboxNotifications);
  const active = raw === true;
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.inboxNotifications.label}
      ariaLabel="Notify when a new Inbox item arrives"
      testId="settings-inbox-notifications"
      value={active ? "on" : "off"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => updateSettings({ inboxNotifications: v === "on" })}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          Covers AMBIENT Inbox posts only. Blocking items an agent needs you to answer (approve,
          answer, validate) ALWAYS notify in the background — the system has to tell you. Turn this
          on to also hear about plain “needs attention” posts; the nav badge counts both either way.
        </p>
      }
    />
  );
}

// ---------- Background agent limit (SW-DX-17d) ----------

function BackgroundAgentLimitRow() {
  // "auto" (default) resolves to an attention-based constant (NOT core-sized —
  // this bounds how many unattended agents a person supervises, not CPU work); an
  // explicit 2–8 is a manual override. `resolveBackgroundAgentLimit` (shared with
  // `use-background-agent-limit`) owns "what the setting means as a number" so the
  // Auto hint here and the applied limit can never disagree.
  const raw = useViewerStore((s) => s.settings.backgroundAgentLimit);
  const active = raw === undefined ? "auto" : String(raw);
  const updateSettings = useViewerStore((s) => s.updateSettings);
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.backgroundAgentLimit.label}
      ariaLabel="Background agent limit"
      testId="settings-background-agent-limit"
      value={active}
      options={[
        { value: "auto", label: `Auto (${DEFAULT_BACKGROUND_AGENT_LIMIT})` },
        { value: "2", label: "2" },
        { value: "4", label: "4" },
        { value: "6", label: "6" },
        { value: "8", label: "8" },
      ]}
      onChange={(v) => updateSettings({ backgroundAgentLimit: v === "auto" ? "auto" : Number(v) })}
      gridClassName="grid-cols-5"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          How many agents can run unattended in the background at once. Sending one more to the
          background is refused until you stop one or raise this. Agents open in a tab (foreground)
          don't count.
        </p>
      }
    />
  );
}

// ---------- About / version ----------

function AboutRow() {
  const [appVersion, setAppVersion] = useState<string>("");
  const [versionError, setVersionError] = useState(false);
  const [updateChecking, setUpdateChecking] = useState(false);
  const manualToastIdRef = useRef<string | number | null>(null);
  // The GitHub update path exists only in the internal channel; beta builds update
  // through the beta channel and say so in the Updates section.
  const beta = isBetaChannel(readBuildChannel());

  useEffect(() => {
    let cancelled = false;
    getVersion()
      .then((v) => {
        if (!cancelled) setAppVersion(v);
      })
      .catch(() => {
        if (!cancelled) setVersionError(true);
      });
    return () => {
      cancelled = true;
      if (manualToastIdRef.current !== null) {
        toast.dismiss(manualToastIdRef.current);
        manualToastIdRef.current = null;
      }
    };
  }, []);

  const onCheck = async () => {
    setUpdateChecking(true);
    try {
      const info = await checkForUpdate();
      if (info) {
        runUpdateCheck();
      } else {
        manualToastIdRef.current = toast(
          `You're up to date${appVersion ? ` (v${appVersion})` : ""}`,
          { id: "update-manual", duration: 3000 },
        );
      }
    } catch (e) {
      const err = e as UpdateError | string;
      const action = {
        label: "Download manually",
        onClick: () => openUrl(PUBLIC_DOWNLOAD_URL),
      };
      // A build with no update source says so; trying again would not change that.
      manualToastIdRef.current =
        typeof err !== "string" && err.kind === "notFound"
          ? toast(err.message, { id: "update-manual", duration: 10_000, action })
          : toast.error("Update check failed", {
              id: "update-manual",
              description: "Try again or download manually.",
              duration: 10_000,
              action,
            });
    } finally {
      setUpdateChecking(false);
    }
  };

  const versionText = versionError ? "version unavailable" : appVersion ? `v${appVersion}` : "";

  return (
    <SettingRow label={GENERAL_ROW_SPECS.about.label}>
      <div className="flex items-center gap-3">
        <span className="text-xs text-muted-foreground" data-testid="settings-about-version">
          CodeSense {versionText}
        </span>
        {!beta && (
          <button
            type="button"
            data-testid="settings-about-check-updates"
            disabled={updateChecking}
            onClick={onCheck}
            className="rounded border border-border bg-muted px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground disabled:opacity-50"
          >
            {updateChecking ? "Checking..." : "Check for Updates"}
          </button>
        )}
      </div>
    </SettingRow>
  );
}

function ClassicSurfaceRow({
  slot = "right-pane",
  surface,
  label,
  footer,
}: {
  slot?: "right-pane" | "file-nav";
  surface: string;
  label: string;
  footer: string;
}) {
  const org = useOrgStore((s) => s.activeOrgSlug);
  const team = useOrgStore((s) => s.activeTeamName);
  const listing = usePaneStore((s) => s.listing);
  const teamKey = usePaneStore((s) => s.teamKey);
  useDashboardProfileStore((s) => s.profile);
  const current = org && team && teamKey === buildTeamKey(org, team);
  const pane = current
    ? (resolveSlot(slot, { surface }).dashboard ??
      listing?.panes.find((pane) =>
        slot === "file-nav" ? pane.declaration?.slots.includes(slot) : servesSurface(pane, surface),
      ))
    : undefined;
  const ref = pane?.dashboardRef;
  if (!ref) return null;
  const raw = dashboardSelected(ref, true, slot);
  return (
    <RadioGroupRow
      label={label}
      ariaLabel={label}
      testId={`settings-${surface}-pane-preview`}
      value={raw ? "off" : "on"}
      options={[
        { value: "on", label: "On" },
        { value: "off", label: "Off" },
      ]}
      onChange={(v) => {
        if (org)
          void (
            slot === "file-nav"
              ? setSlotDashboard(org, slot, v === "off" ? ref : "native")
              : setSurfaceDashboard(org, surface, ref, v === "off")
          ).catch(console.error);
      }}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={<p className="mt-1 text-[10px] text-muted-foreground">{footer}</p>}
    />
  );
}

/** The shipped HTML list, the classic list, or a custom dock dashboard beside the CLI. */
function AgentsListRow() {
  const org = useOrgStore((s) => s.activeOrgSlug);
  const listing = usePaneStore((s) => s.listing);
  useDashboardProfileStore((s) => s.profile);
  const chosen = dockChoice();
  const builtin = builtinAgentListRef(org ?? "");
  const builtinKey = dashboardRefKey(builtin);
  const dashboards = (listing?.panes ?? []).filter(
    (p) =>
      p.dashboardRef &&
      p.declaration?.slots.includes("dock") &&
      dashboardRefKey(p.dashboardRef) !== builtinKey,
  );
  const chosenKey = chosen ? dashboardRefKey(chosen) : null;
  const listed = dashboards.some((p) => dashboardRefKey(p.dashboardRef!) === chosenKey);
  const source = usePaneStore((s) => (chosenKey ? s.sourceRows[chosenKey] : undefined));
  const shown = [
    ...dashboards,
    ...(!listed && chosenKey !== builtinKey && source?.entry ? [source.entry] : []),
  ];
  const gone = chosen && chosenKey !== builtinKey && source && !source.entry ? chosen : null;
  if (!org) return null;
  const options = [
    { value: builtinKey, label: "Agent list (HTML)" },
    { value: "native", label: "Built-in list (classic)" },
    ...shown.map((p) => ({
      value: dashboardRefKey(p.dashboardRef!),
      title: p.path,
      label: (
        <>
          {p.title}{" "}
          <span className="opacity-70">· {dashboardIdentity(p.dashboardRef, p).label}</span>
        </>
      ),
    })),
    ...(gone
      ? [
          {
            value: chosenKey!,
            label: (
              <>
                {gone.id} <span className="opacity-70">· missing</span>
              </>
            ),
          },
        ]
      : []),
  ];
  return (
    <RadioGroupRow
      label={GENERAL_ROW_SPECS.dockDashboard.label}
      ariaLabel={GENERAL_ROW_SPECS.dockDashboard.label}
      testId="settings-dock-dashboard"
      value={chosenKey ?? "native"}
      options={options}
      onChange={(v) => {
        const ref =
          v === "native"
            ? "native"
            : v === builtinKey
              ? builtin
              : (shown.find((p) => dashboardRefKey(p.dashboardRef!) === v)?.dashboardRef ?? gone);
        if (ref) void setSlotDashboard(org, "dock", ref).catch(console.error);
      }}
      gridClassName="grid-cols-2"
      buttonPaddingClassName="px-2 py-1.5"
      footer={
        <p className="mt-1 text-[10px] text-muted-foreground">
          The list beside the CLI, for this organization. If a dashboard doesn&apos;t load, the
          built-in list shows instead.
        </p>
      }
    />
  );
}
