// SPDX-License-Identifier: MIT
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * The hidden-team boundary. A warm team stays mounted, hidden by an inline `display:none`, so a
 * global listener it installs or a document-wide lookup by a repeating id (pane, viewer, HUD,
 * tray chip) can act for a team the user is not looking at. Every such site in the retained
 * team trees is either gated by the one rule (`useScopeActive` / `isScopeActive` for listeners
 * and per-team identity, `queryShown` / `queryAllShown` / `isInHiddenTeam` for DOM lookups) or
 * listed here with the reason it needs no gate. The counts are exact: a new site in a listed
 * file, or in a new file, fails until it is gated or listed with a reason.
 *
 * A body portal escapes the team wrapper, so `isInHiddenTeam` cannot see it: a portal rendered from
 * a team subtree goes through `ScopedPortal` (or a gated Radix primitive in `components/ui`), and
 * any other raw `createPortal(` is listed below with the reason it cannot show a hidden team's UI.
 */
const SOURCE_ROOT = "src";

/** Not rendered inside any team view: test harnesses and mocks. */
const NOT_PRODUCTION =
  /(?:^|\/)(?:__tests__|__test-utils__|mock|test-utils)(?:\/|\.)|\.test\.tsx?$/;

const GLOBAL_SITE =
  /\b(?:document|window)\.(?:addEventListener|querySelectorAll|querySelector|getElementById)\s*(?:<[^>(]*>)?\(/g;

const SITES: Record<string, { count: number; reason: string }> = {
  "src/features/dashboard/components/hosted-menu-rect.ts": {
    count: 1,
    reason:
      "window message listener; answers only the frame whose window sent it (event.source), never by a repeating id.",
  },
  "src/features/dashboard/components/html-viewer.tsx": {
    count: 3,
    reason:
      "visibilitychange (page-level), focusin that only records the focus owner (no action), and a message listener matched on event.source.",
  },
  "src/features/dashboard/components/markdown-viewer.tsx": {
    count: 1,
    reason:
      "removes the singleton #hljs-theme style tag; one element for the whole page, not per team.",
  },
  "src/features/dashboard/components/menu-action-delivery.ts": {
    count: 1,
    reason: "window message listener; answers only the frame whose window sent it (event.source).",
  },
  "src/features/dashboard/components/use-diagram-container.ts": {
    count: 1,
    reason: "removes the diagram container it created itself under a generated unique id.",
  },
  "src/features/dashboard/components/verb-confirmation-provider.tsx": {
    count: 1,
    reason:
      "modal confirmation suspends every iframe on the page on purpose, hidden teams included, while the dialog is up.",
  },
  "src/features/dashboard/components/viewer-chrome.ts": {
    count: 1,
    reason:
      "Gated: the keydown handler returns first when its scroll root is in a hidden team (isInHiddenTeam).",
  },
  "src/features/notes/components/pane-sheet.tsx": {
    count: 1,
    reason:
      "Gated: the outside-press handler returns when the sheet is in a hidden team (isInHiddenTeam).",
  },
  "src/features/shell/host-menu-layer.tsx": {
    count: 1,
    reason: "window blur for the one host menu layer; app-level singleton, not a team subtree.",
  },
  "src/features/shell/native-text-menu.tsx": {
    count: 3,
    reason:
      "app-level singleton native text menu, mounted once by the shell; acts on the event target, never on an id lookup.",
  },
  "src/features/shell/right-pane/live-list.tsx": {
    count: 2,
    reason: "window focus and visibilitychange refreshes; they re-read data and take no input.",
  },
  "src/features/shell/right-pane/right-pane-stack.tsx": {
    count: 3,
    reason:
      "release listeners (pointerup, pointercancel, blur) live only for the user's own held-pointer drag in this pane; they end the drag and its resize shield and take no other input, and the gesture can outlast a team switch until the pointer is released.",
  },
  "src/features/shell/slots/file-nav-slot-host.tsx": {
    count: 1,
    reason: "window resize measurement; no input, no id lookup.",
  },
  "src/features/viewer/components/agents-popover.ts": {
    count: 7,
    reason:
      "an agents popover, anchored to the control the user opened; closes on its own outside press and acts on its own DOM, never by id.",
  },
  "src/features/viewer/components/agents-tree-view.tsx": {
    count: 1,
    reason:
      "closes this view's own filter menu on an outside press; only while that menu is open, on its own field ref.",
  },
  "src/features/viewer/components/fullscreen-overlay-shell.tsx": {
    count: 4,
    reason:
      "Gated: the Escape handler returns when its container is in a hidden team (isInHiddenTeam); the others track the opener press and the frame-escape event on its own container, measure the singleton titlebar and resize.",
  },
  "src/features/viewer/components/new-terminal-chooser.tsx": {
    count: 1,
    reason:
      "window event matched on the detail's teamKey and workspaceId, so a hidden team's chooser ignores the shown team's request.",
  },
  "src/features/viewer/components/overlay-shell.tsx": {
    count: 2,
    reason:
      "Gated: useOverlayEscape runs only while `enabled`; its sole caller, the diff overlay, passes useScopeActive.",
  },
  "src/features/viewer/components/scratch-note-panel.tsx": {
    count: 2,
    reason: "scroll and resize reposition this panel's own selection menu; no input, no id lookup.",
  },
  "src/features/viewer/components/source-diff-overlay.tsx": {
    count: 2,
    reason:
      "focusin and pointerdown only record whether focus was last inside this overlay's own shell ref (no action); its keys and watch use useScopeActive.",
  },
  "src/features/viewer/components/story-hud.tsx": {
    count: 2,
    reason:
      "Gated: the Escape keydown and the outside pointerdown both install only while ownsKeys (useScopeActive).",
  },
  "src/features/viewer/components/terminal-tile-group.tsx": {
    count: 2,
    reason: "drag-end listeners installed only for the duration of the user's own pointer drag.",
  },
  "src/features/viewer/hooks/use-session-prompt.ts": {
    count: 1,
    reason: "visibilitychange (page-level); no input, no id lookup.",
  },
  "src/features/viewer/hooks/use-sprint-registry.ts": {
    count: 1,
    reason: "window focus refresh of registry data; no input, no id lookup.",
  },
  "src/features/viewer/hooks/use-tray-reorder.ts": {
    count: 8,
    reason:
      "pointer tracking and drag-session listeners for the user's own tray drag; the tile-zone lookup goes through queryAllShown.",
  },
  "src/features/viewer/hooks/use-workspace-card-drag.ts": {
    count: 7,
    reason: "listeners installed only for the duration of the user's own card drag.",
  },
  "src/features/viewer/hooks/use-workspace-cycle-intercept.ts": {
    count: 1,
    reason:
      "one listener, mounted once by the single CLI host; it compares the host's repo path to the active team and does not exist per team.",
  },
  "src/features/viewer/lib/agents-list-focus.ts": {
    count: 1,
    reason:
      "the list id embeds the normalized teamKey, which is not guaranteed unique across orgs; the only mount is the single dock, so the lookup finds one list.",
  },
  "src/features/viewer/lib/attention-alerts-controller.ts": {
    count: 3,
    reason:
      "singleton controller: window pointer and key capture for dismissing alerts, and a page-wide 'is a dialog open' scan; none is per team.",
  },
  "src/features/viewer/lib/focus-indicator.ts": {
    count: 4,
    reason:
      "singleton focus-ring tracker for the whole window; reads the event target, no id lookup.",
  },
  "src/features/viewer/lib/os-attention.ts": {
    count: 2,
    reason: "singleton window focus/blur tracker for OS attention.",
  },
  "src/features/viewer/lib/tile-question-focus.ts": {
    count: 3,
    reason: "Gated: every lookup filters its matches through isInHiddenTeam.",
  },
  "src/features/viewer/stores/session-timers-store.ts": {
    count: 1,
    reason: "pagehide flush; page-level.",
  },
  "src/features/viewer/views/team-workspace-view.tsx": {
    count: 4,
    reason:
      "Gated: keydown and mousedown start with `!isScopeActive(teamKey)`; the codesense:find listener acts on its own viewer ref, and contextmenu only suppresses the browser menu.",
  },
  "src/features/viewer/views/use-drag-resize.ts": {
    count: 4,
    reason: "listeners installed only for the duration of the user's own divider drag.",
  },
  "src/app/App.tsx": {
    count: 2,
    reason:
      "app-level singleton mounted once: a capture click handler that opens http links externally, and the close-tab shortcut which reads the active team from the store, never a team subtree.",
  },
  "src/app/e2e-content-nav-control.ts": {
    count: 1,
    reason:
      "e2e control surface installed only under the test harness; not rendered in a team view.",
  },
  "src/app/e2e-terminal-webgl-control.ts": {
    count: 1,
    reason:
      "e2e control surface installed only under the test harness; not rendered in a team view.",
  },
  "src/app/hooks/use-dashboard-stream.ts": {
    count: 2,
    reason:
      "window focus and visibilitychange only mark the stream stale (the next live delta restores it); mounted once by App, no input.",
  },
  "src/app/hooks/use-shell-scroll-containment.ts": {
    count: 1,
    reason:
      "window scroll pin for the whole shell; mounted once by App, acts on the scrolled element.",
  },
  "src/app/install-global-error-handlers.ts": {
    count: 2,
    reason: "window error reporting; page-level, no input, no id lookup.",
  },
  "src/app/renderer-heartbeat.ts": {
    count: 1,
    reason:
      "reads the singleton #root's child count for the liveness heartbeat; app-wide, read-only, no team data.",
  },
  "src/features/dashboard-publishing/use-dashboard-manager.ts": {
    count: 2,
    reason: "window focus and visibilitychange refresh data; no input, no id lookup.",
  },
  "src/features/dashboard/lib/bridge-timing.ts": {
    count: 5,
    reason:
      "page lifecycle (visibilitychange, freeze, resume, pagehide, pageshow), installed once per renderer and only when debug logging is on; writes a log line, no action, no id lookup.",
  },
  "src/features/dashboard/lib/configuration-live.ts": {
    count: 1,
    reason: "visibilitychange refresh of data; page-level.",
  },
  "src/features/dashboard/lib/prefs-live.ts": {
    count: 1,
    reason: "visibilitychange refresh of data; page-level.",
  },
  "src/features/git-sync/lib/refresh-scheduler.ts": {
    count: 1,
    reason: "window focus schedules a git-sync refresh; page-level, no input.",
  },
  "src/features/git-sync/lib/use-context-git-source.ts": {
    count: 2,
    reason: "online and offline status refresh; page-level, no input.",
  },
  "src/features/inbox/components/composer.tsx": {
    count: 2,
    reason:
      "the outside press closes this composer's own popup, and the suggestion id is derived from the instance's own list id; the Inbox is not a retained team view.",
  },
  "src/features/inbox/components/inbox-splitter.tsx": {
    count: 4,
    reason: "pointer listeners installed only for the duration of the user's own splitter drag.",
  },
  "src/features/inbox/hooks/use-document-visible.ts": {
    count: 1,
    reason: "visibilitychange subscription; page-level.",
  },
  "src/features/inbox/hooks/use-terminal-split-inset.ts": {
    count: 3,
    reason: "measures the singleton titlebar and window resize; no input.",
  },
  "src/features/machines/components/remote-file-tree.tsx": {
    count: 1,
    reason: "window focus refreshes data; no input, no id lookup.",
  },
  "src/features/machines/lib/machine-memory-boot.ts": {
    count: 1,
    reason: "pagehide flush; page-level.",
  },
  "src/features/org/components/context-attachments.tsx": {
    count: 1,
    reason: "app event that refreshes data; no input.",
  },
  "src/features/org/components/missing-context-repos.tsx": {
    count: 1,
    reason: "app event that refreshes data; no input.",
  },
  "src/features/org/hooks/use-org-discovery.ts": {
    count: 1,
    reason: "app event that refreshes data; no input.",
  },
  "src/features/settings/components/appearance-section.tsx": {
    count: 1,
    reason: "Gated: the Undo keydown returns when its pane is in a hidden team (isInHiddenTeam).",
  },
  "src/features/shell/slots/reveal-slot.ts": {
    count: 1,
    reason:
      "finds the native file nav by data-file-nav-active=true, which only the shown nav carries.",
  },
  "src/features/shell/slots/slot-debug.ts": {
    count: 2,
    reason:
      "debug builds only: per mint, a 1.5 s message listener reads theme acks and probe answers, and an ack-probe listener lives until the ack's probe reply or the mint's removal; no input.",
  },
  "src/features/team-edits/stores/use-team-edits-refresh.ts": {
    count: 1,
    reason: "window focus refreshes data; no input. A hidden team installs none (owns-scope gate).",
  },
  "src/features/viewer/components/terminal-tile-chrome.tsx": {
    count: 1,
    reason: "reminder anchor looked up by the tab's own unique id.",
  },
  "src/features/viewer/lib/cli-host.ts": {
    count: 1,
    reason: "finds the one global CLI host element; a singleton, not per team.",
  },
  "src/main.tsx": {
    count: 7,
    reason: "bootstrap: mounts React on the single root element.",
  },
  "src/shared/components/positioned-context-menu.tsx": {
    count: 2,
    reason:
      "the Escape listener is installed only while the menu is open and its owning team is shown (`useOwnsScope`), and the menu closes when its team is hidden; the resize listener only re-measures the open menu and takes no input.",
  },
  "src/shared/hooks/use-gesture-pane-scope.ts": {
    count: 3,
    reason:
      "one module-level gesture listener set shared by every caller; decides from the gesture target, not per team.",
  },
  "src/shared/hooks/use-mcp-actions.ts": {
    count: 2,
    reason:
      "mounted once by App; pointer and key presses only supersede that single app-level action.",
  },
  "src/shared/hooks/use-shortcut-handler.ts": {
    count: 1,
    reason:
      "the one app-level shortcut handler, mounted once by App; reads the active team from the store.",
  },
  "src/shared/hooks/use-window-focused.ts": {
    count: 4,
    reason: "records window and page focus state; no action, no id lookup.",
  },
  "src/shared/lib/popover-bounds.ts": {
    count: 2,
    reason: "measures the singleton workbench and workspace bar; no input.",
  },
  "src/shared/lib/send-to-terminal.ts": {
    count: 1,
    reason:
      "focus hop skips any textarea whose container is display:none, so a hidden team's terminal is never focused.",
  },
  "src/shared/lib/xterm-webgl-diag.ts": {
    count: 3,
    reason: "diagnostic page-level listeners; no input.",
  },
  "src/shared/terminal/terminal-manager.ts": {
    count: 3,
    reason:
      "the singleton parking element and window blur/focus bookkeeping over owned displays; no team input.",
  },
};

const PORTAL_SITE = /\bcreatePortal\s*(?:<[^>(]*>)?\(/g;

const PORTAL_SITES: Record<string, { count: number; reason: string }> = {
  "src/shared/lib/warm-scopes/owning-scope.tsx": {
    count: 1,
    reason:
      "the shared ScopedPortal wrapper itself: renders nothing while the owning team is hidden.",
  },
  "src/features/shell/host-menu-layer.tsx": {
    count: 1,
    reason: "one app-level layer mounted outside every team; it serves the single shown request.",
  },
  "src/features/shell/right-pane/use-reparenting-panel-bodies.tsx": {
    count: 1,
    reason:
      "portals a panel body into a container inside the right-pane layout, not to the body; the layout is in the team's own right-pane host.",
  },
  "src/features/shell/right-pane/right-pane-stack.tsx": {
    count: 1,
    reason:
      "the resize shield belongs to the held-pointer gesture on this splitter: it is dropped on release, and a gesture started in a team can outlast a switch until then.",
  },
  "src/features/shell/components/workbench.tsx": {
    count: 1,
    reason: "the crash-recovery portal renders only when its team is `shown`.",
  },
  "src/features/shell/slots/slot-dashboard-host.tsx": {
    count: 1,
    reason:
      "portals a failure fallback into its own slot's mount target, which the team wrapper hides.",
  },
  "src/features/dashboard/components/verb-confirmation-provider.tsx": {
    count: 1,
    reason: "one app-level provider mounted in main.tsx outside every team; one request at a time.",
  },
  "src/features/viewer/components/workspace-tray.tsx": {
    count: 1,
    reason:
      "fills the title-bar rail slot the shell registers; the one shared tray (`SharedWorkspaceTray`) is mounted once for the active team, so no hidden team renders into it.",
  },
  "src/features/viewer/views/team-workspace-view.tsx": {
    count: 1,
    reason:
      "portals the team's right pane into that team's own right-pane host in the workbench, which is hidden with the team; the pane keeps its state, so it cannot unmount.",
  },
};

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (NOT_PRODUCTION.test(relative(process.cwd(), path))) continue;
    if (statSync(path).isDirectory()) out.push(...sourceFiles(path));
    else if (/\.tsx?$/.test(name)) out.push(path);
  }
  return out;
}

/** Counts every global listener / document-wide lookup per file, from `{ path: source }`. */
function countSites(files: Record<string, string>): Record<string, number> {
  const found: Record<string, number> = {};
  for (const [path, text] of Object.entries(files)) {
    const count = (text.match(GLOBAL_SITE) ?? []).length;
    if (count > 0) found[path] = count;
  }
  return found;
}

function sourceTexts(): Record<string, string> {
  const files: Record<string, string> = {};
  for (const file of sourceFiles(join(process.cwd(), SOURCE_ROOT))) {
    files[relative(process.cwd(), file)] = readFileSync(file, "utf8");
  }
  return files;
}

function actualSites(): Record<string, number> {
  return countSites(sourceTexts());
}

/** Counts every raw `createPortal(` per file, from `{ path: source }`. */
function countPortals(files: Record<string, string>): Record<string, number> {
  const found: Record<string, number> = {};
  for (const [path, text] of Object.entries(files)) {
    const count = (text.match(PORTAL_SITE) ?? []).length;
    if (count > 0) found[path] = count;
  }
  return found;
}

const listedPortals = () =>
  Object.fromEntries(Object.entries(PORTAL_SITES).map(([f, s]) => [f, s.count]));

const listedCounts = () => Object.fromEntries(Object.entries(SITES).map(([f, s]) => [f, s.count]));

describe("hidden-team boundary", () => {
  it("every global listener and document-wide lookup is gated or listed with a reason", () => {
    expect(actualSites()).toEqual(listedCounts());
  });

  it("every raw createPortal goes through ScopedPortal or is listed with a reason", () => {
    expect(countPortals(sourceTexts())).toEqual(listedPortals());
  });

  it("a new raw createPortal fails the guard, in an existing file and in a new one", () => {
    const file = "src/features/agents/components/agents-tab.tsx";
    const real = readFileSync(join(process.cwd(), file), "utf8");
    const site = "createPortal(<div />, document.body);";
    for (const counts of [
      { ...countPortals(sourceTexts()), ...countPortals({ [file]: `${real}\n${site}` }) },
      { ...countPortals(sourceTexts()), ...countPortals({ "src/new.tsx": site }) },
    ]) {
      expect(counts).not.toEqual(listedPortals());
    }
  });

  it("every allowlist entry says why it needs no gate", () => {
    for (const [file, site] of [...Object.entries(SITES), ...Object.entries(PORTAL_SITES)]) {
      expect(site.reason.trim().length, file).toBeGreaterThan(20);
    }
  });

  // The guard must bite: an ungated site added to an existing retained file, or to a new file
  // anywhere under src/, changes the counts and fails the comparison above.
  describe.each([
    ["an ungated keydown listener", 'document.addEventListener("keydown", onKey);'],
    ["a singular querySelector", 'document.querySelector("[data-viewer-content]");'],
    ["a querySelectorAll", 'document.querySelectorAll("[data-viewer-content]");'],
    ["a getElementById", 'document.getElementById("story-hud-toggle");'],
    ["a typed window listener", 'window.addEventListener<"pointerdown">("pointerdown", on);'],
  ])("%s", (_name, site) => {
    it("fails in an existing retained file", () => {
      const file = "src/features/viewer/views/team-workspace-view.tsx";
      const real = readFileSync(join(process.cwd(), file), "utf8");
      const counts = { ...actualSites(), ...countSites({ [file]: `${real}\n${site}` }) };
      expect(counts).not.toEqual(listedCounts());
    });
    it.each([
      "src/features/viewer/components/new-file.tsx",
      "src/features/agents/components/new-file.tsx",
      "src/shared/terminal/new-file.ts",
    ])("fails in a new file at %s", (file) => {
      const counts = {
        ...actualSites(),
        ...countSites({ [file]: `export const x = () => { ${site} };` }),
      };
      expect(counts).not.toEqual(listedCounts());
    });
  });
});
