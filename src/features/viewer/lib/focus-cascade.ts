// SPDX-License-Identifier: MIT
import { currentContextNavigationRevision } from "@/shared/lib/context-navigation";
import { recordContextChange } from "./context-switch";
import {
  activeForegroundTiles,
  getViewerState,
  type Workspace,
} from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { supersedes, type FocusIntent, type Presence } from "./cli-workspace/activation";
import { getActiveTeamKey } from "@/shared/lib/team-key";
import { isTerminalPopupFocused } from "@/shared/lib/focus-utils";
import { queryShown } from "@/shared/lib/warm-scopes";
import { cliHostRoot } from "./cli-host";
import { isTerminalColumnShown } from "./terminal-column";
import {
  pickMruResumeTarget,
  railsFromOrder,
  resolveAdjacentEligibleRail,
  resolveSelectedRailKey,
} from "./group-workspaces-by-epic";

/**
 * Focus the CLI's first visible xterm textarea (terminal panels hide via the
 * `style.display` attribute when inactive). Scoped to the CLI's host, so a terminal drawn
 * elsewhere (the Agents pane's quick view) never takes it.
 */
export function focusVisibleTerminal(): void {
  const panels = cliHostRoot()?.querySelectorAll<HTMLElement>(".xterm textarea") ?? [];
  for (const el of panels) {
    if (!isHiddenFromFocus(el)) {
      el.focus();
      return;
    }
  }
}

/** A hidden or inert ancestor cannot receive terminal focus. */
function isHiddenFromFocus(el: HTMLElement): boolean {
  for (let n: HTMLElement | null = el; n; n = n.parentElement) {
    if (n.style?.display === "none" || n.inert || n.getAttribute("aria-hidden") === "true")
      return true;
  }
  return false;
}

/**
 * SW-EW-33: focus the visible terminal textarea WITHIN one workspace tile,
 * scoped to its `data-pane-id` frame. In a mosaic ALL panes' terminals
 * are simultaneously visible, so the global `focusVisibleTerminal` would grab the
 * first one — keyboard pane navigation (Cmd+Opt+arrows) needs the cursor to land
 * in the pane it just highlighted so the user can type there immediately.
 * Multi-tab workspaces hide inactive tabs via `display:none`, so we skip those
 * and focus the active tab's terminal. Scoped to the CLI's host, which shows one
 * team's tiles: workspace ids are not unique across teams, so a document-wide
 * match could land elsewhere. Returns whether focus actually moved.
 */
export function focusWorkspaceTerminal(workspaceId: string): boolean {
  if (typeof document === "undefined") return false;
  for (const frame of cliHostRoot()?.querySelectorAll<HTMLElement>("[data-pane-id]") ?? []) {
    if (frame.getAttribute("data-pane-id") !== workspaceId) continue;
    if (isHiddenFromFocus(frame)) continue; // a hidden team's frame (same id) — skip
    // The active tab's terminal first: in a tab grid every cell is visible, so
    // "the first visible terminal" would be the first cell, not the selected one.
    const active = frame.querySelector<HTMLElement>("[data-pane-active] .xterm textarea");
    if (active && !isHiddenFromFocus(active)) {
      active.focus();
      return true;
    }
    for (const el of frame.querySelectorAll<HTMLElement>(".xterm textarea")) {
      if (!isHiddenFromFocus(el)) {
        el.focus();
        return true;
      }
    }
    // A visible frame with no focusable terminal (a viewer pane, or a terminal
    // still spawning) — keep scanning; another frame for this id may qualify.
  }
  return false;
}

/** OC-5/OC-9c: the per-team activation authority — the monotonic generation plus
 *  the intent and target it was minted with. The coordinator is its sole owner
 *  (call sites never mint or pass a generation), so an A→B→A race resolves by a
 *  strictly-increasing stamp (the FE analogue of the Rust generation fence), and
 *  the panel's attach-time reveal can consult the latest intent instead of
 *  deciding independently. Module-scoped, deliberately NOT a store field or
 *  persisted state. Presentation may observe the activation gesture without
 *  copying this record. One record per team keeps focus intent to a single
 *  owner (OC-1). */
interface ActivationRecord {
  readonly generation: number;
  readonly navigationRevision: number;
  readonly presence: Presence;
  readonly focus: FocusIntent;
  readonly workspaceId: string | undefined;
}

const activationGenerations = new Map<string, ActivationRecord>();

// Presentation can observe an explicit foreground activation even when the
// active store id is unchanged. The existing coordinator remains the only
// activation authority; listeners carry no state and never persist anything.
const foregroundActivationListeners = new Set<(teamKey: string, workspaceId: string) => void>();
export function subscribeForegroundWorkspaceActivation(
  listener: (teamKey: string, workspaceId: string) => void,
): () => void {
  foregroundActivationListeners.add(listener);
  return () => {
    foregroundActivationListeners.delete(listener);
  };
}

function mintActivation(
  teamKey: string,
  presence: Presence,
  focus: FocusIntent,
  workspaceId: string | undefined,
): number {
  const generation = (activationGenerations.get(teamKey)?.generation ?? 0) + 1;
  activationGenerations.set(teamKey, {
    generation,
    presence,
    focus,
    workspaceId,
    navigationRevision: currentContextNavigationRevision(),
  });
  return generation;
}

/** OC-9c: the current activation generation for a team — the stamp the terminal
 *  panel captures at attach-initiation so a later {@link shouldFocusTerminalOnReveal}
 *  can tell whether a newer activation has landed since. `0` = never activated. */
export function currentActivationGeneration(teamKey: string): number {
  return activationGenerations.get(teamKey)?.generation ?? 0;
}

/**
 * OC-9c: the single focus authority the panel's attach-time reveal consults
 * instead of deciding on `?? true`. Suppress-only — it never grants focus a caller
 * wasn't already going to take; it only vetoes a reveal the coordinator knows has
 * been superseded. Given the generation the panel captured when it began attaching
 * (`sinceGeneration`), the reveal is allowed unless a strictly-newer activation has
 * landed whose intent is NOT "focus this same terminal" (a different workspace, a
 * non-`focus-terminal` intent, or a background warm). With no record, or nothing
 * newer landed, it returns `true` so the coordinator-silent DOM paths (mosaic
 * reparent, cold/warm remount) keep their historical autofocus. A preserve-focus
 * request also vetoes a reveal that began during that same activation.
 */
export function shouldFocusTerminalOnReveal(
  teamKey: string,
  workspaceId: string,
  sinceGeneration: number,
): boolean {
  const latest = activationGenerations.get(teamKey);
  if (!latest) return true; // coordinator never activated this team — do not interfere
  if (latest.navigationRevision !== currentContextNavigationRevision()) return false;
  if (latest.focus === "preserve-focus") return false;
  if (!supersedes(sinceGeneration, latest.generation)) return true; // nothing newer — still current
  // A strictly-newer activation landed: allow only if it also wants THIS terminal focused.
  return (
    latest.presence === "foreground" &&
    latest.focus === "focus-terminal" &&
    latest.workspaceId === workspaceId
  );
}

/** OC-5: intent for {@link activateAndFocus}. `presence`/`focus` are the OC-4
 *  contract values; `workspaceId` is the explicit target and, when omitted, the
 *  team's active workspace is read after the caller's synchronous store mutation
 *  has committed. */
export interface ActivateAndFocusRequest {
  readonly teamKey: string;
  readonly presence?: Presence;
  readonly focus?: FocusIntent;
  readonly workspaceId?: string;
  /** Skip the focus if, by the time it lands, an editable control inside the
   *  target tile holds focus (a tab-rename input opened by a double-click). */
  readonly yieldToEditing?: boolean;
}

/**
 * OC-5 — the single activation-focus coordinator entry point every foreground
 * path routes through (keyboard cycle / jump / flip, epic-pill MRU resume, direct
 * chip click). Consolidates what used to be a per-call-site `<store mutation> +
 * focusActiveWorkspaceTerminal` pair plus the panel's mount-time autofocus.
 *
 * Contract-bound and non-blocking:
 *  - **Coherence + intent gate (UX-DR2):** a `background` warm never focuses, and
 *    only an explicit `focus-terminal` intent moves the cursor — so a background
 *    open / control / doc-open (`preserve-focus` / `focus-elsewhere`) can never
 *    steal terminal focus by omission.
 *  - **Generation fence (NFR14):** the entry mints the next generation
 *    atomically; the deferred callback re-checks it via the contract `supersedes`
 *    primitive and no-ops if a newer activation has superseded it — so a stale
 *    A-focus can't land after B has won.
 *  - **Observable readiness, no busy-wait (NFR8 / AC7):** work is deferred to a
 *    single animation frame (after the store commit and DOM paint), never a
 *    fixed timeout or spin-loop, so activation never stalls the UI thread.
 *
 * The `terminalVisible` guard keeps focus off an off-screen (collapsed / viewer-
 * layout) terminal. A nav that RESTORES a minimized workspace remounts its
 * terminal, so the textarea isn't in the DOM this frame and this no-ops for that
 * path — the panel's attach-time focus lands the cursor there instead.
 */
export function activateAndFocus(request: ActivateAndFocusRequest): void {
  const {
    teamKey,
    presence = "foreground",
    focus = "focus-terminal",
    workspaceId,
    yieldToEditing = false,
  } = request;
  // Resolve the target ONCE, here at mint: the recorded authority (which the panel
  // consults) and the rAF focus below must name the same workspace, so there is a
  // single resolution point rather than two that could drift. The caller's store
  // mutation has already committed, so the active slot is the post-activation one.
  const targetWorkspaceId = workspaceId ?? getViewerState().activeWorkspace?.[teamKey];
  // Mint on EVERY activation — even one that will not focus — so a background
  // warm or a `focus-elsewhere` / `preserve-focus` intent SUPERSEDES any still-
  // pending terminal focus from a prior activation. Otherwise that stale focus
  // would land a frame later and steal focus back from the newer, non-terminal
  // action (AC4). The gate is checked AFTER minting for exactly this reason.
  const generation = mintActivation(teamKey, presence, focus, targetWorkspaceId);
  if (presence === "foreground" && targetWorkspaceId) {
    for (const listener of foregroundActivationListeners) listener(teamKey, targetWorkspaceId);
  }
  if (presence === "background" || focus !== "focus-terminal") return;
  const revision = currentContextNavigationRevision();
  requestAnimationFrame(() => {
    const latest = currentActivationGeneration(teamKey);
    if (
      supersedes(generation, latest) ||
      getActiveTeamKey() !== teamKey ||
      currentContextNavigationRevision() !== revision
    )
      return; // a newer activation superseded this one
    const state = getViewerState();
    if (!state.terminalVisible) return; // terminal not on screen
    if (!targetWorkspaceId) return;
    if (yieldToEditing && isEditingInTile(targetWorkspaceId)) return;
    focusWorkspaceTerminal(targetWorkspaceId);
  });
}

/** Whether a terminal shown in `workspaceId`'s tile may take focus without a
 *  request: only when nothing holds focus, or a terminal of that same tile (or
 *  one just parked out of it) does.
 *  A tab switch the user made from the keyboard inside the terminal lands in
 *  the new tab; one made by an agent, a layout restore or a tab-strip key press
 *  leaves focus where it is (a pointer or menu activation asks for focus itself). */
export function mayTakeUnrequestedFocus(workspaceId: string): boolean {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement) || active === document.body) return true;
  // A terminal popup's terminal (the quick view) is being typed into: it keeps focus.
  if (!active.closest(".xterm") || isTerminalPopupFocused()) return false;
  // A terminal in no tile is a parked display: the tab the keys were going to.
  const frame = active.closest("[data-pane-id]:not([data-focus-pane])");
  return !frame || frame.getAttribute("data-pane-id") === workspaceId;
}

/** Whether focus sits in an editable control (not xterm's own textarea) inside
 *  the tile frame for `workspaceId`. */
function isEditingInTile(workspaceId: string): boolean {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement)) return false;
  const editable =
    active.isContentEditable ||
    active instanceof HTMLInputElement ||
    (active instanceof HTMLTextAreaElement && !active.closest(".xterm"));
  if (!editable) return false;
  const frame = active.closest("[data-pane-id]:not([data-focus-pane])");
  return frame?.getAttribute("data-pane-id") === workspaceId;
}

/**
 * SW-EW-33 / OC-5: keyboard-nav adapter — focus the now-active workspace terminal
 * with the default foreground/focus-terminal intent. A thin delegate over
 * {@link activateAndFocus} so every keyboard path (cycle / `Cmd+1..9` jump /
 * flip-to-last / agent drill) shares the one generation-fenced coordinator rather
 * than repeating the intent at each call site.
 */
export function focusActiveWorkspaceTerminal(teamKey: string): void {
  activateAndFocus({ teamKey });
}

/**
 * The workspace bar stays on screen while the CLI panel is closed, so picking a
 * workspace there (a pill, a "+N" row, an epic, ⌘1–9, the Agents control) brings
 * the panel back first and the focus that follows lands in a visible terminal.
 * It reopens IN PLACE — the bottom split, never a jump to full-screen CLI that
 * would hide the viewer the user was reading (SW-LN-B38). An open panel, split
 * or full-screen, is left exactly as it is. Returns whether it reopened.
 */
export function revealCollapsedTerminal(): boolean {
  const state = getViewerState();
  if (isTerminalColumnShown(state)) return false;
  state.revealTerminalInPlace(getActiveTeamKey());
  return true;
}

/**
 * OC-5: the tray-pill activation tuple as ONE call — activate the tray workspace,
 * then focus it through the coordinator. Every pill/chip site routes through here
 * so a new one can't activate without focusing (regressing the "type without a
 * second click" guarantee). The explicit `workspaceId` targets the just-selected
 * chip rather than re-reading the active slot.
 */
export function activateAndFocusTrayWorkspace(teamKey: string, workspaceId: string): void {
  if (!workspaceId) return;
  recordContextChange("nav", () => getViewerState().activateTrayWorkspace(teamKey, workspaceId));
  activateAndFocus({ teamKey, workspaceId });
}

/**
 * OC-6 — select an epic rail and resume its MRU story workspace (activate + focus
 * in ONE call). The shared tail of BOTH the pointer pill click and keyboard
 * prev/next-epic, so the two can never drift (AC5). `members` exposes each
 * member's id + kind (the resume policy's only inputs). Selection always moves;
 * activation happens only when the rail has an eligible target — an all-ineligible
 * rail moves the selection but activates nothing (guarded). Returns whether it
 * activated, for the caller's one-time coachmark gate.
 */
export function selectAndResumeEpicRail(
  teamKey: string,
  railKey: string,
  members: readonly Pick<Workspace, "id" | "kind">[],
  mru: readonly string[],
): boolean {
  getViewerState().setSelectedEpicGroup(teamKey, railKey);
  const target = pickMruResumeTarget(members, mru);
  if (!target) return false;
  activateAndFocusTrayWorkspace(teamKey, target);
  return true;
}

/**
 * OC-6 — keyboard prev/next-epic: move to the adjacent eligible epic rail and
 * resume its workspace through the SAME coordinator a pointer pill click uses.
 * Acts only in grouped tray mode. Reconstructs the rail order from cached store
 * state (`trayFullOrderByTeam` + `railKeyByWorkspaceId`) because the pipeline tree
 * lives only in a hook, unreachable from this imperative command path — so the
 * keyboard rail order can never diverge from the rendered rail. Non-blocking +
 * generation-fenced by construction (all focus work is the coordinator's single
 * rAF). A single or zero eligible rail is a safe no-op. Returns `true` when it
 * moved to another rail, `false` on the no-op (non-grouped mode, or no other
 * eligible rail) so the caller can surface the blocked-nav cue.
 */
export function navigateEpicRail(teamKey: string, direction: "next" | "prev"): boolean {
  const s = getViewerState();
  if (s.trayMode[teamKey] !== "grouped") return false; // epic-nav is a grouped-mode axis
  const mru = s.mruWorkspaceStack[teamKey] ?? [];
  const openById = new Map((s.workspaces[teamKey] ?? []).map((w) => [w.id, w]));
  const shadow = s.softClosedWorkspaces[teamKey] ?? {};
  const rails = railsFromOrder(
    s.trayFullOrderByTeam[teamKey] ?? [],
    s.railKeyByWorkspaceId[teamKey] ?? {},
    (id) => openById.get(id) ?? shadow[id]?.workspace,
  );
  // Resolve the effective selected rail with the SAME AC-8 fallback the tray uses
  // (workspace-tray `selectedRailKey`): a stored key that no longer maps to a live
  // rail (or an unset key when there is no Ungrouped rail) falls back to the first
  // rail — so keyboard nav starts from the rail the user actually sees selected and
  // never re-selects a stale/absent one.
  const selectedRailKey = resolveSelectedRailKey(
    rails.map((r) => r.railKey),
    s.selectedEpicDirName[teamKey] ?? "",
  );
  const rail = resolveAdjacentEligibleRail(rails, selectedRailKey, direction, mru);
  if (!rail) return false;
  selectAndResumeEpicRail(teamKey, rail.railKey, rail.members, mru);
  return true;
}

/** Focus the shown team's scratch-note editor (a warm team's is hidden, not a target). */
export function focusScratchNoteEditor(): void {
  queryShown(".scratch-note-editor")?.focus();
}

/** Focus the shown team's viewer content (a warm team's is hidden, not a target). */
export function focusViewerContent(): void {
  queryShown("[data-viewer-content]")?.focus();
}

/**
 * Move focus to the most contextually-relevant pane:
 *   1. Terminal if visible
 *   2. Scratch note if open
 *   3. Viewer content otherwise
 *
 * Used after layout-mode transitions (e.g., hiding the terminal panel
 * should move focus back to the viewer; opening a scratch note should
 * focus the editor). Runs in a `requestAnimationFrame` so the DOM has
 * settled before focus is moved.
 */
export function runFocusCascade(_teamKey: string): void {
  requestAnimationFrame(() => {
    const state = getViewerState();
    const termVisible = state.terminalVisible;
    const noteOpen = state.rightPaneOpen;

    if (termVisible) {
      focusVisibleTerminal();
    } else if (noteOpen) {
      focusScratchNoteEditor();
    } else {
      focusViewerContent();
    }
  });
}

/**
 * Toggle the scratch-note panel AND move focus in one step:
 *   - opening  (was closed) → focus the scratch-note editor
 *   - closing  (was open)   → run the cascade (terminal > note > viewer)
 *
 * Single source of truth for "toggle-and-refocus." Every caller — the
 * `viewer.toggle-right-pane` shortcut and the RightPaneLayout close button —
 * routes through here so focus behavior can never drift between surfaces.
 *
 * T.4 Phase 5: caller derives `teamKey` for the layout check.
 */
export function toggleRightPaneWithFocus(teamKey: string): void {
  const state = getViewerState();
  // F-6 AC-10: rightPaneOpen is global now; toggleRightPane takes no args.
  const wasOpen = state.rightPaneOpen;
  state.toggleRightPane();
  if (wasOpen) {
    runFocusCascade(teamKey);
  } else {
    requestAnimationFrame(focusScratchNoteEditor);
  }
}

/**
 * Toggle the bottom terminal AND move focus in one step (collapse to the
 * `viewer` layout / restore the last non-viewer mode). SW-FX-8: single source
 * of truth so the `viewer.toggle-terminal` shortcut (⌘\) and the
 * terminal-divider click can never drift.
 */
export function toggleTerminalWithFocus(teamKey: string): void {
  const state = getViewerState();
  const current = state.layoutMode ?? "shared";
  if (current === "viewer") {
    state.setLayoutMode(state.lastTerminalMode);
    // Restore focus to the workspace the user last worked in (the active-workspace
    // authority), not the first terminal in id-sorted tile order. focusWorkspaceTerminal
    // reports whether it moved focus; fall back to the visible-terminal scan only when
    // there is no active workspace or no visible frame of its own can be focused (frame
    // unmounted, still spawning, or present only in a hidden team). The fallback is the
    // prior behavior, so restore is never worse than before.
    requestAnimationFrame(() => {
      const activeId = getViewerState().activeWorkspace?.[teamKey];
      if (!activeId || !focusWorkspaceTerminal(activeId)) focusVisibleTerminal();
    });
  } else {
    state.setLayoutMode("viewer");
    runFocusCascade(teamKey);
  }
}

/**
 * SV-PBF-184: the configured terminal shortcut reveals and focuses the existing
 * CLI when invoked from non-CLI focus, and retains the ordinary toggle-to-hide
 * when focus is already inside the CLI host. With the terminal shown and no
 * foreground tile (the empty state) there is nothing to focus, so it hides.
 * Divider clicks keep the plain toggle behavior via
 * {@link toggleTerminalWithFocus}; this helper is only for the keyboard
 * shortcut so focus intent is respected.
 */
export function revealOrToggleTerminal(teamKey: string): void {
  const active = typeof document === "undefined" ? null : document.activeElement;
  const host = cliHostRoot();
  if (active instanceof HTMLElement && host && host.contains(active)) {
    toggleTerminalWithFocus(teamKey);
    return;
  }
  const state = getViewerState();
  const shown = isTerminalColumnShown(state);
  if (shown && activeForegroundTiles(state, teamKey).length === 0) {
    // Nothing in the CLI to focus (the empty state): the shortcut hides it. Focus is
    // outside the CLI, so it stays where it is (no cascade to steal a doc's caret).
    state.setLayoutMode("viewer");
    return;
  }
  if (!shown) state.revealTerminalInPlace(teamKey);
  focusActiveWorkspaceTerminal(teamKey);
}

/**
 * Toggle the left-nav collapsed state for a team. SW-FX-8: single source of
 * truth so the `viewer.toggle-nav` shortcut (⌘B) and the nav-divider click
 * stay in lockstep. No explicit focus move is needed: the collapsed sidebar is
 * marked `inert` (see team-workspace-view), so the browser pulls focus out of
 * the hidden subtree on collapse — focus is never stranded (T8).
 */
export function toggleNavCollapsed(teamKey: string): void {
  const state = getViewerState();
  state.setNavCollapsed(teamKey, !(state.navCollapsed[teamKey] ?? false));
}

// SW-EW-33: `toggleWorkspacePanelCollapsed` removed with the left
// WorkspaceManagementPanel (the bottom tray has no collapse/toggle). The
// `setWorkspacePanel*` store setters + their persisted fields are left inert (D4).
