// SPDX-License-Identifier: MIT
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { openRoutineDraft } from "@/features/routines/routines-virtual-tab";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { createMockAgents, type MockAgents } from "@/app/mock/handlers/agents";
import { ROUTINES_CHANGED_EVENT } from "@/tauri/events";
import { resetRoutinesStoreForTests } from "@/features/routines/stores/routines-store";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { OwningScopeProvider, warmScopes } from "@/shared/lib/warm-scopes";

import { resetAgentsStoreForTests, useAgentsStore, watchAgentList } from "../stores/agents-store";
import type { HubSnapshot } from "@/features/dashboard/lib/live-hub";
import { openRoleEditor, useRoleEditorStore } from "../stores/role-editor-store";
import { useRoleConfigsStore } from "@/shared/hooks/use-role-configs";
import { IdentityMenu } from "@/features/shell/components/identity-menu";
import { AgentsTab, DELETE_WARNING } from "./agents-tab";
import { showAgent } from "./agents-tab-nav";
import { useLaunchSheetStore } from "../lib/launch-sheet";
import { BOOTSTRAP_NOTE } from "./agent-manifest-panel";
import type { AgentRow } from "@/tauri/commands";
import { PREVIEW_RUNTIME_NOTE, registeredCli } from "@/shared/lib/agent-runtimes";
import { agentCliLabel } from "@/shared/lib/agent-clis";

const NO_LOGIN_STARTERS = (
  createMockAgents(new MockEventBus()).handlers.agents_list({}) as AgentRow[]
).filter((row) => row.key === `builtin/${row.runtime}` && row.account.provider === null);

/**
 * The Agents tab driven through the same door the app uses (`window.__TAURI_INTERNALS__`),
 * over the RF-29 mock agents backend: every read is a real command round-trip and every
 * update arrives as an `agents:changed` event. Only the launch controller is faked, at the
 * one module the dialog lazy-loads, so the dispatched open-workspace arg can be checked.
 */

const { openTabs } = vi.hoisted(() => ({
  openTabs: [] as string[],
}));

vi.mock("@/shared/lib/open-management-tab", () => ({
  openManagementTab: (path: string) => openTabs.push(path),
}));

const TRIAGE = "acme/backend/triage";
const SCRIBE = "acme/backend/scribe";

let world: MockAgents;
let events: MockEventBus;

let activeAccounts: Partial<Record<"claude" | "codex", string>>;

beforeEach(() => {
  useAgentListProjectionStore.setState({ spend: {} });
  useRoleEditorStore.setState({ teamKey: null, role: null, dirty: false, pendingLeave: null });
  openTabs.length = 0;
  resetAgentsStoreForTests();
  resetRoutinesStoreForTests();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
  useViewerStore.setState({
    agentsListFilter: null,
    workspacePanelCollapsed: true,
    layoutMode: "viewer",
    terminalVisible: false,
    rightPaneOpen: false,
  });
  useLocalProfileStore.setState({
    status: {
      active: { principal: "1", displayName: "Alice" },
    },
  } as unknown as Parameters<typeof useLocalProfileStore.setState>[0]);
  events = new MockEventBus();
  activeAccounts = {};
  world = createMockAgents(events, { running: [`${TRIAGE}@3`], active: activeAccounts });
  const internals = buildTauriInternals(
    createInvokeRouter({ ...events.handlers(), ...world.handlers }),
  );
  events.bind(internals.runCallback);
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = internals;
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
});

afterEach(() => {
  cleanup();
  resetAgentsStoreForTests();
  resetRoutinesStoreForTests();
});

function renderTab(hostTeamKey?: string) {
  return render(
    <TooltipProvider>
      <IdentityMenu />
      <AgentsTab filePath="virtual://agents" teamContentPath="" hostTeamKey={hostTeamKey} />
    </TooltipProvider>,
  );
}

const rowKeys = () => screen.getAllByTestId("agent-row").map((r) => r.dataset.agentKey);
const rowOf = (key: string) =>
  screen.getAllByTestId("agent-row").find((r) => r.dataset.agentKey === key)!;

async function loaded() {
  renderTab();
  await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
}

async function openAgent(key: string, name: string) {
  fireEvent.click(within(rowOf(key)).getByRole("button", { name: `Open ${name}` }));
  await screen.findByTestId("agent-overview");
}

async function expectPermissions(text: string) {
  await waitFor(() =>
    expect(screen.getByTestId("agent-sentence-permissions")).toHaveTextContent(text),
  );
}

/** An action from a row's ⋯ menu. */
async function rowMenu(key: string, name: string, item: string) {
  fireEvent.keyDown(within(rowOf(key)).getByRole("button", { name: `More actions for ${name}` }), {
    key: "Enter",
  });
  fireEvent.click(await screen.findByRole("menuitem", { name: `${item} ${name}` }));
}

it("keeps a routine seed through the real agent choice and opens its filled form under the chosen owner", async () => {
  await loaded();
  await openAgent(TRIAGE, "Triage");
  const seed = {
    audience: "personal" as const,
    draft: { name: "Tidy my notes", task: "Tidy note alpha", when: "weekday 18:00" },
  };
  act(() => openRoutineDraft(seed));
  expect(useAgentsStore.getState().selectedKey).toBeNull();
  expect(useRoutinesStore.getState().creating).toBe(false);
  expect(screen.queryByLabelText("Task")).not.toBeInTheDocument();
  fireEvent.click(within(rowOf(SCRIBE)).getByRole("button", { name: "Open Scribe" }));
  expect(await screen.findByLabelText("Task")).toHaveValue(seed.draft.task);
  expect(screen.getByLabelText("Name")).toHaveValue(seed.draft.name);
  expect(screen.getByLabelText("When")).toHaveValue(seed.draft.when);
  expect(useAgentsStore.getState().selectedKey).toBe(SCRIBE);
  expect(useRoutinesStore.getState()).toMatchObject({
    routineOwner: SCRIBE,
    creating: true,
    createSeed: seed,
  });
});

describe("Agents tab", () => {
  it.each(NO_LOGIN_STARTERS)(
    "shows no CodeSense login affordance for $runtime",
    async ({ runtime, name }) => {
      await loaded();
      fireEvent.click(screen.getByRole("button", { name: "New agent" }));
      const picker = await screen.findByTestId("new-agent-dialog");
      const template = within(picker)
        .getAllByTestId("new-agent-template")
        .find((row) => row.dataset.templateKey === `builtin/${runtime}`)!;
      fireEvent.click(template);
      fireEvent.click(within(picker).getByRole("button", { name: `Use ${name}` }));
      await waitFor(() =>
        expect(useAgentsStore.getState().selectedKey).toBe(`_personal/1/personal/${runtime}-copy`),
      );
      const key = `_personal/1/personal/${runtime}-copy`;
      // A CLI that can't launch without a model endpoint says that line instead.
      expect(
        within(rowOf(key)).queryByTestId("agent-row-line") ??
          within(rowOf(key)).getByTestId("agent-account-warning"),
      ).not.toHaveTextContent("your own login");
      await openAgent(key, name);
      expect(screen.getByTestId("agent-strip-account")).toHaveTextContent("No CodeSense login");
      expect(screen.getByText("Model settings are in Edit → Runs on")).toBeVisible();
      expect(screen.queryByTestId("agent-strip-account-action")).toBeNull();
    },
  );

  it("a retained Agents tab of a hidden team renders no second discard dialog", async () => {
    useViewerStore.setState((state) => ({
      activeTab: {
        ...state.activeTab,
        "acme:backend": "virtual://agents",
        "acme:other": "virtual://agents",
      },
    }));
    act(() => warmScopes.activate("acme:backend"));
    render(
      <TooltipProvider>
        {["acme:backend", "acme:other"].map((team) => (
          <OwningScopeProvider key={team} scope={team}>
            <AgentsTab filePath="virtual://agents" teamContentPath="" hostTeamKey={team} />
          </OwningScopeProvider>
        ))}
      </TooltipProvider>,
    );
    await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
    act(() => useAgentsStore.setState({ pendingLeave: { kind: "create" } }));
    expect(await screen.findAllByTestId("agent-discard-confirm")).toHaveLength(1);
  });

  it("keeps a list-only consumer live while the selected detail is hidden, then catches up on reveal", async () => {
    const teamKey = "acme:backend";
    useViewerStore.setState((state) => ({
      activeTab: { ...state.activeTab, [teamKey]: "virtual://agents" },
    }));
    renderTab(teamKey);
    await screen.findAllByTestId("agent-row");
    fireEvent.click(within(rowOf(TRIAGE)).getByRole("button", { name: "Open Triage" }));
    await waitFor(() => expect(useAgentsStore.getState().details[TRIAGE]).toBeDefined());
    const overview = await screen.findByTestId("agent-overview");
    const stop = watchAgentList(false);
    try {
      act(() =>
        useViewerStore.setState((state) => ({
          activeTab: { ...state.activeTab, [teamKey]: "README.md" },
        })),
      );
      await waitFor(() => {
        const view = world.handlers.agent_definitions_prepare({}) as HubSnapshot;
        expect(view.snapshot.some((row) => row.rowType === "detail")).toBe(false);
      });
      act(() =>
        world.editAs(
          TRIAGE,
          { name: "Renamed while detail is hidden" },
          { kind: "user", name: "Bob" },
        ),
      );
      await waitFor(() =>
        expect(useAgentsStore.getState().rows!.find((row) => row.key === TRIAGE)!.name).toBe(
          "Renamed while detail is hidden",
        ),
      );
      expect(useAgentsStore.getState().details[TRIAGE].definition.name).toBe("Triage");
      act(() =>
        useViewerStore.setState((state) => ({
          activeTab: { ...state.activeTab, [teamKey]: "virtual://agents" },
        })),
      );
      await waitFor(() =>
        expect(useAgentsStore.getState().details[TRIAGE].definition.name).toBe(
          "Renamed while detail is hidden",
        ),
      );
      expect(useAgentsStore.getState().selectedKey).toBe(TRIAGE);
      expect(screen.getByTestId("agent-overview")).toBe(overview);
    } finally {
      stop();
    }
  });

  it("groups Mine then one group per team, each by name, every row with its facts", async () => {
    await loaded();
    const mine = screen.getByRole("region", { name: "Mine" });
    expect(
      within(mine)
        .getAllByTestId("agent-row")
        .map((r) => r.dataset.agentKey),
    ).toEqual(["_personal/1/personal/dev"]);
    const team = screen.getByRole("region", { name: "backend · acme" });
    expect(
      within(team)
        .getAllByTestId("agent-row")
        .map((r) => r.dataset.agentKey),
    ).toEqual(["acme/backend/reviewer", SCRIBE, TRIAGE]);
    // The archived agent stays out until asked for.
    expect(rowKeys()).not.toContain("acme/backend/writer");

    const triage = within(rowOf(TRIAGE));
    expect(triage.getByTestId("agent-row-line").textContent).toBe("Claude · opus · on claude-work");
    expect(triage.getByTestId("agent-running").textContent).toBe("1 running");
    // Every row is fully actionable, team ones included: Launch and the ⋯ menu.
    expect(triage.getByRole("button", { name: "Launch Triage" })).toBeTruthy();
    expect(triage.getByRole("button", { name: "More actions for Triage" })).toBeTruthy();

    const reviewer = within(rowOf("acme/backend/reviewer"));
    expect(reviewer.getByTestId("agent-row-line").textContent).toBe(
      "Codex · gpt-5 · your own login",
    );
    expect(reviewer.queryByTestId("agent-running")).toBeNull();
    // A binding to a detached account is a warning with a Fix, never a silent fallback.
    const scribe = within(rowOf(SCRIBE));
    expect(scribe.getByTestId("agent-account-warning").textContent).toContain("detached");
    expect(scribe.getByRole("button", { name: "Fix the account for Scribe" })).toBeTruthy();
    expect(scribe.queryByRole("button", { name: "Launch Scribe" })).toBeNull();
  });

  it("lists Routines before the Spend cap, both as plain sections with the same heading", async () => {
    await loaded();
    await openAgent(TRIAGE, "Triage");
    const routines = await screen.findByTestId("agent-routines");
    const cap = screen.getByRole("region", { name: "Spend cap" });
    expect(routines.compareDocumentPosition(cap) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    for (const section of [routines, cap]) {
      expect(section.className).not.toMatch(/\bborder\b/);
      expect(section.className).not.toMatch(/\bbg-/);
    }
    const heading = (el: HTMLElement) => within(el).getAllByRole("heading", { level: 3 })[0];
    expect(heading(routines).className).toContain("text-sm");
    expect(heading(routines).className).toContain("font-semibold");
    expect(heading(cap).className).toContain("text-sm");
    expect(heading(cap).className).toContain("font-semibold");
  });

  it("names a row's Open button for the agent and describes it with the second line", async () => {
    await loaded();
    const open = within(rowOf(TRIAGE)).getByRole("button", { name: "Open Triage" });
    expect(open).toHaveAccessibleDescription(/Claude · opus · on claude-work/);
    const scribe = within(rowOf(SCRIBE)).getByRole("button", { name: "Open Scribe" });
    expect(scribe).toHaveAccessibleDescription(/the bound account was detached/);
  });

  it("never reorders the list when a row is selected", async () => {
    await loaded();
    const before = rowKeys();
    await openAgent(SCRIBE, "Scribe");
    expect(rowKeys()).toEqual(before);
    expect(rowOf(SCRIBE).dataset.selected).toBe("true");
  });

  it("shows the profile strip and the fact sheet, with manifest errors as a count", async () => {
    await loaded();
    await openAgent(TRIAGE, "Triage");
    // The strip: what it runs on (the model from --model), the account, health, next run.
    expect(screen.getByTestId("agent-strip-runs-on").textContent).toContain("opus");
    expect(screen.getByTestId("agent-strip-runs-on").textContent).toContain("Model from --model");
    expect(screen.getByTestId("agent-strip-account").textContent).toBe("claude-work");
    expect(screen.getByTestId("agent-strip-health").textContent).toContain("1 running");
    expect(screen.getByTestId("agent-routines-link").textContent).toContain("1 routine");
    expect(screen.getByTestId("agent-created-by").textContent).toBe("Created by alice");
    // The sheet: tools, what it starts with, its permissions.
    const facts = within(screen.getByLabelText("Triage at a glance"));
    expect(facts.getByTestId("agent-fact-tools").textContent).toContain("slack");
    expect(facts.getByTestId("agent-fact-tools").textContent).toContain("github");
    expect(facts.getByTestId("agent-fact-starts").textContent).toContain("6 paths · 23 files");
    expect(screen.getByTestId("agent-sentence-permissions").textContent).toBe(
      "5 allow, 2 deny rules",
    );
    expect(screen.queryByTestId("agent-manifest-errors")).toBeNull();
    await openAgent("acme/backend/reviewer", "Reviewer");
    await waitFor(() =>
      expect(screen.getByTestId("agent-sentence-permissions").textContent).toBe(
        "approvals (CLI default) · sandbox (CLI default)",
      ),
    );
    await openAgent(SCRIBE, "Scribe");
    await waitFor(() =>
      expect(screen.getByTestId("agent-manifest-errors").textContent).toBe("1 manifest error"),
    );
    // The mode is part of what runs without asking, so it is always said.
    expect(screen.getByTestId("agent-sentence-permissions").textContent).toBe(
      "no permission rules of its own · dontAsk (default)",
    );
  });

  it.each(["deny", "ask"])(
    "shows a selected OpenCode agent's native %s fallback and keeps permissions navigation",
    async (effect) => {
      world.editAs(
        TRIAGE,
        {
          runtime: "opencode",
          args: [],
          permissions: { opencode: { permission: { "*": effect, read: "allow" } } },
        },
        { kind: "user", name: "alice" },
      );
      await loaded();
      await openAgent(TRIAGE, "Triage");
      await expectPermissions(`2 custom permission rules · unspecified tools ${effect}`);
      await openAgent("acme/backend/reviewer", "Reviewer");
      await expectPermissions("approvals (CLI default) · sandbox (CLI default)");
      await openAgent(SCRIBE, "Scribe");
      await expectPermissions("no permission rules of its own · dontAsk (default)");
      await openAgent(TRIAGE, "Triage");
      await expectPermissions(`2 custom permission rules · unspecified tools ${effect}`);
      fireEvent.keyDown(screen.getByRole("button", { name: "Permissions for Triage" }), {
        key: "Enter",
      });
      expect(useAgentsStore.getState().view).toBe("edit");
      const nav = await screen.findByTestId("agent-editor-nav");
      await waitFor(() =>
        expect(within(nav).getByRole("button", { name: "Permissions" })).toHaveAttribute(
          "aria-current",
          "true",
        ),
      );
    },
  );

  it("shows the bootstrap note in Edit → Starts with, not between the facts and the routines", async () => {
    await loaded();
    await openAgent(TRIAGE, "Triage");
    expect(screen.queryByTestId("agent-bootstrap-note")).toBeNull();
    fireEvent.mouseDown(screen.getByRole("tab", { name: "Edit" }));
    const note = await screen.findByTestId("agent-editor-bootstrap-note");
    expect(note.textContent).toBe(BOOTSTRAP_NOTE);
    expect(screen.getByTestId("agent-manifest-panel").contains(note)).toBe(true);
  });

  it("SW-AN-50: a bridge-only agent says what the team offers on its Tools fact", async () => {
    await loaded();
    // Reviewer carries no MCP servers; its team offers slack/github/linear.
    await openAgent("acme/backend/reviewer", "Reviewer");
    const tools = within(await screen.findByTestId("agent-fact-tools"));
    const line = tools.getByTestId("agent-mcp-offered");
    expect(line.textContent).toContain("The team offers: slack, github, linear");
    // The agent is team-owned (not a starter), so copy says Edit, not Use template.
    expect(line.textContent).toContain("Edit the agent");
    // No banner sits between the fact sheet and the routines.
    expect(screen.getByTestId("agent-overview").querySelector("[role=alert]")).toBeNull();
    // Triage already names servers → no line.
    await openAgent(TRIAGE, "Triage");
    expect(screen.queryByTestId("agent-mcp-offered")).toBeNull();
  });

  /** Open the strip's account picker: Change, or Fix when the account can't launch. */
  async function accountMenu(name: string, verb: "Change" | "Fix" = "Change") {
    const trigger = screen.getByTestId("agent-strip-account-action");
    expect(trigger).toHaveAccessibleName(
      verb === "Fix" ? `Fix the account for ${name}` : `Change the account for ${name}`,
    );
    fireEvent.keyDown(trigger, { key: "Enter" });
    return within(await screen.findByRole("menu"));
  }

  /** Your raw-terminal consent, in the open account picker. */
  const toggle = () =>
    screen.getByRole("menuitemcheckbox", { name: /Retain raw terminal snapshot/ });

  it("changes the account from the strip: only accounts of the agent's CLI, and one shows on the row", async () => {
    await loaded();
    await openAgent(TRIAGE, "Triage");
    // The account control no longer sits at the bottom of the Overview.
    expect(screen.queryByTestId("agent-account-select")).toBeNull();
    expect(screen.queryByText("Your account (only you)")).toBeNull();
    let menu = await accountMenu("Triage");
    expect(menu.getByText("Your account (only you)")).toBeTruthy();
    // An account is the user's own CLI login; CodeSense has no accounts of its own.
    expect(menu.getByTestId("agent-account-hint").textContent).toContain(
      "your own Claude CLI logins",
    );
    expect(document.body.textContent).not.toMatch(/CodeSense account/i);
    const options = () => menu.getAllByRole("menuitemradio");
    await waitFor(() =>
      expect(options().map((o) => o.textContent)).toEqual([
        "Use my default",
        "claude-work",
        "claude-personal",
      ]),
    );
    expect(menu.getByRole("menuitemradio", { name: "claude-work" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(menu.getByRole("menuitemradio", { name: "claude-personal" }));
    await waitFor(() =>
      expect(within(rowOf(TRIAGE)).getByTestId("agent-row-line").textContent).toContain(
        "on claude-personal",
      ),
    );
    expect(screen.getByTestId("agent-strip-account").textContent).toBe("claude-personal");
    menu = await accountMenu("Triage");
    fireEvent.click(menu.getByRole("menuitemradio", { name: "Use my default" }));
    await waitFor(() =>
      expect(within(rowOf(TRIAGE)).getByTestId("agent-row-line").textContent).toContain(
        "your own login",
      ),
    );

    await openAgent("acme/backend/reviewer", "Reviewer");
    menu = await accountMenu("Reviewer");
    await waitFor(() =>
      expect(menu.getAllByRole("menuitemradio").map((o) => o.textContent)).toEqual([
        "Use my default",
        "codex-main",
      ]),
    );
  });

  it("keeps your raw-terminal consent in the account picker, through the binding", async () => {
    await loaded();
    await openAgent(TRIAGE, "Triage");
    let menu = await accountMenu("Triage");
    expect(toggle()).toHaveAttribute("aria-checked", "false");
    expect(menu.getByTestId("agent-raw-terminal-toggle").textContent).toContain("Saves at once");
    fireEvent.click(toggle());
    await waitFor(() => expect(toggle()).toHaveAttribute("aria-checked", "true"));
    // Going back to the default account keeps the consent.
    menu = within(screen.getByRole("menu"));
    fireEvent.click(menu.getByRole("menuitemradio", { name: "Use my default" }));
    await waitFor(() =>
      expect(within(rowOf(TRIAGE)).getByTestId("agent-row-line").textContent).toContain(
        "your own login",
      ),
    );
    menu = await accountMenu("Triage");
    expect(toggle()).toHaveAttribute("aria-checked", "true");
    fireEvent.click(toggle());
    await waitFor(() => expect(toggle()).toHaveAttribute("aria-checked", "false"));
  });

  it("a built-in agent opened from a session's Inbox backlink keeps your account and capture controls", async () => {
    await loaded();
    act(() => showAgent("builtin/claude"));
    await screen.findByTestId("agent-overview");
    // The definition is read-only: no Edit, no Runs on link.
    expect(screen.queryByRole("tab", { name: "Edit" })).toBeNull();
    let menu = await accountMenu("Claude");
    await waitFor(() =>
      expect(menu.getAllByRole("menuitemradio").map((o) => o.textContent)).toContain("claude-work"),
    );
    fireEvent.click(menu.getByRole("menuitemradio", { name: "claude-work" }));
    await waitFor(() =>
      expect(screen.getByTestId("agent-strip-account").textContent).toBe("claude-work"),
    );
    // Capture consent can be granted and withdrawn.
    menu = await accountMenu("Claude");
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /Retain raw terminal snapshot/ }));
    await waitFor(() => expect(toggle()).toHaveAttribute("aria-checked", "true"));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: /Retain raw terminal snapshot/ }));
    await waitFor(() => expect(toggle()).toHaveAttribute("aria-checked", "false"));
  });

  it("a broken-account agent keeps the launch sheet one click away, from the header and the row", async () => {
    await loaded();
    act(() => useLaunchSheetStore.getState().close());
    await openAgent(SCRIBE, "Scribe");
    // Fix is the primary action; Launch… stays beside it, since the account preview is this
    // Mac's and the launch may be for another machine.
    expect(
      screen.getAllByRole("button", { name: "Fix the account for Scribe" }).length,
    ).toBeGreaterThan(0);
    fireEvent.click(screen.getByTestId("agent-detail-launch"));
    expect(useLaunchSheetStore.getState().request).toMatchObject({ agentKey: SCRIBE });
    act(() => useLaunchSheetStore.getState().close());
    await rowMenu(SCRIBE, "Scribe", "Launch");
    expect(useLaunchSheetStore.getState().request).toMatchObject({ agentKey: SCRIBE });
  });

  it("opens the account picker when something asks for it, from whichever view the agent is on", async () => {
    await loaded();
    await openAgent(SCRIBE, "Scribe");
    fireEvent.mouseDown(screen.getByRole("tab", { name: "History" }));
    act(() => useAgentsStore.getState().requestAccountPicker(SCRIBE));
    const menu = await screen.findByRole("menu");
    expect(within(menu).getByText("Choose an account")).toBeTruthy();
    expect(screen.getByRole("tab", { name: "Overview", hidden: true })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    // The request is spent once the picker is open.
    expect(useAgentsStore.getState().request).toBeNull();
  });

  it("explains a detached binding, and Fix opens the same picker as Choose an account", async () => {
    await loaded();
    await openAgent(SCRIBE, "Scribe");
    expect(screen.getByTestId("agent-account-reason").textContent).toContain(
      "the bound account was detached",
    );
    const menu = await accountMenu("Scribe", "Fix");
    expect(menu.getByText("Choose an account")).toBeTruthy();
    fireEvent.click(menu.getByRole("menuitemradio", { name: "Use my default" }));
    await waitFor(() => expect(screen.queryByTestId("agent-account-reason")).toBeNull());
  });

  it("names the Active account on an unpinned agent, in the row and the strip", async () => {
    activeAccounts.codex = "acct-codex";
    await loaded();
    expect(within(rowOf("acme/backend/reviewer")).getByTestId("agent-row-line").textContent).toBe(
      "Codex · gpt-5 · on codex-main (Active)",
    );
    await openAgent("acme/backend/reviewer", "Reviewer");
    expect(screen.getByTestId("agent-strip-account").textContent).toBe("codex-main (Active)");
    expect(screen.queryByTestId("agent-account-reason")).toBeNull();
  });

  it("refuses an unpinned agent whose Active account is gone, with Fix, never as your own login", async () => {
    activeAccounts.codex = "acct-gone";
    await loaded();
    const row = within(rowOf("acme/backend/reviewer"));
    expect(row.queryByTestId("agent-row-line")).toBeNull();
    const line = row.getByTestId("agent-account-warning").textContent;
    expect(line).toBe("Codex · the account no longer exists");
    expect(line).not.toContain("your own login");
    await openAgent("acme/backend/reviewer", "Reviewer");
    expect(screen.getByTestId("agent-strip-account").textContent).toBe("Account unavailable");
    expect(screen.getByTestId("agent-account-reason").textContent).toContain(
      "the account no longer exists",
    );
    expect(screen.getByTestId("agent-strip-account-action").textContent).toBe("Fix");
  });

  it("links N running to the bottom agents list, revealed and filtered to the agent", async () => {
    await loaded();
    const running = within(rowOf(TRIAGE)).getByTestId("agent-running");
    // On hover or focus the word gives way to the actions, which keep the same shortcut as a
    // ghost button beside Launch and ⋯.
    expect(running.closest('[class*="group-hover/row:hidden"]')).not.toBeNull();
    const action = within(rowOf(TRIAGE)).getByTestId("agent-running-action");
    expect(action.closest('[class*="group-hover/row:flex"]')).not.toBeNull();
    expect(action.textContent).toBe("1 running");
    fireEvent.click(action);
    const s = useViewerStore.getState();
    expect(s.agentsListFilter).toEqual({ agentKey: TRIAGE, name: "Triage" });
    expect(s.terminalVisible).toBe(true);
    expect(s.layoutMode).toBe("shared");
    expect(s.workspacePanelCollapsed).toBe(false);
    // The right pane is for user surfaces: the link no longer opens it.
    expect(s.rightPaneOpen).toBe(false);
  });

  it("links N routines to Agents → Schedule filtered to the agent (SPEC §10)", async () => {
    await loaded();
    await openAgent(SCRIBE, "Scribe");
    fireEvent.click(screen.getByTestId("agent-routines-link"));
    expect(useAgentsStore.getState().tabView).toBe("schedule");
    expect(useAgentsStore.getState().scheduleFilter).toEqual({
      kind: "agent",
      agentKey: SCRIBE,
      name: "Scribe",
    });
    expect(openTabs).toContain("virtual://agents");
    expect(openTabs).not.toContain("virtual://routines");
  });

  it("opens an editor section from the fact that names it", async () => {
    await loaded();
    await openAgent(TRIAGE, "Triage");
    fireEvent.click(screen.getByTestId("agent-fact-permissions"));
    expect(useAgentsStore.getState().view).toBe("edit");
    const nav = await screen.findByTestId("agent-editor-nav");
    await waitFor(() =>
      expect(within(nav).getByRole("button", { name: "Permissions" })).toHaveAttribute(
        "aria-current",
        "true",
      ),
    );
  });

  it("shows archived agents on request, with the routines still using them", async () => {
    await loaded();
    fireEvent.click(screen.getByRole("switch", { name: "Show archived agents" }));
    await waitFor(() => expect(rowKeys()).toContain("acme/backend/writer"));
    const writer = within(rowOf("acme/backend/writer"));
    expect(writer.getByTestId("agent-state").textContent).toBe("Archived");
    await openAgent("acme/backend/writer", "Writer");
    expect(screen.getByTestId("agent-routines-link").textContent).toContain("archived · 1 routine");
    fireEvent.click(writer.getByRole("button", { name: "Unarchive Writer" }));
    await waitFor(() =>
      expect(screen.getByTestId("agent-routines-link").textContent).toContain("1 routine"),
    );
    expect(screen.getByTestId("agent-routines-link").textContent).not.toContain("archived");
  });

  it("sends the listed revision, not an older detail, as an action's base", async () => {
    await loaded();
    // Triage's detail is read at revision 3, then another agent is selected.
    await openAgent(TRIAGE, "Triage");
    await openAgent(SCRIBE, "Scribe");
    fireEvent.click(screen.getByRole("switch", { name: "Show archived agents" }));
    await rowMenu(TRIAGE, "Triage", "Archive");
    fireEvent.click(
      within(await screen.findByTestId("agent-archive-confirm")).getByRole("button", {
        name: "Archive",
      }),
    );
    await waitFor(() =>
      expect(within(rowOf(TRIAGE)).getByRole("button", { name: "Unarchive Triage" })).toBeTruthy(),
    );
    fireEvent.click(within(rowOf(TRIAGE)).getByRole("button", { name: "Unarchive Triage" }));
    await waitFor(() =>
      expect(within(rowOf(TRIAGE)).getByRole("button", { name: "Launch Triage" })).toBeTruthy(),
    );
    expect(screen.queryByTestId("agents-notice")).toBeNull();
  });

  it("archives a team agent anyone created, and it drops out of the list", async () => {
    await loaded();
    await rowMenu(SCRIBE, "Scribe", "Archive");
    fireEvent.click(
      within(await screen.findByTestId("agent-archive-confirm")).getByRole("button", {
        name: "Archive",
      }),
    );
    await waitFor(() => expect(rowKeys()).not.toContain(SCRIBE));
  });

  it("asks before archiving an agent's routines with it, and offers them back on unarchive", async () => {
    await loaded();
    fireEvent.click(screen.getByRole("switch", { name: "Show archived agents" }));
    await rowMenu(SCRIBE, "Scribe", "Archive");
    // The confirm names both routines; cancelling changes nothing.
    const confirm = await screen.findByTestId("agent-archive-confirm");
    // Design §4.10: the confirm is titled with the count and lists the routines by name.
    expect(confirm.textContent).toContain("Archive Scribe and 2 routines?");
    expect(
      within(confirm)
        .getAllByRole("listitem")
        .map((li) => li.textContent),
    ).toEqual(["my-inbox-digest", "standup-notes"]);
    expect(confirm.textContent).toContain(
      "They stop running. Restoring Scribe offers to bring them back.",
    );
    fireEvent.click(within(confirm).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByTestId("agent-archive-confirm")).toBeNull();
    expect(within(rowOf(SCRIBE)).queryByRole("button", { name: "Unarchive Scribe" })).toBeNull();

    await rowMenu(SCRIBE, "Scribe", "Archive");
    fireEvent.click(
      within(await screen.findByTestId("agent-archive-confirm")).getByRole("button", {
        name: "Archive",
      }),
    );
    await waitFor(() =>
      expect(within(rowOf(SCRIBE)).getByRole("button", { name: "Unarchive Scribe" })).toBeTruthy(),
    );
    await openAgent(SCRIBE, "Scribe");
    expect(screen.getByTestId("agent-routines-link").textContent).toContain(
      "archived · 2 routines archived",
    );

    fireEvent.click(within(rowOf(SCRIBE)).getByRole("button", { name: "Unarchive Scribe" }));
    const offer = await within(rowOf(SCRIBE)).findByTestId("agent-restore-routines");
    expect(offer.textContent).toContain(
      "Scribe is back. 2 of Scribe's routines are still archived.",
    );
    fireEvent.click(within(offer).getByRole("button", { name: "Restore routines" }));
    await waitFor(() =>
      expect(within(rowOf(SCRIBE)).queryByTestId("agent-restore-routines")).toBeNull(),
    );
    await waitFor(() =>
      expect(screen.getByTestId("agent-routines-link").textContent).toContain("2 routines"),
    );
  });

  it("copies an agent into Mine and selects the copy", async () => {
    await loaded();
    await rowMenu(TRIAGE, "Triage", "Duplicate");
    await waitFor(() => expect(rowKeys()).toContain("_personal/1/personal/triage-copy"));
    const mine = screen.getByRole("region", { name: "Mine" });
    expect(
      within(mine)
        .getAllByTestId("agent-row")
        .map((r) => r.dataset.agentKey),
    ).toContain("_personal/1/personal/triage-copy");
    expect(rowOf("_personal/1/personal/triage-copy").dataset.selected).toBe("true");
  });

  it("hides built-in templates from the Agents list — they're reached through New agent", async () => {
    await loaded();
    // No Built-in region: templates are not peer agents, they live behind the New agent dialog.
    expect(screen.queryByRole("region", { name: "Built-in" })).toBeNull();
    // And none of the built-in keys appear as rows in the main list.
    for (const key of [
      "builtin/claude",
      "builtin/claude-unattended",
      "builtin/codex",
      "builtin/codex-unattended",
    ]) {
      expect(rowKeys()).not.toContain(key);
    }
    // No Controller starter exists any more.
    expect(rowKeys().some((k) => k?.endsWith("/controller"))).toBe(false);
    // The New agent button is present; clicking it would open the template picker.
    expect(screen.getByRole("button", { name: "New agent" })).toBeTruthy();
  });

  // Note: the earlier variant of this test launched a starter via its row (openClassicLaunch
  // on `builtin/codex`). Templates no longer have rows in the Agents list — a one-off launch
  // goes through the launch sheet's "Templates" group. That flow is covered in the launch
  // sheet seam tests; the dispatch wiring itself (storyId / agent / prompt → open_workspace)
  // is unchanged.

  it("confirms a delete with the hidden-routines warning and lists the routines refusing it", async () => {
    await loaded();
    await rowMenu(SCRIBE, "Scribe", "Delete");
    const confirm = await screen.findByTestId("agent-delete-confirm");
    expect(confirm.textContent).toContain(DELETE_WARNING);
    expect(screen.queryByRole("menuitem", { name: "Delete Reviewer" })).toBeNull();
    fireEvent.click(within(confirm).getByRole("button", { name: "Delete" }));
    await waitFor(() =>
      expect(within(rowOf(SCRIBE)).getByTestId("agent-in-use").textContent).toBe(
        "Scribe has 2 active routines. Archive Scribe instead, or turn those routines off first.",
      ),
    );

    // Refusal text may arrive before the modal closes and restores background accessibility.
    await waitFor(() => expect(screen.queryByTestId("agent-delete-confirm")).toBeNull());

    // An unused agent goes.
    await rowMenu("acme/backend/reviewer", "Reviewer", "Delete");
    const reviewerConfirm = await screen.findByRole("dialog", { name: "Delete Reviewer?" });
    fireEvent.click(within(reviewerConfirm).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(rowKeys()).not.toContain("acme/backend/reviewer"));
    expect(rowKeys()).toContain(SCRIBE);
  });

  it("drops a refused delete's routines once routines change", async () => {
    await loaded();
    await rowMenu(SCRIBE, "Scribe", "Delete");
    fireEvent.click(
      within(await screen.findByTestId("agent-delete-confirm")).getByRole("button", {
        name: "Delete",
      }),
    );
    await waitFor(() => expect(within(rowOf(SCRIBE)).getByTestId("agent-in-use")).toBeTruthy());
    act(() => events.emit(ROUTINES_CHANGED_EVENT, { key: "acme/backend/standup-notes" }));
    await waitFor(() => expect(within(rowOf(SCRIBE)).queryByTestId("agent-in-use")).toBeNull());
  });

  it("deletes against the revision the user confirmed, never a newer one", async () => {
    await loaded();
    const reviewer = "acme/backend/reviewer";
    await rowMenu(reviewer, "Reviewer", "Delete");
    const confirm = await screen.findByTestId("agent-delete-confirm");
    // Someone else changes it while the confirm is open: the list re-reads revision 3.
    await act(async () => {
      await world.handlers.agent_archive({ key: reviewer });
      await world.handlers.agent_unarchive({ key: reviewer, baseRevision: 2 });
    });
    fireEvent.click(within(confirm).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(screen.getByTestId("agents-notice")).toBeTruthy());
    // Notices float over the tab like toasts, never stacked into its layout.
    expect(screen.getByTestId("agents-notices")).toHaveClass("absolute");
    expect(rowKeys()).toContain(reviewer);
  });

  it("opens from the title bar's identity menu", async () => {
    await loaded();
    fireEvent.click(screen.getByTestId("identity-menu-trigger"));
    fireEvent.click(await screen.findByTestId("identity-menu-agents"));
    expect(openTabs).toContain("virtual://agents");
  });
});

it.each(["header", "session chip"])(
  "Discard from the same role's %s entry reverts the draft and keeps later edits guarded",
  async (entry) => {
    useRoleConfigsStore.setState({
      byTeam: {
        "acme:backend": [{ id: "reviewer", name: "Reviewer", icon: null, defaultHandler: null }],
      },
    });
    await loaded();
    fireEvent.mouseDown(screen.getByRole("tab", { name: "Roles" }));
    const name = await screen.findByRole("textbox", { name: /^Name/ });
    fireEvent.change(name, { target: { value: "Discard this name" } });
    if (entry === "header") fireEvent.click(screen.getByRole("tab", { name: "Roles" }));
    else await act(async () => openRoleEditor("acme:backend", "reviewer"));
    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Discard" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: /^Name/ })).toHaveValue("Reviewer"),
    );
    expect(useRoleEditorStore.getState().dirty).toBe(false);
    expect(screen.getByRole("button", { name: "Revert" })).not.toBeEnabled();
    fireEvent.change(screen.getByRole("textbox", { name: /^Name/ }), {
      target: { value: "Keep this name" },
    });
    fireEvent.mouseDown(screen.getByRole("tab", { name: "Agents" }));
    fireEvent.click(
      within(await screen.findByRole("dialog")).getByRole("button", { name: "Cancel" }),
    );
    expect(screen.getByRole("textbox", { name: /^Name/ })).toHaveValue("Keep this name");
    expect(useRoleEditorStore.getState().dirty).toBe(true);
  },
);

it("updates the saved-agent row from live spend without reloading the list", async () => {
  await loaded();
  const load = vi.spyOn(useAgentsStore.getState(), "loadList");
  const cap = {
    today: 40,
    week: 80,
    cap: { perDay: 50, perWeek: 200 },
    state: "warn" as const,
    pending: false,
    requestedCap: { perDay: 50, perWeek: 200 },
    unpricedModels: [],
  };
  await openAgent(TRIAGE, "Triage");
  act(() => useAgentListProjectionStore.setState({ spend: { [`agent:${TRIAGE}`]: cap } }));
  const spend = within(screen.getByTestId("agent-fact-spend"));
  expect(spend.getByTestId("spend-cap-chip").textContent).toBe("$40 of $50 today");
  // The value is plain text beside the meter, not a pill.
  expect(spend.getByTestId("spend-cap-chip").className).not.toMatch(/rounded|\bbg-/);
  expect(spend.getByRole("meter", { name: "Spend today" })).toHaveAttribute("aria-valuenow", "40");
  act(() =>
    useAgentListProjectionStore.setState({
      spend: { [`agent:${TRIAGE}`]: { ...cap, today: 50, state: "reached" } },
    }),
  );
  expect(within(screen.getByTestId("agent-fact-spend")).getByRole("status").textContent).toBe(
    "Cap reached · $50 today",
  );
  expect(load).not.toHaveBeenCalled();
  load.mockRestore();
});

describe("an Agents page row names its CLI honestly", () => {
  const STARTERS = (
    createMockAgents(new MockEventBus()).handlers.agents_list({}) as AgentRow[]
  ).filter((row) => row.key === `builtin/${row.runtime}`);

  it.each(STARTERS)(
    "$runtime: Preview after the CLI, and an endpoint it lacks",
    async (starter) => {
      const { runtime, name } = starter;
      await loaded();
      fireEvent.click(screen.getByRole("button", { name: "New agent" }));
      const picker = await screen.findByTestId("new-agent-dialog");
      const template = within(picker)
        .getAllByTestId("new-agent-template")
        .find((row) => row.dataset.templateKey === `builtin/${runtime}`)!;
      // Inside the template option the badge takes no focus; the option carries its note.
      const templateBadge = within(template).queryByTestId("preview-badge");
      expect(templateBadge !== null).toBe(registeredCli(runtime)!.preview);
      if (templateBadge) {
        expect(templateBadge.tagName).toBe("SPAN");
        expect(template).toHaveAccessibleDescription(expect.stringContaining(PREVIEW_RUNTIME_NOTE));
      }
      fireEvent.click(template);
      fireEvent.click(within(picker).getByRole("button", { name: `Use ${name}` }));
      const key = `_personal/1/personal/${runtime}-copy`;
      await waitFor(() => expect(useAgentsStore.getState().selectedKey).toBe(key));
      await waitFor(() => expect(rowOf(key)).toBeTruthy());
      const copy = useAgentsStore.getState().rows!.find((row) => row.key === key)!;
      const cli = registeredCli(runtime)!;
      const label = agentCliLabel(runtime);
      const row = within(rowOf(key));
      const open = row.getByRole("button", { name: `Open ${name}` });
      const badge = row.queryByTestId("preview-badge");
      expect(badge !== null).toBe(cli.preview);
      if (cli.preview)
        expect(open).toHaveAccessibleDescription(expect.stringContaining(PREVIEW_RUNTIME_NOTE));
      if (cli.modelEndpoint && copy.model === null) {
        const line = row.getByTestId("agent-account-warning");
        expect(line).toHaveTextContent(`${label} · Needs a model endpoint`);
        expect(line).not.toHaveTextContent("CLI default");
        // The reason first, the badge last.
        if (badge) expect(line.lastElementChild).toBe(badge);
        expect(row.getByTestId("agent-state")).toHaveTextContent("Can't launch");
        expect(row.queryByRole("button", { name: `Launch ${name}` })).toBeNull();
        // The agent's own header says it too, in place of "CLI default".
        const runsOn = await screen.findByTestId("agent-strip-runs-on");
        expect(runsOn).toHaveTextContent("Needs a model endpoint");
        expect(runsOn).toHaveTextContent("This Mac · no endpoint set");
        expect(runsOn).not.toHaveTextContent("CLI default");
        // Its health and its Fix name the endpoint, not the account.
        expect(screen.getByTestId("agent-strip-health")).toHaveTextContent(
          "Set a model endpoint first",
        );
        expect(screen.getByTestId("agent-detail-fix")).toHaveAccessibleName(
          `Fix the model endpoint for ${name}`,
        );
        fireEvent.click(row.getByRole("button", { name: `Fix the model endpoint for ${name}` }));
        await waitFor(() => expect(useAgentsStore.getState().view).toBe("edit"));
        expect(await screen.findByTestId("agent-section-runs-on")).toBeInTheDocument();
      } else {
        const line = row.getByTestId("agent-row-line");
        expect(line.textContent!.startsWith(label)).toBe(true);
        // The badge follows the CLI word.
        if (badge) expect(badge.previousElementSibling).toHaveTextContent(new RegExp(`^${label}$`));
        expect(row.queryByTestId("agent-state")).not.toHaveTextContent("Can't launch");
        expect(await screen.findByTestId("agent-strip-runs-on")).not.toHaveTextContent(
          "Needs a model endpoint",
        );
        act(() => useAgentsStore.getState().setView("edit"));
      }
      // The editor's Runtime select labels a Preview CLI on its trigger and in its options.
      const select = await screen.findByRole("combobox", { name: "Runtime" });
      expect(select.querySelector('[data-testid="preview-badge"]') !== null).toBe(cli.preview);
      fireEvent.click(select);
      for (const option of await screen.findAllByRole("option")) {
        const value = option.getAttribute("data-value");
        if (!value || !registeredCli(value)) continue;
        expect(option.querySelector('[data-testid="preview-badge"]') !== null).toBe(
          registeredCli(value)!.preview,
        );
      }
    },
  );
});
