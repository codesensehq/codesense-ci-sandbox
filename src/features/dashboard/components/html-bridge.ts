// SPDX-License-Identifier: MIT
import { openDiff } from "@/features/git-sync/lib/open-diff";
import type { OpenDiffParams } from "@/features/git-sync/lib/open-diff-types";
import { sendTextToTerminal } from "@/shared/lib/send-to-terminal";
import { fileSurfaceCall, fileSurfacePrepareView } from "@/tauri/commands";
import { buildTeamKey } from "@/shared/lib/team-key";
import { openRoutineDraft } from "@/features/routines/routines-virtual-tab";
import { messageDocKey, INITIAL_MESSAGE_HEIGHT } from "@/features/weft/lib/inbox-message-frame";
import { VERB_REGISTRY } from "../lib/verb-registry.generated";
import { liveHub, type HubFrame } from "../lib/live-hub";
import { openMessageTab } from "@/features/weft/components/inbox-message-dashboard";
import { buildMessageViewerUrl, HTML_FRAME_ALLOW } from "@/shared/lib/viewer-file-type";
import { sanitizeInboxHtml } from "@/features/weft/components/inbox-message-content";
import { isLiveInboxMessage } from "@/features/weft/lib/live-inbox-message";
import type { DocTheme } from "./doc-theme";
import {
  analyze,
  commandsList,
  commandsRegister,
  commandsRun,
  recordsCall,
  recordsPrepareView,
  mintFrame,
  releaseFrame,
  resolveFrameGrant,
  type FrameRecord,
  type FrameGrant,
  routineResult,
  routineRunNow,
  routineCancelResult,
  runSkill as runSkillCommand,
  getDependencyStatus,
  applyMetadataEdit,
  routeFileToContext,
  createFile,
  queryContext,
  bridgeDocContext,
  checkPathExists,
  bridgeResolveOpenTarget,
  updateFrontmatter,
  agentDefinitionsPrepare,
  dismissAgentMark,
  wvRosterForSession,
  wvWorkerForceClose,
  listAgentUpdates,
  sendRoleMessage,
  agentReadAgent,
  agentReadInbox,
  inboxReplyRecipient,
  listInboxRollup,
  agentReadRoles,
  getHumanStatus,
  setHumanStatus,
  replyAsHuman,
  reactAsHuman,
  loadInboxMedia,
  markInboxMessagesRead,
  markInboxMessagesUnread,
  markInboxMessagesArchived,
  sendInboxAsHuman,
  sendPromptToAgent,
  type AgentPromptResult,
  routineReadList,
  routineReadGet,
  routineReadRuns,
  routineReadRun,
  runReadList,
  runRead,
  runReadEvidence,
  runReadWorkProduct,
  listManagedWorktrees,
  listMachines,
  sessionReadUsage,
  sessionRead,
  setupStatus,
  whyLine,
  type WhyLineParams,
  storyChanges,
  STORY_CHANGES_ONLY,
  type StoryChangesOnly,
  storyTotals,
  type SessionReadParams,
  sessionReadModelRates,
  sessionSetModelRate,
  type HumanStatusValue,
  type InboxQuery,
  type InboxScope,
  type MessageKind,
  type InboxTeam,
  type MessageContentType,
  type MessageOnClick,
  type MetadataEdit,
  type FrontmatterValue,
  type LongRateArgs,
} from "@/tauri/commands";
import { switchContext } from "@/features/viewer/lib/context-switch";
import { openFileInTeam } from "@/shared/lib/open-file-in-team";
import { readDashboardPrefs, writeDashboardPrefs } from "../lib/dashboard-prefs";
import { setActiveDashboardCommands } from "../lib/dashboard-commands";
import { FRAME_PRESENTATION_UNAVAILABLE } from "../lib/frame-presentation";
import type { ContextQuery } from "@/shared/lib/context-query/types";
import {
  readAgentListSessions,
  readAgentListTeam,
} from "@/features/viewer/lib/agent-list-projection";
import { launchUnderAgent, openLaunchSheet } from "@/features/agents/lib/launch-sheet";
import { openAgentInboxForRow } from "@/features/viewer/lib/agent-inbox-actions";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";
import { useOrgStore } from "@/shared/stores/org-store";
import { locateAgentByAgentId } from "@/features/weft/lib/locate-agent";
import {
  getDashboardLaunchController,
  normalizeInitialPrompt,
  splitStoryHandle,
  type DashboardLaunchController,
  type OpenWorkspaceArg,
} from "./dashboard-launch";
import {
  openWorkspaceTab,
  sendToWorkspaceTab,
  makeRealOpenWorkspaceTabDeps,
  makeRealSendToWorkspaceTabDeps,
} from "./workspace-tab-verbs";

/**
 * SW-LD-1 — the `window.codesense` bridge allowlist.
 *
 * The bridge is a `postMessage` RPC (not raw Tauri `invoke`): an HTML doc
 * calls `window.codesense.invoke(command, args)`, the injected shim posts to the
 * parent (`html-viewer.tsx`), and the parent runs the mapped handler HERE and
 * posts the result back. See the "Architecture correction" in the SW-LD-1 story
 * for why direct `invoke` is impossible (the doc frame is cross-origin to the
 * Tauri webview).
 *
 * Two hard rules:
 *  1. **Never expose raw `invoke`.** Every callable is an explicit entry in
 *     `HTML_BRIDGE_ALLOWLIST` below — a doc-facing method name mapped to a real,
 *     typed command wrapper. Adding an entry adds API surface every doc can
 *     call; keep it small and reviewed.
 *  2. **Typed wrappers over confined commands.** The read verbs
 *     (`getDependencyStatus`, `analyze`) are side-effect-free. The MUTATING
 *     **action** verbs (`setStatus`/`routeToContext`/`draft`, the spawn-class
 *     `openWorkspace`/`runPrompt`, …) are each a typed wrapper over a real command
 *     that already confines writes to a managed target (Rust is the wall).
 *     Rust capability admission controls inbox rollups and team targets.
 *     The verb classes below document the audit/rate-guard
 *     lanes in `use-action-gate.ts`.
 *
 * ── SW-DX-6 deny/error contract (stable, typed prefixes) ─────────────────────
 * Every failure resolves the doc's `act()`/`invoke()` Promise to a REJECTION
 * whose `message` starts with one of these prefixes, so an author can branch on
 * `err.message.startsWith(...)`:
 *   - `... is not allowlisted`       — unknown verb (BridgeCommandNotAllowedError)
 *   - `denied: write budget`         — metadata-write runaway rate guard tripped
 *   - `denied: send budget`          — chat-send runaway rate guard tripped
 *   - `denied: prefs budget`         — setPrefs runaway guard tripped (SW-DNS-2; its own budget, not audited)
 *   - `denied: spawn budget`         — cap-free open (openTab, headed dir/epic) runaway rate guard tripped
 *   - `denied: route not applied (…)`— routeToContext did not route (non-managed/refused)
 *   - `busy`                         — bounded concurrency cap saturated (SW-DX-7)
 *   - `stale_cursor: …`              — a queryContext cursor's index snapshot is gone; restart paging without it
 *   - `unavailable: …`               — stubbed spawn verb / backend down
 *   - verbatim Rust reject           — managed-target confinement ("refusing to edit metadata…")
 * (Also taught by get_live_data_contract `action.errors` and the generated page SDK reference.)
 */
/** The inbox message a message doc renders: the inbox (org + team) holding it,
 *  its id and who sent it. */
export type BridgeMessageIdentity = {
  org: string;
  team: string;
  messageId: number;
  sender: string;
};

/** Trusted-side facts about the caller, supplied by the host — never by the doc. */
export type BridgeContext = {
  presentation?: (verb: string, args: unknown) => Promise<unknown>;
  frame?: FrameRecord | null;
  /** The resolved host surface theme used for the caller's initial viewer paint. */
  theme?: DocTheme | null;
  frameGrant?: FrameGrant;
  prefsSource?: import("@/tauri/commands").DashboardRef;
  /** Absolute path of the HTML doc making the call. */
  docPath?: string;
  /** Set when the caller is an `html` inbox message rendered live: its team-scoped
   *  calls act in the message's own team. */
  message?: BridgeMessageIdentity;
  /** The calling doc's launch controller. Several docs are live at once (a centre
   *  dashboard, right-pane panes, the expanded tab grid), so a launch verb acts for
   *  its own doc, never whichever mounted last. */
  controller?: DashboardLaunchController | null;
  /** The host's opener for a file in the team on screen (`openFile` uses it for
   *  a file in the active team, so the host's preview/reveal behaviour applies). */
  navigate?: (path: string, options?: { openInNewTab?: boolean; keepLayout?: boolean }) => void;
  /** SW-DNS-2: the dashboard instance whose `prefs` this caller reads and writes
   *  (a pane's own file, a centre dashboard's path, a message's id), built by the
   *  host from what it renders. Absent: the caller has no prefs. */
  prefsKey?: string;
  liveFrame?: () => Promise<HubFrame | null>;
  embedMessage?: (frame: FrameRecord) => void;
  /** Owned message boots inherit the parent’s presentation. */
  presented?: boolean;
  releaseMessage?: (frameId: string) => void;
};

/** Pages name selectors; the host alone resolves them into granted view keys. */
async function subscriptionView(args: Record<string, unknown>, ctx?: BridgeContext) {
  if (args.order !== undefined && (args.topic !== "agentList" || args.order !== "status"))
    throw new Error("invalid: agentList order must be status");
  if (args.topic === "files")
    return { viewKey: await fileSurfacePrepareView(ctx!.frame!.frameId, args) };
  if (args.topic === "records")
    return { viewKey: await recordsPrepareView(ctx!.frame!.frameId, String(args.collection)) };
  if (args.topic === "agentDefinitions") {
    const view = await agentDefinitionsPrepare(undefined, ctx?.frame?.frameId);
    liveHub.seed(view);
    return { viewKey: view.viewKey };
  }
  if (args.order === "status") return { order: "status" as const };
  return undefined;
}

export type BridgeHandler = (args: unknown, ctx?: BridgeContext) => Promise<unknown>;

/** The calling doc's launch controller; without one in the context (a caller
 *  outside any doc) the app-wide registered one. */
function launchController(ctx: BridgeContext | undefined): DashboardLaunchController | null {
  return ctx && "controller" in ctx ? (ctx.controller ?? null) : getDashboardLaunchController();
}

// SW-DX-6: route outcomes that mean the file IS in context (success). Any other
// outcome (refused / notProjectable / collision) means it was
// NOT routed → `routeToContext` rejects rather than silently resolving.
const ROUTE_OK_OUTCOMES = new Set(["routed", "alreadyRouted", "alreadyTracked"]);

// ── SW-DX-11: the `readWorkspace` extension whitelist (AC-4) ──────────────────
// Declared HERE (the allowlist module, trusted) and passed to the Rust command
// — a forged doc supplies only `path`, never this list, so widening it for a
// dashboard type is a CODE REVIEW of this constant, not a runtime toggle. Rust
// is the enforcing wall (the second gate); this is the first, reviewable one.
export const READ_WORKSPACE_EXTS = ["md", "json", "yaml", "txt"] as const;

// SW-CN-W2a/W3: bound the write response body we parse + ship across IPC to the Rust
// `errorMessageAt` (blocked reason) / `resultUrlAt` (created-resource URL) extractors.
// Each output is short (≤ 280 chars / a URL), so a large upstream body never needs to
// cross the boundary whole. 64 KiB is generous. Shared by the success + error paths.

/**
 * SW-GW-6: validate an OPTIONAL string arg — `undefined` passes through as
 * absent, a present value must be a non-empty (non-whitespace) string, else a
 * loud typed shape error. Returns the TRIMMED value so surrounding whitespace
 * never leaks into a tab title. Shared by the `openWorkspace` story target's
 * optional string fields (`title`, `agent`, ...) so they can't drift.
 */
function optionalNonEmptyString(value: unknown, errorLabel: string): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(errorLabel);
  }
  return value.trim();
}

/** SW-GW-6: a valid named-session label — lowercase alphanumeric segments joined
 *  by SINGLE hyphens (no leading/trailing/double hyphen), so the handle
 *  (`storyId#label`) and the humanized tab title never diverge, `#` stays
 *  reserved as the handle separator, and the label is a safe, bounded key. */
const STORY_SESSION_SLUG = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const STORY_SESSION_MAX_LEN = 64;

/**
 * SW-GW-11 (DRY): the ONE validator for an optional named-session label, shared
 * by `openWorkspace`'s story target and `runPrompt` so the two rules can't drift.
 * Blank/whitespace/absent → `undefined` (the main session — an absent label, not
 * an error). A present label must be a bounded lowercase slug (single-hyphen
 * segments; `#` reserved as the `storyId#label` handle separator; ≤ max len) — an
 * ill-formed value is a loud, typed error naming the caller `ctx`.
 */
function validateStorySessionLabel(session: unknown, ctx: string): string | undefined {
  if (session === undefined || String(session).trim() === "") return undefined;
  if (
    typeof session !== "string" ||
    session.length > STORY_SESSION_MAX_LEN ||
    !STORY_SESSION_SLUG.test(session)
  ) {
    throw new Error(
      `${ctx}: session must be a lowercase slug ` +
        `(a-z, 0-9, single hyphens; no '#'; ≤${STORY_SESSION_MAX_LEN} chars)`,
    );
  }
  return session;
}

/**
 * SW-GW-11 (DRY): the shared `#`-in-storyId guard. `#` is reserved as the
 * `storyId#label` handle separator, so a storyId carrying one could collide with
 * a named session's compound handle — reject at the seam rather than relying on a
 * downstream resolver miss. Shared by `openWorkspace`'s story target and (via the
 * label split) `runPrompt` — a guard the retired `fireSkill` lacked.
 */
function assertStoryIdNoHash(storyId: string, ctx: string): void {
  if (storyId.includes("#")) {
    throw new Error(`${ctx}: { storyId } must not contain '#'`);
  }
}

/**
 * SW-DX-17e: validate the optional launch session-mode at the untrusted wire
 * boundary. Absent → `undefined` (the DEFAULT `focus`/reuse path — byte-identical
 * to today). A present value must be EXACTLY `"new"` or `"focus"` (a closed set,
 * validated the same way the `target.kind` union is); anything else is a loud,
 * typed shape error. `"new"` (fork a fresh session) is STORY-ONLY — the caller
 * enforces that (a non-story `new` rejects, mirroring the `initialPrompt` guard).
 */
function validateSessionMode(value: unknown): "new" | "focus" | undefined {
  if (value === undefined) return undefined;
  if (value !== "new" && value !== "focus") {
    throw new Error("openWorkspace: sessionMode, if present, must be 'new' or 'focus'");
  }
  return value;
}

/** Resolve the Inbox team the pane acts in: a message doc's own team, else the
 *  doc's own team when the host supplied a `docPath` (looked up through `bridgeDocContext`), otherwise the
 *  user's currently active team. `null` → no team available (a pane with no
 *  docPath and no org/team selected yet). */
async function resolveInboxTeam(ctx: BridgeContext | undefined): Promise<InboxTeam | null> {
  if (ctx?.message) return { org: ctx.message.org, team: ctx.message.team };
  if (ctx?.docPath) {
    try {
      const doc = await bridgeDocContext(ctx.docPath);
      if (doc && typeof doc === "object" && "team" in doc && "org" in doc) {
        const d = doc as { org: unknown; team: unknown };
        if (typeof d.org === "string" && typeof d.team === "string" && d.org && d.team) {
          return { org: d.org, team: d.team };
        }
      }
    } catch {
      // fall through to the active-team path
    }
  }
  const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
  if (!activeOrgSlug || !activeTeamName) return null;
  return { org: activeOrgSlug, team: activeTeamName };
}

/** Architecture: the teams this page is admitted to (non-archived, and only its own team under a team-scope grant). */
function pageAdmittedTeams(ctx: BridgeContext | undefined): InboxTeam[] {
  const grant = ctx?.frameGrant ?? ctx?.frame?.grant;
  return useOrgStore
    .getState()
    .orgs.flatMap((org) =>
      org.teams
        .filter(
          (team) =>
            !team.archived &&
            (grant?.scope !== "team" ||
              (org.slug === grant.team?.org && team.name === grant.team?.team)),
        )
        .map((team) => ({ org: org.slug, team: team.name })),
    );
}

/** The admitted teams that match the inbox rollup's synced footprint: a local content directory. */
async function pageInboxTeams(ctx: BridgeContext | undefined): Promise<InboxTeam[]> {
  const roots = new Map(useOrgStore.getState().orgs.map((org) => [org.slug, org.org_root]));
  const teams = await Promise.all(
    pageAdmittedTeams(ctx).map(async (team) =>
      (await checkPathExists(pathToTeamContentDirSync(roots.get(team.org)!, team.team)))
        ? team
        : null,
    ),
  );
  return teams.filter((team): team is InboxTeam => team !== null);
}

const INBOX_TARGET_VERBS: readonly string[] = VERB_REGISTRY.filter((verb) => verb.inboxTarget).map(
  (verb) => verb.name,
);

/** Explicit targets still go through the inbox host's synced-team resolver. */
async function inboxTeam(
  ctx: BridgeContext | undefined,
  verb: string,
  args?: Record<string, unknown>,
): Promise<InboxTeam> {
  if (args && ("org" in args || "team" in args)) {
    if (
      typeof args.org !== "string" ||
      !args.org.trim() ||
      typeof args.team !== "string" ||
      !args.team.trim()
    ) {
      throw new Error(`${verb}: {org, team} must be non-empty strings given together`);
    }
    return { org: args.org.trim(), team: args.team.trim() };
  }
  const team =
    ctx?.frameGrant && INBOX_TARGET_VERBS.includes(verb)
      ? ctx.frameGrant.team
      : await resolveInboxTeam(ctx);
  if (!team) throw new Error(`${verb}: no active team`);
  return team;
}

/** Materialize the handler's target once, before native capability admission.
 * Execution receives these same args; a missing target then falls back only to
 * the resolved frame grant, rather than to a later active-team selection. */
export async function resolveBridgeCommandArgs(
  verb: string,
  args: unknown,
  ctx: BridgeContext,
): Promise<unknown> {
  if (!INBOX_TARGET_VERBS.includes(verb)) return args;
  const a = argObject(args, verb);
  const targetContext = { ...ctx, frameGrant: undefined };
  const team = await inboxTeam(targetContext, verb, a);
  return { ...a, ...team };
}

/** Keep explicit inbox-target errors local to the writes that support them. */
async function inboxWrite<T>(
  ctx: BridgeContext | undefined,
  verb: string,
  args: Record<string, unknown>,
  write: (team: InboxTeam) => Promise<T>,
): Promise<T> {
  const team = await inboxTeam(ctx, verb, args);
  try {
    return await write(team);
  } catch (error) {
    if (!("org" in args || "team" in args)) throw error;
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(message.startsWith(`${verb}:`) ? message : `${verb}: ${message}`);
  }
}

/** The doc's args as an object (`undefined`/`null` → `{}`), or a shape error. */
function argObject(args: unknown, verb: string): Record<string, unknown> {
  if (args === undefined || args === null) return {};
  if (typeof args !== "object" || Array.isArray(args)) {
    throw new Error(`${verb}: args must be an object`);
  }
  return args as Record<string, unknown>;
}

function optionalEpochMs(value: unknown, verb: string, field: string): number | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "number" || !Number.isSafeInteger(Math.floor(value))) {
    throw new Error(`${verb}: ${field}, if present, must be epoch milliseconds`);
  }
  return Math.floor(value);
}

/** A whole, positive limit that fits in the Rust side's `usize`. A fraction is refused, not truncated. */
function sessionUsageLimit(value: unknown): number | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1) {
    throw new Error("sessionUsage: limit, if present, must be a whole number of at least 1");
  }
  if (!Number.isSafeInteger(value)) {
    throw new Error("sessionUsage: limit is too large");
  }
  return value;
}

function optionalLimit(value: unknown, verb: string): number | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    throw new Error(`${verb}: limit, if present, must be a positive number`);
  }
  return Math.floor(value);
}

/** A required non-blank string arg, trimmed, from the first of `keys` present. */
function requiredId(a: Record<string, unknown>, keys: string[], message: string): string {
  const raw = keys.map((k) => a[k]).find((v) => v !== undefined);
  if (typeof raw !== "string" || !raw.trim()) throw new Error(message);
  return raw.trim();
}

/** Validate an optional `InboxScope` wire value (absent: the read model's default). */
function optionalInboxScope(value: unknown): InboxScope | undefined {
  if (value === undefined || value === null) return undefined;
  if (value === "to-human" || value === "to-agents" || value === "all") return value;
  throw new Error("inbox: scope, if present, must be 'to-human', 'to-agents' or 'all'");
}

/** Validate an optional `MessageKind` filter (absent: both kinds). */
function optionalMessageKind(value: unknown): MessageKind | undefined {
  if (value === undefined || value === null) return undefined;
  if (value === "needs-you" || value === "update") return value;
  throw new Error("inbox: kind, if present, must be 'needs-you' or 'update'");
}

/** A required inbox message id. */
function messageId(value: unknown, verb: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${verb}: {messageId} must be a message id from inbox()`);
  }
  return value;
}

const HUMAN_STATUSES: readonly HumanStatusValue[] = ["online", "away", "busy", "offline"];

/** Validate an optional `MessageOnClick` payload. `undefined` passes through
 *  as absent; a present value must be the discriminated union the Rust side
 *  stores (shape error loudly). */
function validateOnClick(value: unknown): MessageOnClick | null {
  if (value === undefined || value === null) return null;
  if (typeof value !== "object" || Array.isArray(value)) {
    throw new Error("onClick must be an object with a `type` discriminator");
  }
  const v = value as { type?: unknown; agentId?: unknown; path?: unknown };
  switch (v.type) {
    case "goto-agent": {
      if (typeof v.agentId !== "string" || !v.agentId.trim()) {
        throw new Error('onClick type="goto-agent" requires a non-empty {agentId}');
      }
      return { type: "goto-agent", agentId: v.agentId.trim() };
    }
    case "open-file": {
      if (typeof v.path !== "string" || !v.path.trim()) {
        throw new Error('onClick type="open-file" requires a non-empty {path}');
      }
      return { type: "open-file", path: v.path.trim() };
    }
    case "show-message":
      return { type: "show-message" };
    default:
      throw new Error("onClick.type must be 'goto-agent', 'open-file' or 'show-message'");
  }
}

/** Validate an optional `MessageContentType` payload. `undefined` / `null` → the
 *  renderer default. A present value must be exactly one of the three literals. */
function validateContentType(value: unknown): MessageContentType | null {
  if (value === undefined || value === null) return null;
  if (value === "plain" || value === "markdown" || value === "html") return value;
  throw new Error("contentType, if present, must be 'plain', 'markdown' or 'html'");
}

/** Validate an optional one-line `summary` (the message's list preview). Absent or
 *  blank → none; the store trims and caps it. */
function validateSummary(value: unknown, verb: string): string | null {
  if (value === undefined || value === null) return null;
  if (typeof value !== "string") throw new Error(`${verb}: summary, if present, must be a string`);
  return value.trim() ? value : null;
}

function routineKey(args: unknown): string {
  if (
    !args ||
    typeof args !== "object" ||
    !("key" in args) ||
    typeof args.key !== "string" ||
    !args.key.trim()
  ) {
    throw new Error("routine action requires a routine key");
  }
  return args.key;
}

async function readHtmlMessage(
  args: unknown,
  ctx: BridgeContext | undefined,
  verb: string,
  includeSent = false,
) {
  const a = argObject(args, verb);
  const id = messageId(a.messageId, verb);
  const team = await inboxTeam(ctx, verb, a);
  const { messages } = await agentReadInbox(team, { scope: includeSent ? "all" : "to-human" });
  const row = messages.find(
    (m) => m.messageId === id && (m.toHuman || (includeSent && m.from === "human")),
  );
  return { a, id, team, row };
}

function presentationHandler(verb: string): BridgeHandler {
  return async (args, ctx) => {
    if (!ctx?.presentation) throw new Error(FRAME_PRESENTATION_UNAVAILABLE);
    return ctx.presentation(verb, args);
  };
}

export const HTML_BRIDGE_ALLOWLIST: Record<string, BridgeHandler> = {
  showMenu: presentationHandler("showMenu"),
  showPopover: presentationHandler("showPopover"),
  focusOut: presentationHandler("focusOut"),
  focusTerminal: presentationHandler("focusTerminal"),
  // The last call replaces the dashboard's whole active set; Rust rejects an id
  // its manifest didn't declare.
  "commands.register": async (args, ctx) => {
    const ids = argObject(args, "commands.register").ids;
    if (!Array.isArray(ids) || ids.some((id) => typeof id !== "string")) {
      throw new Error("commands.register: ids must be an array of declared command ids");
    }
    if (!ctx?.frame) throw new Error("unavailable: frame identity");
    const { dashboard, active } = await commandsRegister(
      ctx.frame.origin,
      ctx.frame.token,
      ids as string[],
    );
    setActiveDashboardCommands(dashboard, active);
    return { active };
  },
  // The person's twins of command_list / command_run, over the same host registry.
  "commands.list": () => commandsList(),
  "commands.run": async (args) => {
    const a = argObject(args, "commands.run");
    if (typeof a.id !== "string" || !a.id) {
      throw new Error("commands.run: id must be a command id from commands.list()");
    }
    await commandsRun(a.id, a.args);
    return { requested: true };
  },

  ...Object.fromEntries(
    VERB_REGISTRY.filter((verb) => verb.name.startsWith("records.")).map((verb) => [
      verb.name,
      async (args: unknown, ctx?: BridgeContext) => {
        if (!ctx?.frame) throw new Error("denied: no frame grant");
        return recordsCall(
          ctx.frame.frameId,
          verb.name.slice("records.".length),
          args == null ? {} : argObject(args, verb.name),
        );
      },
    ]),
  ),
  routineResult: (args, ctx) =>
    ctx
      ? routineResult(routineKey(args), ctx.frame?.frameId, ctx.docPath)
      : routineResult(routineKey(args)),
  routineStatus: (args, ctx) =>
    ctx
      ? routineResult(routineKey(args), ctx.frame?.frameId, ctx.docPath)
      : routineResult(routineKey(args)),
  routineRefresh: (args, ctx) => {
    const key = routineKey(args);
    let variables: Record<string, string> | undefined;
    if (args && typeof args === "object" && "variables" in args && args.variables !== undefined) {
      const values = args.variables;
      if (
        !values ||
        typeof values !== "object" ||
        Array.isArray(values) ||
        Object.values(values).some((v) => typeof v !== "string")
      ) {
        throw new Error("routineRefresh variables must be a string map");
      }
      variables = Object.fromEntries(Object.entries(values).map(([k, v]) => [k, String(v)]));
    }
    return ctx
      ? routineRunNow(key, variables, ctx.frame?.frameId, ctx.docPath)
      : routineRunNow(key, variables);
  },
  routineCancel: (args) => routineCancelResult(routineKey(args)),
  // SW-OC-26: find context files by metadata — the one Rust engine. Team content
  // is footprint-wide unless the query scopes it; private items only ever come
  // from the calling doc's own org (the host supplies `docPath`; a call without
  // one sees no private items). Read-only; the Rust command validates the shape
  // (unknown keys/operators reject). The shim's `subscribeContext` re-runs this
  // on the parent's `codesense:context-changed` push.
  queryContext: (args, ctx) => {
    const page = ctx?.docPath ?? "";
    if (args === undefined || args === null) return queryContext({}, page, ctx?.frame?.frameId);
    if (typeof args !== "object" || Array.isArray(args)) {
      throw new Error("queryContext: the query must be an object");
    }
    return queryContext(args as ContextQuery, page, ctx?.frame?.frameId);
  },
  // Where the calling page lives — `{org, team, layer, contentRoot}`, or null for a
  // page outside every known content root — resolved from the host-supplied page
  // path (never one the page names), so a page can scope its own queries.
  context: async (_args, ctx) => {
    let location = await bridgeDocContext(ctx?.docPath ?? "");
    const frame = ctx?.frame;
    if (
      !location &&
      frame?.kind === "builtin" &&
      frame.grant.dashboard.startsWith("dashboard:built-in:") &&
      frame.grant.team
    ) {
      // Architecture: the authenticated bundled reader retains its team while an attached checkout is unavailable.
      location = { ...frame.grant.team, layer: "team", contentRoot: frame.contentRoot };
    }
    if (!location) return null;
    const { activeOrgSlug, activeTeamName } = useOrgStore.getState();
    return {
      ...location,
      inboxScope: ctx?.frame?.effectiveGrant?.includes("inbox:read:all-teams")
        ? "all-teams"
        : "team",
      activeTeam:
        activeOrgSlug && activeTeamName ? { org: activeOrgSlug, team: activeTeamName } : null,
      inboxTeams: await pageInboxTeams(ctx),
    };
  },
  // SW-DNS-2: this dashboard's own settings, kept in the user's app state (a
  // frame's localStorage is lost on restart). The entry is the host's `prefsKey`;
  // nothing the doc passes can name another dashboard's.
  prefs: async (_args, ctx) => readDashboardPrefs(ctx?.prefsKey, undefined, ctx?.prefsSource),
  setPrefs: async (args, ctx) =>
    writeDashboardPrefs(ctx?.prefsKey, args, undefined, ctx?.prefsSource),
  // [set_model_rate] set a model's rate from a local day, and price the usage it covers:
  // {modelPrefix, effectiveFrom (YYYY-MM-DD), inputPerMtok, outputPerMtok, cacheReadPerMtok?,
  // long?: {fromTokens, inputPerMtok, outputPerMtok, cacheReadPerMtok?}} → the count of costs it changed.
  setModelRate: (args) => {
    const a = argObject(args, "setModelRate");
    // The app refuses a key it does not know, so a misspelled price is refused here, not dropped.
    const refuseUnknown = (o: Record<string, unknown>, known: string[], what: string) => {
      const unknown = Object.keys(o).filter((k) => !known.includes(k));
      if (unknown.length > 0)
        throw new Error(`invalid: ${what} has unknown keys: ${unknown.join(", ")}`);
    };
    refuseUnknown(
      a,
      ["modelPrefix", "effectiveFrom", "inputPerMtok", "outputPerMtok", "cacheReadPerMtok", "long"],
      "setModelRate",
    );
    const modelPrefix = optionalNonEmptyString(
      a.modelPrefix,
      "invalid: setModelRate needs a model id prefix",
    );
    const effectiveFrom = optionalNonEmptyString(
      a.effectiveFrom,
      "invalid: setModelRate needs effectiveFrom as YYYY-MM-DD",
    );
    const isPrice = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
    const input = a.inputPerMtok;
    const output = a.outputPerMtok;
    if (!modelPrefix || !effectiveFrom || !isPrice(input) || !isPrice(output)) {
      throw new Error(
        "invalid: setModelRate needs {modelPrefix, effectiveFrom, inputPerMtok, outputPerMtok}",
      );
    }
    const cacheRead = a.cacheReadPerMtok ?? undefined;
    if (cacheRead !== undefined && !isPrice(cacheRead)) {
      throw new Error("invalid: setModelRate cacheReadPerMtok must be a finite number");
    }
    let long: LongRateArgs | undefined;
    if (a.long !== undefined && a.long !== null) {
      const l = argObject(a.long, "invalid: setModelRate long");
      refuseUnknown(
        l,
        ["fromTokens", "inputPerMtok", "outputPerMtok", "cacheReadPerMtok"],
        "setModelRate long",
      );
      const longCacheRead = l.cacheReadPerMtok ?? undefined;
      if (
        typeof l.fromTokens !== "number" ||
        !Number.isSafeInteger(l.fromTokens) ||
        l.fromTokens <= 0 ||
        !isPrice(l.inputPerMtok) ||
        !isPrice(l.outputPerMtok) ||
        (longCacheRead !== undefined && !isPrice(longCacheRead))
      ) {
        throw new Error(
          "invalid: setModelRate long needs {fromTokens (a whole number above zero), inputPerMtok, outputPerMtok, cacheReadPerMtok?}",
        );
      }
      long = {
        fromTokens: l.fromTokens,
        inputPerMtok: l.inputPerMtok,
        outputPerMtok: l.outputPerMtok,
      };
      if (longCacheRead !== undefined) long.cacheReadPerMtok = longCacheRead;
    }
    return sessionSetModelRate({
      modelPrefix,
      effectiveFrom,
      inputPerMtok: input,
      outputPerMtok: output,
      ...(cacheRead !== undefined ? { cacheReadPerMtok: cacheRead } : {}),
      ...(long ? { long } : {}),
    });
  },
  // ── The agents read model (one Rust read model; the MCP tools return the same
  // shapes): `sessions` is the `list_sessions` row, `agent` the `agent_get` data with
  // its running instances, `inbox` the non-destructive `inbox_list`, `roles` the
  // `role_state` queues, `updates` `list_agent_updates`, `humanStatus`
  // `get_human_status`. Team-scoped reads act on the doc's own team.
  // The app-root roster is loaded alongside: its changes are what nudge `onSessions`.
  subscribe: async (args, ctx) => {
    const a = argObject(args, "subscribe");
    const frame = await ctx?.liveFrame?.();
    if (!frame) throw new Error("unavailable: live frame is starting");
    const view = await subscriptionView(a, ctx);
    return frame.subscribe(
      String(a.topic),
      typeof a.sinceRev === "number" ? a.sinceRev : undefined,
      Array.isArray(a.window) ? { ...view, window: a.window as [number, number] } : view,
    );
  },
  page: async (args, ctx) => {
    const a = argObject(args, "page");
    const frame = await ctx?.liveFrame?.();
    if (!frame) throw new Error("unavailable: live frame is starting");
    return frame.page(String(a.topic), String(a.cursor), await subscriptionView(a, ctx));
  },
  unsubscribe: async (args, ctx) => {
    const a = argObject(args, "unsubscribe");
    await (await ctx?.liveFrame?.())?.unsubscribe(String(a.topic), await subscriptionView(a, ctx));
    return null;
  },
  setUiState: async (args, ctx) => {
    const a = argObject(args, "setUiState");
    const frame = await ctx?.liveFrame?.();
    if (!frame) throw new Error("unavailable: live frame is starting");
    // The dock's selected row is the CLI focus: the host writes it; a page calls gotoAgent.
    if (a.topic === "ui:dock" && a.patch && typeof a.patch === "object" && "selected" in a.patch)
      throw new Error("invalid: ui:dock.selected follows the CLI focus; call gotoAgent");
    return frame.writeUi(
      String(a.topic),
      a.patch as Record<string, unknown>,
      typeof a.requestId === "string" ? a.requestId : undefined,
    );
  },
  sessions: async (_args, ctx) => {
    const frame = await ctx?.liveFrame?.();
    const snapshot = frame
      ? await frame.subscribe("sessions")
      : await liveHub.snapshot("sessions", "all");
    return liveHub.joinSessions(snapshot.snapshot);
  },
  // The prepared view applies the calling frame's grant to the shared catalog.
  agents: async (_args, ctx) => {
    const view = await agentDefinitionsPrepare(undefined, ctx?.frame?.frameId);
    liveHub.seed(view);
    const frame = await ctx?.liveFrame?.();
    const snapshot = frame
      ? await frame.subscribe("agentDefinitions", undefined, { viewKey: view.viewKey })
      : view;
    return snapshot.snapshot
      .filter((r) => r.rowType === "definition")
      .map((r) => {
        const a = r.row as Record<string, unknown>;
        return {
          key: a.key,
          name: a.name,
          description: a.description,
          runtime: a.runtime,
          scope: a.scope,
          team: a.team,
          runningNow: a.runningNow ?? a.running_now,
          avatar: a.avatar ?? null,
        };
      });
  },
  agent: async (args, ctx) => {
    const key = argObject(args, "agent").key;
    if (typeof key !== "string" || !key.trim()) {
      throw new Error("agent: {key} must be an agent id from agents()");
    }
    return agentReadAgent(key.trim(), ctx?.frame?.frameId, ctx?.docPath);
  },
  inbox: async (args, ctx) => {
    const a = argObject(args, "inbox");
    const query: InboxQuery = {};
    const scope = optionalInboxScope(a.scope);
    if (scope) query.scope = scope;
    const agentId = optionalNonEmptyString(a.agentId, "inbox: agentId must be a non-empty string");
    if (agentId) query.agentId = agentId;
    const sessionId = optionalNonEmptyString(
      a.sessionId,
      "inbox: sessionId must be a non-empty string",
    );
    if (sessionId) query.sessionId = sessionId;
    const conversation = optionalNonEmptyString(
      a.conversation,
      "inbox: conversation must be a non-empty string",
    );
    if (conversation) query.conversation = conversation;
    if (a.before !== undefined) {
      if (typeof a.before !== "number" || !Number.isSafeInteger(a.before) || a.before <= 0)
        throw new Error("inbox: before must be a positive integer");
      query.before = a.before;
    }
    const role = optionalNonEmptyString(a.role, "inbox: role must be a non-empty string");
    if (role) query.role = role;
    const kind = optionalMessageKind(a.kind);
    if (kind) query.kind = kind;
    const limit = optionalLimit(a.limit, "inbox");
    if (limit) query.limit = limit;
    return agentReadInbox(await inboxTeam(ctx, "inbox", a), query);
  },
  openMessage: async (args, ctx) => {
    const { id, team, row } = await readHtmlMessage(args, ctx, "openMessage");
    if (!row) throw new Error(`openMessage: no message ${id} for the human in this team`);
    if (row.contentType !== "html") {
      throw new Error(`openMessage: message ${id} is not an html message; render it in the page`);
    }
    if (!isLiveInboxMessage(row)) {
      throw new Error(`openMessage: message ${id} is from the human; render it in the page`);
    }
    openMessageTab(team, id, row.from, { keepLayout: true });
    return { opened: true, messageId: id };
  },
  embedMessage: async (args, ctx) => {
    const a = argObject(args, "embedMessage");
    if (a.release === true) {
      if (typeof a.frameId !== "string")
        throw new Error("embedMessage: frameId is required to release an embed");
      ctx?.releaseMessage?.(a.frameId);
      return { released: true };
    }
    const { id, team, row } = await readHtmlMessage(args, ctx, "embedMessage", true);
    if (!row || row.contentType !== "html") throw new Error("embedMessage: message unavailable");
    if (!isLiveInboxMessage(row)) return { html: sanitizeInboxHtml(row.text) };
    if (!ctx?.frame || !ctx.embedMessage || ctx.frame.grant.slot !== "right-pane")
      throw new Error("embedMessage: unavailable outside a pane");
    const frame = await mintFrame({
      documentPath: messageDocKey({ ...team, messageId: id }),
      contentRoot: ctx.frame.contentRoot,
      slot: "message",
      message: { ...team, messageId: id },
      parentFrame: ctx.frame.frameId,
    });
    if (frame.parentFrame !== ctx.frame.frameId) {
      await releaseFrame(frame.frameId);
      throw new Error("denied: parent frame");
    }
    ctx.embedMessage(frame);
    const url = new URL(
      buildMessageViewerUrl(Number(new URL(frame.origin).port), team.org, team.team, id, {
        origin: frame.origin,
        theme: ctx?.theme ? JSON.stringify(ctx.theme) : undefined,
      }),
    );
    if (ctx.presented === false) url.searchParams.set("codesenseCandidate", "1");
    return {
      height: INITIAL_MESSAGE_HEIGHT,
      allow: HTML_FRAME_ALLOW,
      frameId: frame.frameId,
      url: url.toString(),
    };
  },
  inboxRollup: (_args, ctx) => listInboxRollup(ctx?.frame?.frameId, ctx?.docPath),
  roles: async (args, ctx) => {
    const a = argObject(args, "roles");
    const role = optionalNonEmptyString(a.role, "roles: role must be a non-empty string");
    const limit = optionalLimit(a.limit, "roles");
    return agentReadRoles(
      await inboxTeam(ctx, "roles", a),
      role,
      limit,
      ctx?.frame?.frameId,
      ctx?.docPath,
    );
  },
  updates: async (args, ctx) => {
    const a = argObject(args, "updates");
    const limit = optionalLimit(a.limit, "updates");
    return { updates: await listAgentUpdates(await inboxTeam(ctx, "updates", a), limit) };
  },
  humanStatus: async (args, ctx) =>
    getHumanStatus(await inboxTeam(ctx, "humanStatus", argObject(args, "humanStatus"))),
  // ── Routines, runs, worktrees and machines: each returns its MCP twin's shape
  // (named in brackets) so a dashboard and an agent read the same data.
  // [routine_list] every routine this user can see.
  routines: (_args, ctx) => routineReadList(ctx?.frame?.frameId, ctx?.docPath),
  // [routine_get] one routine in full: {key} (a routines() row's `id`).
  routine: (args, ctx) =>
    routineReadGet(
      requiredId(
        argObject(args, "routine"),
        ["key", "id"],
        "routine: {key} must be a routine id from routines()",
      ),
      ctx?.frame?.frameId,
      ctx?.docPath,
    ),
  // [routine_runs] a routine's run history, newest first: {key, limit?, before?}.
  routineRuns: (args, ctx) => {
    const a = argObject(args, "routineRuns");
    const key = requiredId(
      a,
      ["key", "id"],
      "routineRuns: {key} must be a routine id from routines()",
    );
    const limit = optionalLimit(a.limit, "routineRuns");
    const before = optionalNonEmptyString(
      a.before ?? undefined,
      "routineRuns: before, if present, must be a previous page's next_before",
    );
    return routineReadRuns(key, limit, before, ctx?.frame?.frameId, ctx?.docPath);
  },
  // [routine_run] one routine run with its routine: {runId}.
  routineRun: (args, ctx) =>
    routineReadRun(
      requiredId(
        argObject(args, "routineRun"),
        ["runId", "run_id"],
        "routineRun: {runId} must be a run_id from routineRuns()",
      ),
      ctx?.frame?.frameId,
      ctx?.docPath,
    ),
  // [list_runs] the latest outcome of every agent run, newest first: {limit?}.
  runs: (args) => runReadList(optionalLimit(argObject(args, "runs").limit, "runs")),
  // [read_run] one agent run: {runId, maxArtifactBytes?} → {outcome, transcript, artifact, artifactTruncated}.
  run: (args) => {
    const a = argObject(args, "run");
    const runId = requiredId(a, ["runId", "run_id"], "run: {runId} must be a run id from runs()");
    const max = a.maxArtifactBytes;
    if (
      max !== undefined &&
      max !== null &&
      (typeof max !== "number" || !Number.isFinite(max) || max <= 0)
    ) {
      throw new Error("run: maxArtifactBytes, if present, must be a positive number");
    }
    return runRead(runId, typeof max === "number" ? Math.floor(max) : undefined);
  },
  // [read_run_evidence] one agent run's evidence: {runId, storyId?} → {header, run,
  // perNodeResults, authority} | null. storyId defaults to the run's own story.
  runEvidence: (args) => {
    const a = argObject(args, "runEvidence");
    const runId = requiredId(
      a,
      ["runId", "run_id"],
      "runEvidence: {runId} must be a run id from runs()",
    );
    const storyId = optionalNonEmptyString(
      a.storyId ?? a.story_id ?? undefined,
      "runEvidence: storyId, if present, must be a runs() row's outcome.story_id",
    );
    return runReadEvidence(runId, storyId);
  },
  // [read_run_work_product] the branch an agent run produced: {runId} → {branch} | null.
  runWorkProduct: (args) =>
    runReadWorkProduct(
      requiredId(
        argObject(args, "runWorkProduct"),
        ["runId", "run_id"],
        "runWorkProduct: {runId} must be a run id from runs()",
      ),
    ),
  // [describe_setup] this Mac's attribution and capture status: {attribution}.
  setup: () => setupStatus(),
  // [list_worktrees] this Mac's managed worktrees: {rows, reaped}.
  worktrees: () => listManagedWorktrees(),
  // [session_read_usage] token use and spend per agent session on this Mac, read from its
  // its agent CLI's own reports (telemetry): {sinceMs?, untilMs?, sessionId?, cwd?, limit?, cursor?} → {sessions, days, totals, nextCursor?}.
  storyTotals: (args, ctx) => {
    const a = argObject(args, "storyTotals");
    if (Object.keys(a).some((key) => key !== "storyIds"))
      throw new Error("storyTotals: unknown field");
    if (!Array.isArray(a.storyIds) || a.storyIds.some((id) => typeof id !== "string"))
      throw new Error("storyTotals: storyIds must be a list of story ids");
    return storyTotals({ storyIds: a.storyIds as string[] }, ctx?.frame?.frameId, ctx?.docPath);
  },
  storyChanges: (args, ctx) => {
    const a = argObject(args, "storyChanges");
    if (
      Object.keys(a).some((key) => !["storyId", "limit", "cursor", "include", "only"].includes(key))
    )
      throw new Error("storyChanges: unknown field");
    if (
      a.limit != null &&
      (typeof a.limit !== "number" || !Number.isInteger(a.limit) || a.limit < 1)
    )
      throw new Error("storyChanges: invalid limit");
    if (a.cursor != null && typeof a.cursor !== "string")
      throw new Error("storyChanges: invalid cursor");
    if (
      a.include != null &&
      (!Array.isArray(a.include) || a.include.some((value) => value !== "ranges"))
    )
      throw new Error("storyChanges: invalid include");
    if (
      a.only != null &&
      (!Array.isArray(a.only) ||
        a.only.length === 0 ||
        a.only.some((value) => !(STORY_CHANGES_ONLY as readonly unknown[]).includes(value)))
    )
      throw new Error("storyChanges: invalid only");
    const storyId = requiredId(a, ["storyId"], "storyChanges: storyId is required");
    return storyChanges(
      {
        storyId,
        ...(a.only != null ? { only: a.only as StoryChangesOnly[] } : {}),
        ...(a.include != null ? { include: a.include as "ranges"[] } : {}),
        ...(a.limit != null ? { limit: a.limit as number } : {}),
        ...(a.cursor != null ? { cursor: a.cursor as string } : {}),
      },
      ctx?.frame?.frameId,
      ctx?.docPath,
    );
  },
  openDiff: (args, ctx) =>
    openDiff(argObject(args, "openDiff") as unknown as OpenDiffParams, {
      frameId: ctx?.frame?.frameId,
      page: ctx?.docPath,
      team: ctx?.frame?.grant.team ?? undefined,
    }),
  whyLine: (args, ctx) => {
    const a = argObject(args, "whyLine");
    const known = ["repo", "rev", "path", "start", "end"];
    if (Object.keys(a).some((key) => !known.includes(key)))
      throw new Error("whyLine: unknown field");
    for (const key of ["repo", "path"] as const) {
      if (typeof a[key] !== "string" || !a[key].length || a[key].includes("\0"))
        throw new Error(`whyLine: invalid ${key}`);
    }
    if (
      (a.path as string).startsWith("-") ||
      (a.rev !== undefined &&
        (typeof a.rev !== "string" ||
          !a.rev.length ||
          a.rev.startsWith("-") ||
          a.rev.includes("\0")))
    )
      throw new Error("whyLine: invalid path or rev");
    for (const key of ["start", "end"] as const) {
      if (
        typeof a[key] !== "number" ||
        !Number.isInteger(a[key]) ||
        a[key] < 1 ||
        a[key] > 0xffffffff
      )
        throw new Error(`whyLine: invalid ${key}`);
    }
    if ((a.end as number) < (a.start as number) || (a.end as number) - (a.start as number) > 2000)
      throw new Error("whyLine: invalid span");
    return whyLine(a as unknown as WhyLineParams, ctx?.frame?.frameId, ctx?.docPath);
  },
  sessionRead: (args, ctx) => {
    const a = { ...argObject(args, "sessionRead") };
    // Rust Option fields and the SDK treat explicit null as omission.
    for (const field of ["cursor", "limit", "turn", "toolUseId"]) {
      if (a[field] === null) delete a[field];
    }
    const known = ["sessionId", "include", "cursor", "limit", "turn", "toolUseId"];
    if (Object.keys(a).some((key) => !known.includes(key)))
      throw new Error("sessionRead: unknown field");
    const sessionId = optionalNonEmptyString(
      a.sessionId,
      "sessionRead: sessionId must not be empty",
    );
    if (!sessionId) throw new Error("sessionRead: sessionId is required");
    if (
      a.include !== undefined &&
      (!Array.isArray(a.include) ||
        a.include.some((value) => value !== "conversation" && value !== "turns"))
    ) {
      throw new Error('sessionRead: include must be ["conversation"] or ["turns"]');
    }
    if (
      Array.isArray(a.include) &&
      a.include.includes("conversation") &&
      a.include.includes("turns")
    ) {
      throw new Error("sessionRead: include conversation or turns, not both");
    }
    const selectors = [a.cursor, a.turn, a.toolUseId].filter((value) => value !== undefined).length;
    if (selectors > 1) throw new Error("sessionRead: choose one of cursor, turn or toolUseId");
    const paged = (a.include as unknown[] | undefined)?.some(
      (value) => value === "conversation" || value === "turns",
    );
    if ((selectors || a.limit !== undefined) && !paged) {
      throw new Error("sessionRead: selectors require a conversation or turns include");
    }
    for (const field of ["cursor", "turn", "limit"]) {
      const value = a[field];
      if (
        value !== undefined &&
        (typeof value !== "number" ||
          !Number.isInteger(value) ||
          value < (field === "limit" ? 1 : 0) ||
          value > (field === "limit" ? 200 : 4294967295))
      ) {
        throw new Error(`sessionRead: invalid ${field}`);
      }
    }
    if (a.toolUseId !== undefined && (typeof a.toolUseId !== "string" || !a.toolUseId.trim()))
      throw new Error("sessionRead: toolUseId must not be empty");
    const params = { ...a, sessionId } as SessionReadParams;
    return sessionRead(params, ctx?.frame?.frameId, ctx?.docPath);
  },
  sessionUsage: (args, ctx) => {
    const a = argObject(args, "sessionUsage");
    // An unknown key is refused, not ignored: a misspelt filter would otherwise read everything.
    const known = ["sinceMs", "untilMs", "sessionId", "cwd", "story", "limit", "cursor"];
    const unknown = Object.keys(a).find((key) => !known.includes(key));
    if (unknown !== undefined) {
      throw new Error(`invalid: sessionUsage has no field ${unknown}`);
    }
    const sinceMs = optionalEpochMs(a.sinceMs, "sessionUsage", "sinceMs");
    const untilMs = optionalEpochMs(a.untilMs, "sessionUsage", "untilMs");
    if (sinceMs !== undefined && untilMs !== undefined && sinceMs >= untilMs) {
      throw new Error("sessionUsage: sinceMs must be before untilMs");
    }
    const filter = {
      sinceMs,
      untilMs,
      sessionId: optionalNonEmptyString(
        a.sessionId ?? undefined,
        "sessionUsage: sessionId, if present, must be a session id from sessions()",
      ),
      cwd: optionalNonEmptyString(
        a.cwd ?? undefined,
        "sessionUsage: cwd, if present, must be a workspace path",
      ),
      story: optionalNonEmptyString(
        a.story ?? undefined,
        "sessionUsage: story, if present, must be a story id",
      ),
      limit: sessionUsageLimit(a.limit),
      cursor: optionalNonEmptyString(
        a.cursor ?? undefined,
        "sessionUsage: cursor, if present, must be the nextCursor of a previous page",
      ),
    };
    return ctx
      ? sessionReadUsage(filter, ctx.frame?.frameId, ctx.docPath)
      : sessionReadUsage(filter);
  },
  // [model_rates] every stored rate, the seeded list prices and the ones setModelRate
  // set, by model prefix and then by day: [{modelPrefix, effectiveFrom, inputPerMtok, outputPerMtok,
  // cacheReadPerMtok, long, source}].
  modelRates: () => sessionReadModelRates(),
  // [list_machines] the machines the page's team sees.
  machines: async (_args, ctx) => {
    const { org, team } = await inboxTeam(ctx, "machines");
    return listMachines(org, team, true);
  },
  // Verified read-only, zero-arg dependency/health status — proves the seam
  // end-to-end for a live dashboard without touching anything sensitive.
  getDependencyStatus: () => getDependencyStatus(),

  // ── Inbox watch verbs — subscription acknowledgements ─────────────────────
  // The HOST already pushes `codesense:inbox-changed` and
  // `codesense:agent-activity-changed` into every live doc frame (see
  // `html-viewer.tsx`); a doc opts in by listening. The watch verbs are a
  // handshake so a doc can discover whether its host emits the event.
  watchInbox: async () => ({ subscribed: "codesense:inbox-changed" }),
  watchAgentInbox: async (args) => {
    const { agentId } = (args ?? {}) as { agentId?: unknown };
    if (agentId !== undefined && (typeof agentId !== "string" || !agentId.trim())) {
      throw new Error("watchAgentInbox: {agentId} must be a non-empty string");
    }
    return { subscribed: "codesense:inbox-changed", agentId: agentId ?? null };
  },
  watchAgentActivity: async () => ({ subscribed: "codesense:agent-activity-changed" }),

  // ── The human's writes from a dashboard ───────────────────────────────────
  // The same inbox operations the Inbox pane uses, as the human. Send class:
  // `sendAsHuman`, `replyAsHuman`; metadata class: `markRead`, `react`, `setHumanStatus`.
  sendAsHuman: async (args, ctx) => {
    const a = argObject(args, "sendAsHuman");
    if (typeof a.to !== "string" || !a.to.trim()) {
      throw new Error("sendAsHuman: {to} must be a session handle or an agent role");
    }
    if (typeof a.text !== "string" || !a.text.trim()) {
      throw new Error("sendAsHuman: {text} must be a non-empty string");
    }
    const attachFile = optionalNonEmptyString(
      a.attachFile,
      "sendAsHuman: attachFile, if present, must be a non-empty string",
    );
    const replyTo = optionalNonEmptyString(
      a.replyTo,
      "sendAsHuman: replyTo, if present, must be a session handle, 'human' or 'role:<agent id>'",
    );
    const to = a.to.trim();
    const text = a.text;
    const inReplyTo = a.inReplyTo == null ? null : messageId(a.inReplyTo, "sendAsHuman");
    return inboxWrite(ctx, "sendAsHuman", a, (team) =>
      sendInboxAsHuman(
        team,
        to,
        text,
        validateContentType(a.contentType),
        validateOnClick(a.onClick),
        attachFile ?? null,
        replyTo ?? null,
        validateSummary(a.summary, "sendAsHuman"),
        inReplyTo,
      ),
    );
  },
  loadMedia: async (args, ctx) => {
    const a = argObject(args, "loadMedia");
    const id = messageId(a.messageId, "loadMedia");
    return inboxWrite(ctx, "loadMedia", a, (team) => loadInboxMedia(team, id));
  },
  replyAsHuman: async (args, ctx) => {
    const a = argObject(args, "replyAsHuman");
    const id = messageId(a.messageId, "replyAsHuman");
    if (typeof a.text !== "string" || !a.text.trim()) {
      throw new Error("replyAsHuman: {text} must be a non-empty string");
    }
    const text = a.text;
    return inboxWrite(ctx, "replyAsHuman", a, (team) =>
      replyAsHuman(
        team,
        id,
        text,
        validateContentType(a.contentType),
        validateSummary(a.summary, "replyAsHuman"),
      ),
    );
  },
  // A reaction is state on the message, not mail: one per (message, reactor, emoji).
  react: async (args, ctx) => {
    const a = argObject(args, "react");
    const id = messageId(a.messageId, "react");
    if (typeof a.emoji !== "string" || !a.emoji.trim()) {
      throw new Error("react: {emoji} must be one emoji");
    }
    if (a.remove != null && typeof a.remove !== "boolean") {
      throw new Error("react: remove, if present, must be true or false");
    }
    const emoji = a.emoji;
    const remove = typeof a.remove === "boolean" ? a.remove : null;
    return inboxWrite(ctx, "react", a, (team) => reactAsHuman(team, id, emoji, remove));
  },
  dismiss_agent_mark: async (args, ctx) => {
    const a = argObject(args, "dismiss_agent_mark");
    if (typeof a.sessionId !== "string" || typeof a.crashId !== "string")
      throw new Error("dismiss_agent_mark: sessionId and crashId are required");
    if (!ctx?.frame) throw new Error("denied: frame identity");
    return dismissAgentMark(
      a.sessionId,
      a.crashId,
      typeof a.requestId === "string" ? a.requestId : undefined,
      { origin: ctx.frame.origin, token: ctx.frame.token },
    );
  },
  markRead: async (args, ctx) => {
    const a = argObject(args, "markRead");
    const ids = a.messageIds;
    if (!Array.isArray(ids) || ids.length === 0) {
      throw new Error("markRead: {messageIds} must be a non-empty array of message ids");
    }
    const valid = ids.map((id) => messageId(id, "markRead"));
    return inboxWrite(ctx, "markRead", a, async (team) => ({
      marked: await markInboxMessagesRead(team, valid),
    }));
  },
  markUnread: async (args, ctx) => {
    const a = argObject(args, "markUnread");
    if (!Array.isArray(a.messageIds) || !a.messageIds.length)
      throw new Error("markUnread: {messageIds} must be a non-empty array of message ids");
    const ids = a.messageIds.map((id) => messageId(id, "markUnread"));
    return inboxWrite(ctx, "markUnread", a, async (team) => ({
      marked: await markInboxMessagesUnread(team, ids),
    }));
  },
  archive: async (args, ctx) => {
    const a = argObject(args, "archive");
    const ids = a.messageIds;
    const error = "archive: {messageIds} must be a non-empty array of message ids";
    if (!Array.isArray(ids) || ids.length === 0) throw new Error(error);
    let valid: number[];
    try {
      valid = ids.map((id) => messageId(id, "archive"));
    } catch {
      throw new Error(error);
    }
    return inboxWrite(ctx, "archive", a, async (team) => ({
      archived: await markInboxMessagesArchived(team, valid),
    }));
  },
  setHumanStatus: async (args, ctx) => {
    const a = argObject(args, "setHumanStatus");
    if (!HUMAN_STATUSES.includes(a.status as HumanStatusValue)) {
      throw new Error("setHumanStatus: {status} must be 'online', 'away', 'busy' or 'offline'");
    }
    const note = optionalNonEmptyString(
      a.note,
      "setHumanStatus: note, if present, must be a non-empty string",
    );
    return setHumanStatus(
      await inboxTeam(ctx, "setHumanStatus"),
      a.status as HumanStatusValue,
      note,
    );
  },

  // ── Inbox primitives — write verbs ────────────────────────────────────────
  // `postMessage({to, text, onClick?})` → `{status, messageId?, deliveryId?}`.
  // `to` is `"human"` (the user's inbox — not yet exposed from a pane; routed
  // through `send_role_message` with role=`human-ish` isn't the shipped path),
  // OR an agent id → routes through `sendRoleMessage` to the agent's role
  // inbox. `onClick` is the per-message action the Inbox UI honors on click
  // (goto-agent / open-file / show-message). Rate-guarded in the send class.
  postMessage: async (args, ctx) => {
    const a = (args ?? {}) as {
      to?: unknown;
      text?: unknown;
      onClick?: unknown;
      contentType?: unknown;
      summary?: unknown;
    };
    if (typeof a.to !== "string" || !a.to.trim()) {
      throw new Error("postMessage: {to} must be a non-empty string");
    }
    if (typeof a.text !== "string" || !a.text.trim()) {
      throw new Error("postMessage: {text} must be a non-empty string");
    }
    const onClick = validateOnClick(a.onClick);
    const contentType = validateContentType(a.contentType);
    const team = await resolveInboxTeam(ctx);
    if (!team) throw new Error("postMessage: no active team");
    const sender = ctx?.docPath ?? "human";
    const result = await sendRoleMessage(
      team,
      sender,
      a.to.trim(),
      a.text,
      // Doc-driven posts are idempotency-keyed per send so a dashboard double-
      // click can't duplicate — the FE-minted `requestId` is unique per call.
      `pane-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`,
      null,
      onClick,
      contentType,
      validateSummary(a.summary, "postMessage"),
    );
    return result;
  },

  // `gotoAgent({agentId})` — equivalent to clicking the agent in the Agents
  // panel: the one Show (its tab, restored or adopted into one, its run output,
  // or a stated reason). Resolves `{located, outcome}`.
  gotoAgent: async (args) => {
    const { agentId, preserveFocus } = (args ?? {}) as {
      agentId?: unknown;
      preserveFocus?: boolean;
    };
    if (typeof agentId !== "string" || !agentId.trim()) {
      throw new Error("gotoAgent: {agentId} must be a non-empty string");
    }
    return locateAgentByAgentId(agentId.trim(), { preserveFocus: preserveFocus === true });
  },

  moveWorkspace: async (args) => {
    const { teamKey, workspaceId, before, after } = (args ?? {}) as {
      teamKey: string;
      workspaceId: string;
      before?: string;
      after?: string;
    };
    if (
      typeof teamKey !== "string" ||
      !teamKey ||
      typeof workspaceId !== "string" ||
      !workspaceId ||
      (typeof before === "string") === (typeof after === "string")
    ) {
      throw new Error(
        "moveWorkspace: requires teamKey, workspaceId and exactly one of before/after",
      );
    }
    const { useViewerStore } = await import("@/shared/stores/viewer-store");
    return {
      moved: useViewerStore
        .getState()
        .reorderWorkspace(
          teamKey,
          workspaceId,
          before !== undefined ? { before } : { after: after! },
        ),
    };
  },

  // Architecture: these only present the existing native forms after registry admission.
  // Policy (open): the person confirms in those forms; no additional prompt.
  openLauncher: async (args, ctx) => {
    const a = argObject(args, "openLauncher");
    if (a.org !== undefined && (typeof a.org !== "string" || typeof a.team !== "string")) {
      throw new Error("openLauncher: org requires a team");
    }
    const team =
      typeof a.team === "string"
        ? {
            org: typeof a.org === "string" ? a.org : useOrgStore.getState().activeOrgSlug,
            team: a.team,
          }
        : await resolveInboxTeam(ctx);
    if (!team?.org) throw new Error("unavailable: no team selected");
    if (!pageAdmittedTeams(ctx).some((t) => t.org === team.org && t.team === team.team)) {
      throw new Error(`openLauncher: unknown or unadmitted team ${team.org}:${team.team}`);
    }
    openLaunchSheet({
      teamKey: buildTeamKey(team.org, team.team),
      task: String(a.task),
    });
    return { opened: true };
  },
  openRoutineDraft: async (args) => {
    const a = argObject(args, "openRoutineDraft");
    openRoutineDraft({
      draft: {
        name: String(a.name),
        task: String(a.task),
        ...(typeof a.when === "string" ? { when: a.when } : {}),
        ...(typeof a.agent === "string" ? { agent: a.agent } : {}),
      },
      audience: "personal",
    });
    return { opened: true };
  },

  launchUnder: async (args) => {
    const { handle, task } = (args ?? {}) as { handle?: string; task?: string };
    const row = (await readAgentListSessions()).find((row) => row.handle === handle && row.alive);
    if (!row) throw new Error("unavailable: no such agent");
    const team = await readAgentListTeam(row);
    if (!team) throw new Error("unavailable: agent team is unavailable");
    launchUnderAgent(row, team, task);
    return { opened: true };
  },

  openAgentInbox: async (args) => {
    const { handle } = (args ?? {}) as { handle?: string };
    const row = (await readAgentListSessions()).find((row) => row.handle === handle);
    if (!row || !(await openAgentInboxForRow(row)))
      throw new Error("unavailable: agent is not running");
    return { opened: true };
  },

  // SW-SD-2 (reviewed exception to "read-first"): runs the user's OWN configured
  // agent CLI (no codesense credential) over {prompt, data} and returns the
  // analysis text. One-shot, no tools (`--max-turns 1` → cannot take actions), so
  // the security property is structural, not a permissions inheritance. Backend
  // is single-flight + blocking-pool; a rejection carries a typed prefix
  // (`busy`/`timeout:`/`unavailable:`) for the doc to render. `data` may be a JSON string or an object — we
  // coerce to the string the Rust command expects.
  analyze: (args) => {
    const { prompt, data } = args as { prompt: string; data: unknown };
    return analyze({
      prompt: String(prompt ?? ""),
      data: typeof data === "string" ? data : JSON.stringify(data ?? null),
    });
  },

  // ── SW-DX-6 action verbs (MUTATING) ──────────────────────────────────────
  // Each is a typed wrapper over a real, managed-target-confined command — never
  // raw `invoke`. This map only maps name → wrapper.

  // setStatus: flip a scanned doc's `status` frontmatter via the SAME
  // `applyMetadataEdit` seam the metadata band uses (`setField` variant — NOT a
  // re-implementation; format-awareness md/HTML lives in the Rust command).
  // Accepts a single path OR a batch (`path: string[]`) — the batch is one
  // call returning a per-item result array (best-effort;
  // frontmatter edits across files can't be transactional). Managed-target
  // confinement is enforced by the Rust command (read-only/installed → reject),
  // surfaced verbatim.
  setStatus: async (args, ctx) => {
    const { path, status } = (args ?? {}) as { path: unknown; status: unknown };
    if (typeof status !== "string") throw new Error("setStatus: status must be a string");
    const paths = Array.isArray(path) ? path : [path];
    if (paths.length === 0 || !paths.every((p) => typeof p === "string" && p.length > 0)) {
      throw new Error("setStatus: path must be a non-empty string or string[]");
    }
    const edits: MetadataEdit[] = [{ kind: "setField", field: "status", value: status }];
    // Single path: let the Rust rejection (e.g. managed-target) propagate verbatim.
    if (!Array.isArray(path)) {
      await applyMetadataEdit(paths[0] as string, edits, ctx?.frame?.frameId, ctx?.docPath);
      return { ok: true };
    }
    // Batch: best-effort per item so one unmanaged path doesn't fail the whole set.
    const results: Array<{ path: string; ok: boolean; error?: string }> = [];
    for (const p of paths as string[]) {
      try {
        await applyMetadataEdit(p, edits, ctx?.frame?.frameId, ctx?.docPath);
        results.push({ path: p, ok: true });
      } catch (e) {
        results.push({ path: p, ok: false, error: e instanceof Error ? e.message : String(e) });
      }
    }
    return results;
  },

  // setField: write top-level frontmatter keys of a content file (`{path, field,
  // value}` or `{path, fields}`); `null` removes a key. Rust confines it to a team
  // or private content root, validates names and values, writes atomically and
  // byte-preserving, and tells the index (so `subscribeContext` pages re-query).
  setField: async (args, ctx) => {
    const a = (args ?? {}) as {
      path?: unknown;
      field?: unknown;
      value?: unknown;
      fields?: unknown;
    };
    if (typeof a.path !== "string" || !a.path) {
      throw new Error("setField: {path} must be a non-empty string");
    }
    let fields: Record<string, FrontmatterValue>;
    if (a.fields !== undefined) {
      if (!a.fields || typeof a.fields !== "object" || Array.isArray(a.fields)) {
        throw new Error("setField: {fields} must be an object of field → value");
      }
      if (a.field !== undefined)
        throw new Error("setField: pass {field, value} or {fields}, not both");
      fields = a.fields as Record<string, FrontmatterValue>;
    } else {
      if (typeof a.field !== "string" || !a.field) {
        throw new Error("setField: {field} must be a non-empty string");
      }
      if (!("value" in a)) throw new Error("setField: {value} is required (null removes the key)");
      fields = { [a.field]: (a.value ?? null) as FrontmatterValue };
    }
    await updateFrontmatter(a.path, fields, ctx?.frame?.frameId, ctx?.docPath);
    return { ok: true };
  },

  // routeToContext: maps onto the shipped `route_file_to_context` command.
  // The command returns a `RouteOutcomeDto` and, for a non-managed/unroutable
  // source, RESOLVES with a non-affirmative outcome (a deliberate sync-engine
  // no-op, never an Err — SLW-FX-13). For a dashboard *action* that would be an
  // ambiguous silent success, so we translate a non-affirmative outcome into a
  // typed `denied:` REJECTION (deny-by-default: the doc never mistakes a
  // no-route for a success). Affirmative outcomes (routed / already-routed /
  // already-tracked) resolve with the DTO.
  routeToContext: async (args, ctx) => {
    const { codeRepoPath, codeRepoSlug, logicalName, layer } = (args ?? {}) as {
      codeRepoPath: unknown;
      codeRepoSlug: unknown;
      logicalName: unknown;
      layer?: unknown;
    };
    if (
      typeof codeRepoPath !== "string" ||
      typeof codeRepoSlug !== "string" ||
      typeof logicalName !== "string"
    ) {
      throw new Error("routeToContext: {codeRepoPath, codeRepoSlug, logicalName} must be strings");
    }
    const outcome = await routeFileToContext(
      codeRepoPath,
      codeRepoSlug,
      logicalName,
      typeof layer === "string" ? layer : undefined,
      ctx?.frame?.frameId,
      ctx?.docPath,
    );
    const kind = (outcome as { outcome?: string } | null)?.outcome;
    // Deny-by-default: only the affirmative outcomes resolve. A non-affirmative
    // outcome (refused/notProjectable/collision) OR a
    // missing/malformed outcome rejects — the doc never mistakes a no-route for
    // a success.
    if (!kind || !ROUTE_OK_OUTCOMES.has(kind)) {
      throw new Error(`denied: route not applied (${kind ?? "unknown"})`);
    }
    return outcome;
  },

  // draft: create + seed a NEW managed content doc derived from `from` at tier
  // `level`. Composed from the context-gated `create_file` command (the derived
  // path lives in `from`'s directory, so `validate_context_path` is the
  // managed-target wall — an unmanaged `from`/derived path is rejected by Rust,
  // surfaced verbatim).
  draft: async (args, ctx) => {
    const { level, from } = (args ?? {}) as { level: unknown; from: unknown };
    if (typeof level !== "string" || !level || typeof from !== "string" || !from) {
      throw new Error("draft: {level, from} must be non-empty strings");
    }
    const slash = from.lastIndexOf("/");
    const dir = slash >= 0 ? from.slice(0, slash) : "";
    const fromName = slash >= 0 ? from.slice(slash + 1) : from;
    // Nonce so two drafts in the same dir can't collide (create_file refuses to
    // overwrite an existing file). Mirrors the snapshot nonce in html-viewer.
    const nonce = Math.floor(Math.random() * 0x100000000).toString(36);
    const path = `${dir ? `${dir}/` : ""}${level}-draft-${nonce}.md`;
    const seed =
      `---\ntype: ${level}\nstatus: todo\nsupporting_docs:\n  - ${fromName}\n---\n\n` +
      `# ${level} draft\n\nDrafted from [${fromName}](${fromName}).\n`;
    await createFile(path, seed, ctx?.frame?.frameId, ctx?.docPath);
    return { path };
  },

  // ── SW-DX-7 spawn-class verbs — the execution loop ───────────────────────
  // Thin forwarders to the dashboard-launch CONTROLLER, which the `HtmlViewer`
  // render scope registers (it owns the teamKey + pipeline tree needed to resolve
  // a storyId to a managed path). The Rust SpawnCap and the storyId↔sessionId
  // binding live behind the controller, never here. When no dashboard is
  // mounted, the verbs reject rather than guess.
  // SW-DX-15: the generic open primitive. Back-compat: the shipped `{ storyId }`
  // wire form normalizes to a `story` target; the new form is `{ target, headed? }`
  // (target: story │ epic). The controller dispatches by `target.kind`.
  openWorkspace: async (args, ctx) => {
    // Validate/normalize the arg BEFORE the controller check (mirrors the other
    // spawn verbs, so a shape error surfaces even with no dashboard mounted).
    const a = (args ?? {}) as {
      storyId?: unknown;
      target?: unknown;
      headed?: unknown;
      initialPrompt?: unknown;
      sessionMode?: unknown;
      reviewPrompt?: unknown;
      operationId?: unknown;
    };
    const headed = a.headed === false ? false : true;
    // SW-GW-9 (AC-8): normalize the optional seed at the untrusted wire boundary via
    // the shared rule (empty/whitespace ≡ absent — byte-identical spawn-only).
    const initialPrompt = normalizeInitialPrompt(a.initialPrompt);
    // SW-DX-17e: validate the optional session-mode enum (absent → default focus).
    const sessionMode = validateSessionMode(a.sessionMode);
    let openArg: OpenWorkspaceArg;
    if (a.target === undefined) {
      if (a.reviewPrompt !== undefined) {
        throw new Error("openWorkspace: reviewPrompt is only supported for branch targets");
      }
      // Legacy `{ storyId }` → story target (always story → initialPrompt + sessionMode
      // allowed — it is the story kind).
      if (typeof a.storyId !== "string" || !a.storyId) {
        throw new Error("openWorkspace: { storyId } must be a non-empty string");
      }
      // SW-GW-11 (DRY): the legacy form must reject a `#`-bearing id too — `#` is the
      // reserved `storyId#label` handle separator, so an unguarded legacy id could
      // collide with a named/fork handle (and a legacy `sessionMode:'new'` would mint a
      // double-`#` handle). Closes a pre-existing asymmetry with the `{target}` path
      // (review r1).
      assertStoryIdNoHash(a.storyId, "openWorkspace");
      openArg = { target: { kind: "story", storyId: a.storyId }, initialPrompt, sessionMode };
    } else {
      const target = a.target as {
        kind?: unknown;
        storyId?: unknown;
        team?: unknown;
        epic?: unknown;
        repo?: unknown;
        baseRepo?: unknown;
        pullNumber?: unknown;
        branch?: unknown;
        path?: unknown;
        // SW-GW-6: story-target extensions (validated in the `story` branch).
        session?: unknown;
        title?: unknown;
        agent?: unknown;
        linkedStoryId?: unknown;
        machineId?: unknown;
      };
      if (!target || typeof target.kind !== "string") {
        throw new Error("openWorkspace: { target: { kind } } required");
      }
      if (a.reviewPrompt !== undefined && target.kind !== "branch") {
        throw new Error("openWorkspace: reviewPrompt is only supported for branch targets");
      }
      // SW-GW-9 (AC-2): `initialPrompt` is STORY-ONLY. A non-story target has no agent
      // to seed at call time — `dir` is a bare CLI shell, `epic` returns no sessionId
      // here (reached later via getEpicHandle), and `branch` has no consumer yet — so
      // seeding them would paste author text at a `$` prompt or silently vanish. ONE
      // uniform guard rejects all non-story kinds (honest boundary, never a silent
      // drop), mirroring the `headed:false` story-only refusals below.
      if (initialPrompt && target.kind !== "story") {
        throw new Error(
          `unavailable: initialPrompt is only supported for story targets, not ${target.kind}`,
        );
      }
      // SW-DX-17e (OQ-5): `sessionMode:'new'` (fork a fresh session) is STORY-ONLY —
      // epic/branch/dir launchers key ONE role per handle, with no distinct-role fork
      // path, so a `new` there would silently focus instead of fork. Reject loudly
      // (mirrors the initialPrompt story-only guard). `focus`/absent is the universal
      // default and passes through untouched for every kind.
      if (sessionMode === "new" && target.kind !== "story") {
        throw new Error(
          `unavailable: sessionMode:'new' is only supported for story targets, not ${target.kind}`,
        );
      }
      // An open names the agent it launches; a launch profile or pack named instead is
      // refused, never silently dropped.
      if ("profile" in target || "packId" in target) {
        throw new Error(
          "invalid: openWorkspace launches an agent; pass { agent }, not profile or packId",
        );
      }
      // `agent` (a key, or an id in the workspace's team then the personal space) applies to
      // every kind that launches one: `story` and `branch` always, `dir` always (an agent
      // session at the dir), and `epic` only headless (a headed epic open is a bare terminal).
      const openAgent = optionalNonEmptyString(
        target.agent,
        "openWorkspace: agent, if present, must be a non-empty string",
      );
      if (openAgent !== undefined && target.kind === "epic" && headed !== false) {
        throw new Error(
          "unavailable: agent on an epic open requires headed:false (the headed epic open launches no agent)",
        );
      }
      // `machineId` (a team machine's id, or "local" for This Mac) says where a story's
      // workspace is created when it has none yet. Story-only.
      const machineId = optionalNonEmptyString(
        target.machineId,
        "openWorkspace: machineId, if present, must be a non-empty string",
      );
      if (machineId !== undefined && target.kind !== "story") {
        throw new Error(
          `unavailable: machineId is only supported for story targets, not ${target.kind}`,
        );
      }
      if (target.kind === "story") {
        if (target.storyId === undefined && typeof target.path === "string" && target.path.trim()) {
          if (
            a.initialPrompt !== undefined ||
            sessionMode === "new" ||
            target.agent !== undefined ||
            target.session !== undefined ||
            target.title !== undefined ||
            target.machineId !== undefined
          ) {
            throw new Error(
              "openWorkspace: a story path opens the native workspace and does not accept launch options",
            );
          }
          openArg = { target: { kind: "story", storyPath: target.path.trim() }, headed };
        } else if (typeof target.storyId !== "string" || !target.storyId) {
          throw new Error("openWorkspace: story target needs a non-empty { storyId }");
        } else {
          // SW-GW-11 (DRY): reject a `#`-bearing storyId via the shared guard.
          assertStoryIdNoHash(target.storyId, "openWorkspace: story");
          // SW-GW-6: optional named session + title, and the agent to launch.
          const storyTarget: {
            kind: "story";
            storyId: string;
            session?: string;
            title?: string;
            agent?: string;
            machineId?: string;
          } = { kind: "story", storyId: target.storyId };
          // SW-GW-11 (DRY): validate the optional named-session label via the shared
          // helper (blank → main session; ill-formed → loud) — the SAME rule `runPrompt`
          // uses, so the two verbs can't drift.
          const storySession = validateStorySessionLabel(target.session, "openWorkspace: story");
          if (storySession !== undefined) storyTarget.session = storySession;
          // `title` (tab display name) — if present, it
          // must be a non-empty string (shared, trimming optional-string validator).
          // Only assign when defined so the main-session target stays a clean
          // `{ kind, storyId }` with no explicit `undefined` keys.
          const title = optionalNonEmptyString(
            target.title,
            "openWorkspace: story title, if present, must be a non-empty string",
          );
          if (title !== undefined) storyTarget.title = title;
          if (openAgent !== undefined) storyTarget.agent = openAgent;
          if (machineId !== undefined) storyTarget.machineId = machineId;
          // SW-GW-9: thread the normalized seed onto the story openArg (story-only,
          // guarded above). Empty/whitespace already normalized to undefined.
          // SW-DX-17e: thread the validated session-mode too (story-only).
          openArg = { target: storyTarget, headed, initialPrompt, sessionMode };
        }
      } else if (target.kind === "epic") {
        // SW-DX-16: an EXPLICIT epic carries both `{team, epic}` (open ANY managed
        // epic); neither → the board's OWN epic (DX-15, resolved from docPath).
        // Both-or-neither: a half-specified target is a shape error, never a
        // silent own-epic fallback (AC-5).
        const hasTeam = target.team !== undefined;
        const hasEpic = target.epic !== undefined;
        if (hasTeam !== hasEpic) {
          throw new Error("openWorkspace: epic target needs both { team, epic } or neither");
        }
        // The agent (validated above; headed-epic reject also above) is what the HEADLESS
        // epic open launches.
        if (hasTeam && hasEpic) {
          if (
            typeof target.team !== "string" ||
            !target.team ||
            typeof target.epic !== "string" ||
            !target.epic
          ) {
            throw new Error("openWorkspace: epic target { team, epic } must be non-empty strings");
          }
          const epicTarget: { kind: "epic"; team: string; epic: string; agent?: string } = {
            kind: "epic",
            team: target.team,
            epic: target.epic,
          };
          if (openAgent !== undefined) epicTarget.agent = openAgent;
          openArg = { target: epicTarget, headed };
        } else {
          const epicTarget: { kind: "epic"; agent?: string } = { kind: "epic" };
          if (openAgent !== undefined) epicTarget.agent = openAgent;
          openArg = { target: epicTarget, headed };
        }
      } else if (target.kind === "branch") {
        if (typeof target.repo !== "string" || !target.repo) {
          throw new Error("openWorkspace: branch target needs a non-empty { repo }");
        }
        if (typeof target.branch !== "string" || !target.branch) {
          throw new Error("openWorkspace: branch target needs a non-empty { branch }");
        }
        const branchTarget: {
          kind: "branch";
          repo: string;
          baseRepo?: string;
          pullNumber?: number;
          branch: string;
          agent?: string;
          linkedStoryId?: string;
        } = {
          kind: "branch",
          repo: target.repo,
          branch: target.branch,
        };
        const baseRepo = optionalNonEmptyString(
          target.baseRepo,
          "openWorkspace: baseRepo, if present, must be a non-empty string",
        );
        if (baseRepo !== undefined) branchTarget.baseRepo = baseRepo;
        if (target.pullNumber !== undefined) {
          const pullNumber = Number(target.pullNumber);
          if (!Number.isSafeInteger(pullNumber) || pullNumber <= 0) {
            throw new Error("openWorkspace: pullNumber must be a positive integer");
          }
          branchTarget.pullNumber = pullNumber;
        }
        if (openAgent !== undefined) branchTarget.agent = openAgent;
        const linkedStoryId = optionalNonEmptyString(
          target.linkedStoryId,
          "openWorkspace: linkedStoryId, if present, must be a non-empty string",
        );
        if (linkedStoryId !== undefined) branchTarget.linkedStoryId = linkedStoryId;
        const reviewPrompt = optionalNonEmptyString(
          a.reviewPrompt,
          "openWorkspace: reviewPrompt, if present, must be a non-empty string",
        );
        if (reviewPrompt !== undefined && headed !== false) {
          throw new Error("openWorkspace: a branch review requires headed:false");
        }
        const operationId = optionalNonEmptyString(
          a.operationId,
          "openWorkspace: operationId, if present, must be a non-empty string",
        );
        openArg = { target: branchTarget, headed, reviewPrompt, operationId };
      } else if (target.kind === "dir") {
        // SW-GW-2: the `dir` target opens a generic CLI workspace (SW-GW-1) at an
        // arbitrary path; with an `agent` (or headless) it opens an agent session there.
        if (typeof target.path !== "string" || !target.path) {
          throw new Error("openWorkspace: dir target needs a non-empty { path }");
        }
        const dirTarget: { kind: "dir"; path: string; agent?: string } = {
          kind: "dir",
          path: target.path,
        };
        if (openAgent !== undefined) dirTarget.agent = openAgent;
        openArg = { target: dirTarget, headed };
      } else {
        throw new Error(`openWorkspace: unsupported target kind '${target.kind}'`);
      }
    }
    const controller = launchController(ctx);
    if (!controller) throw new Error("unavailable: no active dashboard workspace context");
    return controller.openWorkspace({ ...openArg, frameId: ctx?.frame?.frameId });
  },
  // openFile: open a file in the viewer. Rust resolves and validates the target —
  // an absolute path inside a known team content root, a path relative to the
  // page's own root, or `{team, path}` in the page's org — so a page can never
  // open an arbitrary filesystem path. A file in another team switches to it. A
  // page's open is not the user's, so it keeps full-screen CLI on.
  openFile: async (args, ctx) => {
    const a = (args ?? {}) as {
      path?: unknown;
      org?: unknown;
      team?: unknown;
      newTab?: unknown;
      preview?: unknown;
    };
    if (typeof a.path !== "string" || !a.path.trim()) {
      throw new Error("openFile: {path} must be a non-empty string");
    }
    const team = optionalNonEmptyString(
      a.team,
      "openFile: team, if present, must be a non-empty string",
    );
    let page = ctx?.docPath ?? "";
    if (a.org !== undefined) {
      const scoped = await inboxTeam(ctx, "openFile", a as Record<string, unknown>);
      const org = useOrgStore
        .getState()
        .orgs.find((o) => o.slug === scoped.org && o.teams.some((t) => t.name === scoped.team));
      if (!org) throw new Error("openFile: unknown synced team");
      page = pathToTeamContentDirSync(org.org_root, scoped.team) + "/inbox.html";
    }
    const frame = ctx?.frame;
    const resolvePath = async (path: string) => {
      // A verified bundled reader may reopen only its exact own file. Other
      // paths, including traversal spellings, retain the existing admission.
      const self =
        frame?.kind === "builtin" &&
        frame.grant.dashboard.startsWith("dashboard:built-in:") &&
        frame.grant.team &&
        ctx?.docPath === frame.documentPath &&
        a.org === undefined &&
        team === undefined &&
        (path === ctx.docPath || path === ctx.docPath.split("/").pop());
      if (self) return { path: frame.documentPath, ...frame.grant.team! };
      // Team-bound file-nav panes and built-ins share their listing's admission.
      // Other pages and explicit team targets retain the general opener's rules.
      return frame?.grant.team &&
        (frame.kind === "builtin" || frame.grant.slot === "file-nav") &&
        a.org === undefined &&
        team === undefined
        ? ((await fileSurfaceCall("resolve-open", { path }, frame.frameId)) as {
            path: string;
            org: string;
            team: string;
          })
        : await bridgeResolveOpenTarget(page, path, team, frame?.frameId);
    };
    let target: { path: string; org: string; team: string }, hash: string | undefined;
    // An admitted, existing literal path wins; only otherwise interpret '#'
    // as a fragment, with the path part passing through the same admission.
    try {
      target = await resolvePath(a.path);
    } catch (error) {
      const fragmentAt = a.path.indexOf("#");
      if (fragmentAt < 0) throw error;
      target = await resolvePath(a.path.slice(0, fragmentAt));
      hash = a.path.slice(fragmentAt);
    }
    openFileInTeam(target, {
      newTab: a.preview === false || a.newTab === true,
      navigate: ctx?.navigate,
      switchTeam: (teamKey) => switchContext({ teamKey }, "nav"),
      keepLayout: true,
      ...(hash ? { hash } : {}),
    });
    return { path: target.path, org: target.org, team: target.team };
  },
  // ── SW-GW-2 spawn-tier verb — add a tab to a generic (dir) workspace ────────
  // `openTab({workspaceId, agent?}) → {sessionId}` spawns an extra tab in an
  // already-open GENERIC workspace and returns its addressable sessionId (the
  // DRIVE half of SW-GW-2). Handle-addressed, so it operates on the store
  // directly (not the render-scoped controller) — but still fails closed when no
  // dashboard is mounted, and only ever touches a `generic` workspace.
  openTab: async (args, ctx) => {
    if (!launchController(ctx)) {
      throw new Error("unavailable: no active dashboard workspace context");
    }
    return openWorkspaceTab(
      (args ?? {}) as { workspaceId?: unknown; agent?: unknown },
      makeRealOpenWorkspaceTabDeps(ctx?.frame?.frameId, ctx?.docPath),
    );
  },
  // SW-GW-11: the runPrompt DRIVE verb — a {target, prompt} pair into an
  // already-live session. `target` is a story session handle (bare `storyId` or the
  // SW-GW-6 compound `storyId#label`), split + validated here with the SHARED slug
  // seam (DRY with openWorkspace), then resolved TRUSTED-side by the controller. The
  // prompt is arbitrary, not an enum. No spawn: launching an agent lives
  // on openWorkspace.
  runPrompt: async (args, ctx) => {
    const { target, prompt } = (args ?? {}) as {
      target?: unknown;
      prompt?: unknown;
    };
    // A blank/whitespace-only prompt is rejected (never injects a stray Enter into
    // the PTY) — same "whitespace ≡ absent" rule the session label + initialPrompt use.
    if (typeof prompt !== "string" || !prompt.trim()) {
      throw new Error("runPrompt: {prompt} must be a non-empty string");
    }
    if (typeof target !== "string" || !target) {
      throw new Error("runPrompt: {target} must be a non-empty string");
    }
    // Parse via the SHARED canonical split (same seam the drive path resolves
    // with), then validate the label with the SHARED slug seam: a present `#label`
    // must be a valid slug, so a two-`#` target lands a `#` in the label half and
    // is rejected. `storyId` can't carry a `#` (it's the pre-`#` slice).
    const { storyId, label: rawLabel } = splitStoryHandle(target);
    // A `#`-leading target (`"#label"` / `"#"`) has no story id — reject loudly at
    // the seam (symmetric with openWorkspace) rather than deferring to a confusing
    // downstream `cannot resolve story ""`.
    if (!storyId) {
      throw new Error("runPrompt: {target} must include a story id (before any '#')");
    }
    const label = validateStorySessionLabel(rawLabel, "runPrompt");
    const handle = label ? `${storyId}#${label}` : storyId;
    const controller = launchController(ctx);
    if (!controller) throw new Error("unavailable: no active dashboard workspace context");
    return controller.runPrompt({ target: handle, prompt });
  },

  sendPrompt: async (args): Promise<AgentPromptResult> => {
    const active = args as { handle?: string; text?: string; submit?: boolean };
    if (active.handle === "active" && active.submit === false && active.text?.trim()) {
      sendTextToTerminal(active.text);
      return { status: "typed", submitted: false, machine: null, code: null, message: null };
    }

    const { handle, text, submit } = (args ?? {}) as {
      handle: unknown;
      text: unknown;
      submit?: unknown;
    };
    if (typeof handle !== "string" || !handle.trim()) {
      throw new Error("sendPrompt: {handle} must be a non-empty string");
    }
    if (typeof text !== "string" || !text.trim()) {
      throw new Error("sendPrompt: {text} must be a non-empty string");
    }
    if (submit !== undefined && typeof submit !== "boolean") {
      throw new Error("sendPrompt: {submit} must be a boolean");
    }
    try {
      return await sendPromptToAgent(handle, null, text, submit);
    } catch (error) {
      return {
        status: "invalid",
        submitted: false,
        machine: null,
        code: null,
        message: error instanceof Error ? error.message : String(error),
      };
    }
  },

  // A story-bound chat turn resolves through its launch controller.
  sendToSession: async (args, ctx) => {
    const { target, text } = (args ?? {}) as { target: unknown; text: unknown };
    if (typeof target !== "string" || !target) {
      throw new Error("sendToSession: {target} must be a non-empty string");
    }
    if (typeof text !== "string" || !text) {
      throw new Error("sendToSession: {text} must be a non-empty string");
    }
    const controller = launchController(ctx);
    if (!controller) throw new Error("unavailable: no active dashboard workspace context");
    return controller.sendToSession({ target, text });
  },

  // ── SW-GW-10 + SW-WA-F7 management-tier verb — close a session (and optionally
  // reap its managed worktree). Base path (SW-GW-10): `closeWorkspace({target}) →
  // {closed}` kills the handle-bound session (freeing the concurrency cap +
  // writing one Kill audit line) AND evicts its tab. Forwards to the controller,
  // which resolves `target`→sessionId TRUSTED-SIDE (never a doc-supplied raw
  // session id, AC-2), then routes through the one authoritative
  // `dashboard_kill_by_handle` choke. Audited, no write/send rate guard
  // (MANAGEMENT class). An unbound handle is a typed no-op (idempotent close,
  // AC-3). SW-WA-F7 DESTRUCTIVE opt-in: `removeWorktree: true` escalates to the
  // symmetric close of `openWorkspace({storyId})` — tabs close as above, then the
  // managed worktree is reaped through the Tauri `remove_worktree` command.
  // `discardUnsaved: true` on top forces the removal over uncommitted work (the
  // SW-XS-4 `discard_code` + `discard_context` consents collapse to one escape
  // hatch at this seam, same as the Rust `close_workspace` MCP tool).
  closeWorkspace: async (args, ctx) => {
    const { target, removeWorktree, discardUnsaved } = (args ?? {}) as {
      target: unknown;
      removeWorktree?: unknown;
      discardUnsaved?: unknown;
    };
    if (typeof target !== "string" || !target) {
      throw new Error("closeWorkspace: {target} must be a non-empty string");
    }
    const controller = launchController(ctx);
    if (!controller) throw new Error("unavailable: no active dashboard workspace context");
    const base = await controller.closeWorkspace(target);
    // Base behavior stays identical when the destructive opt-in is absent.
    if (removeWorktree !== true) return base;
    // SW-WA-F7: reap the managed worktree. Resolve the open workspace by
    // `target` (a storyId or workspaceId) on the viewer store, then look up its
    // parent repo in the managed-worktrees registry (`main_repo_of_worktree`
    // on the Rust side). A workspace with no managed worktree (team workspace,
    // generic cwd) reports `worktreeRemoved: false` without erroring.
    const viewer = (await import("@/shared/stores/viewer-store")).useViewerStore.getState();
    const workspaces = Object.values(viewer.workspaces).flat();
    const ws = workspaces.find((w) => w.id === target || w.storyId === target);
    if (!ws || !ws.cwd || ws.kind !== "story") {
      return { ...(base ?? {}), worktreeRemoved: false, reason: "noManagedWorktree" };
    }
    const { listManagedWorktrees, removeWorktree: removeWorktreeCmd } =
      await import("@/tauri/commands");
    const listing = await listManagedWorktrees();
    // Match a story-kind row by path first, then by its storyId when known.
    // Epic-integration and orphan rows carry no `storyId`, so the match is
    // narrowed with the discriminator before accessing it.
    const row = listing.rows.find((r) => {
      if (r.path === ws.cwd) return true;
      if (r.kind === "story" && ws.storyId) return r.storyId === ws.storyId;
      return false;
    });
    if (!row?.repoPath) {
      return { ...(base ?? {}), worktreeRemoved: false, reason: "detachedWorktree" };
    }
    const force = discardUnsaved === true;
    try {
      // SW-WA-F7 B1 — the SW-XS-4 two-consent split must NOT collapse here.
      // `force` (= `discard_code`) is the user's escape hatch for uncommitted
      // tracked work; `discardContext` is ALWAYS true because the CodeSense-
      // managed hydrated files ARE the baseline (AC-8). Collapsing both would
      // refuse to close a workspace whose only "dirtiness" is managed context.
      await removeWorktreeCmd(row.repoPath, ws.cwd, force, /* discardContext */ true);
      return { ...(base ?? {}), worktreeRemoved: true };
    } catch (error) {
      // Keep the base session-close result (it already succeeded); surface the
      // reap error on the response so the dashboard can show it.
      return {
        ...(base ?? {}),
        worktreeRemoved: false,
        reason: "worktreeReapFailed",
        message: error instanceof Error ? error.message : String(error),
      };
    }
  },

  // Resolve a concrete tab id before closing: a matching session string is not a tab identity.
  closeTerminalTab: async (args) => {
    const { handle } = (args ?? {}) as { handle: unknown };
    if (typeof handle !== "string" || !handle) {
      throw new Error("closeTerminalTab: {handle} must be a non-empty string");
    }
    const { useViewerStore, selectRunningTerminalTabIndex, selectMinimizedTerminalTabIndex } =
      await import("@/shared/stores/viewer-store");
    const viewer = useViewerStore.getState();
    for (const index of [
      selectRunningTerminalTabIndex(viewer),
      selectMinimizedTerminalTabIndex(viewer),
    ]) {
      for (const [teamKey, workspaces] of Object.entries(index)) {
        for (const [workspaceId, tabs] of Object.entries(workspaces)) {
          const tab = tabs.find((tab) => tab.id === handle);
          if (!tab) continue;
          await viewer.removeTerminalTab(teamKey, tab.id, workspaceId, {
            kill: true,
            replace: false,
            throwOnError: true,
          });
          return { closed: true };
        }
      }
    }
    throw new Error("unavailable: no such tab");
  },

  // Stop any live session by its roster handle, as the Agents panel's Stop does:
  // a Weft worker that never declared done is marked needs-attention first, then
  // the kill choke frees its cap slot and writes one audit line. Resolves once the
  // stop is delivered (a gone handle is a no-op, so a doc can stop defensively);
  // the roster's own refresh then reaches `onSessions`. A durable Weft row stays
  // listed after its process dies, so the roster cannot say "stopped" itself.
  stopSession: async (args) => {
    const { handle } = (args ?? {}) as { handle: unknown };
    if (typeof handle !== "string" || !handle) {
      throw new Error("stopSession: {handle} must be a non-empty string");
    }
    const row = (await readAgentListSessions()).find((r) => r.handle === handle);
    if (row?.sessionId) {
      const worker = await wvRosterForSession(row.sessionId).catch(() => null);
      if (worker?.status === "dispatch") await wvWorkerForceClose(handle).catch(() => {});
    }
    const { endAgentSession } = await import("@/features/viewer/lib/agent-stop-action");
    await endAgentSession(row ?? { handle });
    return { stopped: true };
  },

  // ── SW-GW-2 send-tier verb — drive a generic (dir) workspace tab ────────────
  // `sendToWorkspaceTab({sessionId, mode, text}) → {submitted}` delivers a shell
  // `command` or an agent `prompt` to a tab addressed by sessionId. Like
  // sendToSession it is a SEND verb (rate-guarded), but it is handle-addressed on
  // the store (a generic-workspace tab), not a story binding. The sessionId must
  // belong to an OPEN generic workspace tab; a prompt to a non-live-agent tab
  // refuses.
  sendToWorkspaceTab: async (args, ctx) => {
    if (!launchController(ctx)) {
      throw new Error("unavailable: no active dashboard workspace context");
    }
    return sendToWorkspaceTab(
      (args ?? {}) as { sessionId?: unknown; mode?: unknown; text?: unknown },
      makeRealSendToWorkspaceTabDeps(),
    );
  },

  // ── SW-DX-11 spawn-tier verb — write-capable skill dispatch ────────────────
  // `runSkill({skill, args})` runs a REGISTERED skill on the user's agent CLI
  // with tool use ENABLED + multi-turn — the write-capable sibling of `analyze`.
  // Bounded by a dedicated Rust SpawnCap (`busy` on saturation). A direct headless
  // command call like `analyze` — no doc/session context; the skill catalog is
  // whatever is registered on the user's machine (an uninstalled skill is a config
  // gap the CLI handles as an ordinary prompt, NOT a distinct typed error).
  runSkill: (args, ctx) => {
    const { skill, args: skillArgs } = (args ?? {}) as { skill: unknown; args: unknown };
    if (typeof skill !== "string" || !skill) {
      throw new Error("runSkill: {skill} must be a non-empty string");
    }
    const list = Array.isArray(skillArgs)
      ? skillArgs.filter((a): a is string => typeof a === "string")
      : [];
    return runSkillCommand(skill, list, ctx?.frame?.frameId, ctx?.docPath);
  },

  // ── SW-DX-11 read-action verb — the doc-scoped file read ──────────────────
  // `readWorkspace({path})` reads a file (or lists a dir with a trailing `/` or
  // `./`) STRICTLY within the doc's own directory subtree on a managed surface.
  // The doc supplies only `path`; the trusted controller supplies the doc's own
  // path + the reviewed extension whitelist (`READ_WORKSPACE_EXTS`), and Rust is
  // the confinement/traversal/managed wall. Audited (`read:true`) in
  // `use-action-gate`, never rate-guarded (a read isn't a write). Forwards to the
  // controller because the doc's own path lives in the render scope, not here.
  ...Object.fromEntries(
    ["list", "read", "move", "delete", "create"].map((operation) => [
      `files.${operation}`,
      async (args: unknown, ctx?: BridgeContext) =>
        fileSurfaceCall(operation, args, ctx?.frame?.frameId),
    ]),
  ),
  readWorkspace: async (args, ctx) => {
    const { path } = (args ?? {}) as { path: unknown };
    if (typeof path !== "string" || !path) {
      throw new Error("readWorkspace: {path} must be a non-empty string");
    }
    const controller = launchController(ctx);
    if (!controller) throw new Error("unavailable: no active dashboard workspace context");
    return controller.readWorkspace(path, ctx?.frame?.frameId);
  },

  // ── SW-GW-3 read-action verb — a story's live workspace state (OBSERVE) ─────
  // `getWorkspaceStatus({storyId}) → {exists, live, sessionId}` lets a board
  // render the HONEST affordance — `exists:false → Launch`, `exists:true → Focus`,
  // `live:true → running` — for a story WITHOUT having opened it. Open-state comes
  // from the viewer-store open list (launch-source-agnostic: a story opened in the
  // normal viewer is observed too, the origin bug), liveness from a real
  // `try_wait` probe. A read: audited like `readWorkspace`. Forwards to the
  // controller (which owns the viewer-store access), so it fails closed with no
  // dashboard mounted.
  getWorkspaceStatus: async (args, ctx) => {
    const { storyId } = (args ?? {}) as { storyId: unknown };
    if (typeof storyId !== "string" || !storyId) {
      throw new Error("getWorkspaceStatus: {storyId} must be a non-empty string");
    }
    const controller = launchController(ctx);
    if (!controller) throw new Error("unavailable: no active dashboard workspace context");
    return controller.getWorkspaceStatus(storyId);
  },

  // ── SW-DX-11 §5 read verb — the containing epic's handle (doc discovery) ───
  // `getEpicHandle()` returns the `epicDocPath` of the epic workspace hosting
  // this dashboard (or `null`). Read-only + audit-exempt: it exposes only the
  // doc's OWN location (resolved trusted-side), so a doc can address its epic's
  // running agent via `sendToSession(target)` WITHOUT hard-coding a
  // machine-specific absolute path. Returns null when no dashboard is
  // mounted or the doc isn't inside an epic.
  getEpicHandle: async (_args, ctx) => {
    const controller = launchController(ctx);
    return controller ? controller.getEpicHandle() : null;
  },

  // ── SW-DX-14 read verb — the dashboard's OWN directory (doc discovery) ──────
  // `getDocDir()` returns the absolute directory of this dashboard doc (or
  // `null`). Read-only + audit-exempt: like `getEpicHandle` it exposes only the
  // doc's OWN location (resolved trusted-side), letting a board build an absolute
  // path to a sibling story for `runSkill` WITHOUT an open epic workspace (the
  // skill's agent CLI runs in a different cwd, so it needs an absolute path).
  getDocDir: async (_args, ctx) => {
    const controller = launchController(ctx);
    return controller ? controller.getDocDir() : null;
  },

  // ── SW-ODC-6 read verb — validate a YAML / frontmatter / JSON snippet ──────
  // `validateYaml({ kind, text }) → { ok: boolean, errors: Array<{ line, col, message }> }`.
  // Read-only: parses the supplied text against the shape named by `kind` (one of
  // the slugs in `codesense-docs/getting-started/content/generated/schemas.json`,
  // e.g. "agent", "routine", "org", "teams", "worktree", "bundle",
  // "story-frontmatter", "epic-frontmatter"). Shipped today as a stable shape
  // without the Rust-side validator wired in — the Rust validators (model.rs
  // ValidationError, raw_yaml.rs parse_*_yml, …) land under a later story that
  // will swap the handler body below for a real `invoke`. The error contract
  // (shape + prefixes) is already locked so the schema-browser dashboard can
  // call this verb end-to-end today and fall back cleanly to its in-page parser
  // when `unavailable:` surfaces here.
  //
  // Audit-exempt (READ_VERBS entry below): pure parsing, no writes, no I/O.
  validateYaml: async (args) => {
    if (!args || typeof args !== "object") {
      throw new Error("validateYaml: {kind, text} required");
    }
    const { kind, text } = args as { kind?: unknown; text?: unknown };
    if (typeof kind !== "string" || !kind.trim()) {
      throw new Error("validateYaml: kind must be a non-empty string");
    }
    if (typeof text !== "string") {
      throw new Error("validateYaml: text must be a string");
    }
    // The schema browser's in-page parser is the validator; this verb always
    // answers `unavailable:` so callers fall back to it.
    throw new Error("unavailable: validateYaml has no app validator; validate in the page");
  },

  // ── SW-LD-2 chose the injected-global path, NOT a bridge entry ────────────
  // The authenticated-fetch proxy is reached by a doc via a plain same-origin
  // `fetch(window.__codesenseProxy.base + "/api/v1/proxy/…", { headers: {
  // "X-Codesense-Proxy-Token": window.__codesenseProxy.token } })` — simpler than
  // an RPC round-trip. So the reserved `proxyFetch` slot stays intentionally
  // UNfilled; the capability is injected server-side (`viewer.rs`) into every live
  // render, not via this map.
};

// Contract views derived from the native registry. Admission is owned by Rust.
export const READ_VERBS: readonly string[] = VERB_REGISTRY.filter((v) => !v.action).map(
  (v) => v.name,
);
export const METADATA_ACTION_VERBS: readonly string[] = VERB_REGISTRY.filter(
  (v) => v.class === "metadata" || v.class === "prefs",
).map((v) => v.name);
export const SPAWN_ACTION_VERBS: readonly string[] = VERB_REGISTRY.filter(
  (v) => v.class === "spawn",
).map((v) => v.name);
export const SEND_ACTION_VERBS: readonly string[] = VERB_REGISTRY.filter(
  (v) => v.class === "send",
).map((v) => v.name);
export const READ_ACTION_VERBS: readonly string[] = VERB_REGISTRY.filter(
  (v) => v.action && v.class === "read",
).map((v) => v.name);
export const MANAGEMENT_ACTION_VERBS: readonly string[] = VERB_REGISTRY.filter(
  (v) => v.class === "manage",
).map((v) => v.name);
export const ACTION_VERBS: readonly string[] = VERB_REGISTRY.filter((v) => v.action).map(
  (v) => v.name,
);
export const isReadActionVerb = (verb: string): boolean => READ_ACTION_VERBS.includes(verb);
export const isSpawnVerb = (verb: string): boolean => SPAWN_ACTION_VERBS.includes(verb);
export const isManagementVerb = (verb: string): boolean => MANAGEMENT_ACTION_VERBS.includes(verb);

/** Error thrown when a doc requests a command outside the allowlist. The parent
 *  turns this into a `{ ok:false, error }` reply — it never reaches raw `invoke`. */
export class BridgeCommandNotAllowedError extends Error {
  constructor(command: string) {
    super(`codesense bridge: command "${command}" is not allowlisted`);
    this.name = "BridgeCommandNotAllowedError";
  }
}

/**
 * Run an allowlisted bridge command. Throws `BridgeCommandNotAllowedError` for
 * anything not in `HTML_BRIDGE_ALLOWLIST`; otherwise awaits the mapped handler
 * and returns its result (or lets the handler's own rejection propagate — the
 * parent serializes it into the RPC error reply).
 *
 * The caller runs this handler only after the native dispatcher admits it.
 */
export async function runBridgeCommand(
  command: unknown,
  args: unknown,
  ctx: BridgeContext = {},
): Promise<unknown> {
  // `hasOwnProperty.call` (not `command in`) so inherited keys like
  // "constructor"/"toString" can't smuggle a call past the allowlist.
  if (
    typeof command !== "string" ||
    !Object.prototype.hasOwnProperty.call(HTML_BRIDGE_ALLOWLIST, command)
  ) {
    throw new BridgeCommandNotAllowedError(String(command));
  }
  return HTML_BRIDGE_ALLOWLIST[command](args, ctx);
}

/** Preview the same reply destination the inbox handler uses; reading marks nothing read. */
export async function resolveBridgeReplyRecipient(
  args: unknown,
  ctx?: BridgeContext,
): Promise<string | undefined> {
  const a = argObject(args, "replyAsHuman");
  const team = await inboxTeam(ctx, "replyAsHuman", a);
  return (await inboxReplyRecipient(team, messageId(a.messageId, "replyAsHuman"))) ?? undefined;
}

export type FrameCallIdentity = { origin: string; token: unknown };

/** Resolve the credential on every call so teardown revocation takes effect
 * before any command dispatch or rate-budget accounting. */
export async function resolveFrameCall(
  frame: FrameRecord,
  call?: FrameCallIdentity,
): Promise<FrameGrant> {
  if (
    !call ||
    call.origin !== frame.origin ||
    typeof call.token !== "string" ||
    call.token !== frame.token
  ) {
    throw new Error("denied: frame identity");
  }
  const grant = await resolveFrameGrant(call.origin, call.token);
  if (grant.frameId !== frame.frameId) throw new Error("denied: frame identity");
  return grant;
}

export async function checkFrameGrant(
  grant: FrameGrant,
  command: string,
  args: unknown,
  frame: FrameRecord,
): Promise<void> {
  if (grant.scope === "open" || grant.slot !== "message") return;
  const a = (args ?? {}) as Record<string, unknown>;
  if (
    (a.org !== undefined && a.org !== grant.team?.org) ||
    (a.team !== undefined && a.team !== grant.team?.team)
  ) {
    throw new Error("denied: message team");
  }
  if (command === "openFile") {
    const target = await bridgeResolveOpenTarget(
      frame.contentRoot + "/inbox.html",
      String(a.path ?? ""),
      grant.team?.team,
    );
    if (target.org !== grant.team?.org || target.team !== grant.team?.team)
      throw new Error("denied: message file scope");
  }
  if (["markRead", "markUnread", "archive"].includes(command) && grant.team && frame.message) {
    if (!Array.isArray(a.messageIds) || a.messageIds.some((id) => typeof id !== "number")) {
      throw new Error("denied: message thread");
    }
    // The list view is capped at 500 rows. Resolve ancestry by ID in the stored
    // inbox so a recent reply can still mutate its own older thread.
    await resolveFrameGrant(frame.origin, frame.token, a.messageIds as number[]);
  }
}
