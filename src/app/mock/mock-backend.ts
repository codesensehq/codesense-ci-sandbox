// SPDX-License-Identifier: MIT
import { liveHubFixture } from "./live-hub-fixture";
import { createInvokeRouter } from "./mock-router";
import type { HandlerRegistry, InvokeRouter } from "./mock-router";
import { createMockState } from "./mock-state";
import { verbHandlers } from "./handlers/verbs";
import { bootHandlers } from "./handlers/boot";
import { contentFileHandlers } from "./handlers/content-files";
import { worktreeEditorHandlers } from "./handlers/worktree-editor";
import { teamHandlers } from "./handlers/collections";
import { gitSyncHandlers } from "./handlers/git-sync";
import { setupHandlers } from "./handlers/setup";
import { worktreeHandlers } from "./handlers/worktree";
import { homeBaseHandlers } from "./handlers/home-base";
import { attachmentHandlers } from "./handlers/attachments";
import { workspaceFlowHandlers } from "./handlers/workspace-flows";
import { terminalServerHandlers } from "./handlers/terminal-server";
import { machineHandlers, resolveMachineFixture } from "./handlers/machines";
import { terminalLayoutHandlers } from "./handlers/terminal-layout";
import { routineHandlers } from "./handlers/routines";
import { agentLaunchHandlers } from "./handlers/agents-launch";
import { agentHandlers } from "./handlers/agents";
import { roleHandlers } from "./handlers/roles";
import { pushViewerStateHandler } from "./handlers/agent-list-hub";
import { agentTeamsOrgHandlers, isAgentTeamsWorld } from "./handlers/agents-team";
import { agentContextHandlers, isAgentContextWorld } from "./handlers/agent-context";
import { isLeftNavDemo, leftNavDemoHandlers } from "./handlers/left-nav-demo";
import { navGitHandlers } from "./handlers/nav-git";
import { teamEditsHandlers } from "./handlers/team-edits";
import { paneHandlers } from "./handlers/panes";
import { contextQueryHandlers } from "./handlers/context-query";
import { sessionTimerHandlers } from "./handlers/session-timers";
import { recordsHandlers } from "./handlers/records";
import { notesHandlers } from "./handlers/notes";
import { sourceDiffHandlers } from "./handlers/source-diff";
import { MockEventBus } from "./mock-events";
import { resolveScenario } from "./scenario";
import type { MockScenario } from "./scenario";
import type { OrgSummary } from "@/shared/types/domain";
import type { LoadedOrg } from "@/shared/types/registry";
import { pathToTeamContentDirSync } from "@/shared/lib/team-content-path";

/**
 * Assemble the mock backend for the carved onboarding/config surface: one
 * session-scoped in-memory state, every command-handler group wired to it,
 * folded into a single `invoke` router. The router is what the
 * `__TAURI_INTERNALS__` shim calls for every backend request.
 *
 * The `scenario` selects which first-run world the fixtures describe (resolved
 * from the `?scenario=` URL param at install time). Handler groups whose output
 * depends on how far onboarding has progressed take it; the CRUD/read groups do
 * not. The raw
 * `search` string carries the per-surface worlds beyond the scenario (e.g.
 * `?terminalServer=incompatible`, `?terminals=demo`), and `?machines=` selects the
 * Remote Machines world (the default is This Mac only, so every other surface boots
 * unchanged). `events` delivers the few events the mock emits (the terminal layout's
 * changes).
 */
export function createMockBackend(
  scenario: MockScenario = resolveScenario(""),
  search = "",
  events: MockEventBus = new MockEventBus(),
): InvokeRouter {
  const state = createMockState();
  const terminals = terminalLayoutHandlers(search, events);
  const contentFiles = contentFileHandlers(state);
  const handlers: HandlerRegistry = {
    ...bootHandlers(scenario),
    ...verbHandlers,
    ...teamHandlers(),
    ...setupHandlers(),
    ...worktreeHandlers(scenario),
    ...homeBaseHandlers(),
    ...attachmentHandlers(scenario),
    ...workspaceFlowHandlers(),
    ...contentFiles,
    ...worktreeEditorHandlers(contentFiles, search),
    ...gitSyncHandlers(search),
    ...events.handlers(),
    ...terminals,
    ...terminalServerHandlers(search, terminals.terminal_open_tab),
    ...machineHandlers(resolveMachineFixture(search)),
    ...routineHandlers(events),
    ...agentLaunchHandlers(),
    ...roleHandlers(events),
    ...agentHandlers(
      events,
      search,
      () =>
        handlers.agents_list({ includeArchived: true }) as import("@/tauri/commands").AgentRow[],
    ),
    ...paneHandlers(events, () => JSON.parse(String(handlers.load_app_state({}) ?? "{}"))),
    ...contextQueryHandlers(events),
    ...sessionTimerHandlers(events, search),
    ...sourceDiffHandlers(search),
    ...notesHandlers(events, search),
  };
  // SW-XS-15: the left nav's agent context world, opt-in (`?context=demo`).
  if (isAgentContextWorld(search))
    Object.assign(handlers, agentContextHandlers(state, events, handlers, search));
  // The left nav's demo world, opt-in (`?nav=demo`).
  if (isLeftNavDemo(search)) Object.assign(handlers, leftNavDemoHandlers(state, events, handlers));
  Object.assign(handlers, navGitHandlers(search));
  // SW-XS-27: the unsaved team edits world, opt-in (`?edits=demo`).
  Object.assign(handlers, teamEditsHandlers(search, { ...handlers }));
  // The `?agents=teams` world's second org: discoverable and loadable beside the fixture org, its
  // team with its own avatar pack. Applied last so no later world replaces it; it wraps whatever
  // the other groups answer for every other org and team.
  if (isAgentTeamsWorld(search)) Object.assign(handlers, agentTeamsOrgHandlers({ ...handlers }));
  // Configured fixture teams have content directories even outside terminal demo worlds.
  const terminalPathExists = handlers.check_path_exists;
  handlers.check_path_exists = async (args) => {
    const orgs = (await handlers.discover_local_orgs({})) as OrgSummary[];
    for (const summary of orgs) {
      const { org } = (await handlers.load_org_cmd({ slug: summary.slug })) as LoadedOrg;
      if (
        org?.teams.some((team) => pathToTeamContentDirSync(org.org_root, team.name) === args.path)
      )
        return true;
    }
    return terminalPathExists ? terminalPathExists(args) : false;
  };
  handlers.live_hub_view_key = async ({ origin, token }) => {
    await handlers.resolve_frame_grant?.({ origin, token });
    // The mock frame server mints open grants only.
    return "all";
  };
  handlers.push_viewer_state = pushViewerStateHandler(events);
  const saveState = handlers.save_app_state;
  handlers.save_app_state = async (args) => {
    await saveState(args);
    for (const scope of ["global", "principal"])
      handlers.configuration_prepare({
        topic: "prefs",
        scope,
        expectedPrincipal: args.expectedPrincipal,
      });
  };
  const sourceSubscribe = handlers.live_hub_subscribe;
  handlers.live_hub_subscribe = async (args) =>
    args.topic === "agentDefinitions"
      ? handlers.agent_definitions_prepare({})
      : ["paneSources", "profile", "workspaceConfig", "prefs"].includes(String(args.topic))
        ? sourceSubscribe(args)
        : ((await handlers.live_hub_agent_list_read?.(args)) ?? {
            topic: String(args.topic),
            viewKey: String(args.viewKey),
            rev: 0,
            snapshot:
              args.topic === "inbox"
                ? []
                : (
                    ((await handlers.agent_read_sessions?.({})) as Array<
                      Record<string, unknown>
                    >) ?? []
                  ).map((row) => ({ id: row.handle, ...row })),
          });
  if (new URLSearchParams(search).get("harness") === "live-hub")
    Object.assign(handlers, liveHubFixture(events));
  Object.assign(handlers, recordsHandlers(events, { ...handlers }));
  return createInvokeRouter(handlers);
}
