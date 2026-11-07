// SPDX-License-Identifier: MIT
import { pickOption } from "./pick-option";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { createMockAgents, type MockAgents } from "@/app/mock/handlers/agents";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";
import type { Org } from "@/shared/types/domain";

import { resetAgentsStoreForTests, useAgentsStore } from "../stores/agents-store";
import { AgentsTab } from "./agents-tab";

/**
 * SW-XS-16 — the agent editor's Context section, driven through the app's own door
 * (`window.__TAURI_INTERNALS__`) over the mock agents backend: the want set comes from
 * `agent_context_preview`, every edit is one `agent_context_update` against the revision
 * shown, and the one-time `projections.yml` suggestion rides on that same view.
 */

vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab: () => {} }));

const SCRIBE = "acme/backend/scribe";
const TRIAGE = "acme/backend/triage";

let world: MockAgents;
let calls: Array<{ cmd: string; args: Record<string, unknown> }>;

function mount(options: Parameters<typeof createMockAgents>[1] = {}) {
  const events = new MockEventBus();
  world = createMockAgents(events, options);
  calls = [];
  const router = createInvokeRouter({ ...events.handlers(), ...world.handlers });
  const internals = buildTauriInternals((cmd: string, args: Record<string, unknown> = {}) => {
    calls.push({ cmd, args });
    return router(cmd, args);
  });
  events.bind(internals.runCallback);
  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = internals;
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  render(
    <TooltipProvider>
      <AgentsTab filePath="virtual://agents" teamContentPath="/content/acme/backend" />
    </TooltipProvider>,
  );
}

beforeEach(() => {
  resetAgentsStoreForTests();
  useLocalProfileStore.setState({
    status: { active: { principal: "1", displayName: "Alice" } },
  } as unknown as Parameters<typeof useLocalProfileStore.setState>[0]);
  useOrgStore.setState({
    activeOrgSlug: "acme",
    activeTeamName: "backend",
    orgs: [
      {
        slug: "acme",
        teams: [
          {
            name: "backend",
            source_repos: [
              { owner: "acme", repo: "widgets", host: "github.com" },
              { owner: "acme", repo: "api", host: "github.com" },
            ],
          },
        ],
      } as unknown as Org,
    ],
  });
});

afterEach(() => {
  cleanup();
  resetAgentsStoreForTests();
});

const rowOf = (key: string) =>
  screen.getAllByTestId("agent-row").find((r) => r.dataset.agentKey === key)!;

/** Open `key`'s Edit view and wait for its Context section to resolve. */
async function contextOf(key: string, name: string, options?: Parameters<typeof mount>[0]) {
  mount(options);
  await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
  fireEvent.click(within(rowOf(key)).getByRole("button", { name: `Open ${name}` }));
  await screen.findByTestId("agent-overview");
  fireEvent.mouseDown(screen.getByRole("tab", { name: "Edit" }));
  const editor = await screen.findByTestId("agent-editor");
  const section = await within(editor).findByRole("region", { name: "Context" });
  await within(section).findByTestId("agent-context-footer");
  return { editor, section };
}

const footer = (section: HTMLElement) =>
  within(section).getByTestId("agent-context-footer").textContent ?? "";

const entryPaths = (section: HTMLElement) =>
  within(section)
    .queryAllByTestId("agent-context-entry")
    .map((e) => e.dataset.path);

const updates = () => calls.filter((c) => c.cmd === "agent_context_update").map((c) => c.args);

const revisionOf = (key: string) => useAgentsStore.getState().details[key]?.definition.revision;

describe("Agent editor · Context", () => {
  it("AC1: an agent with no context shows the Claude default, and Customize writes it out", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe");
    const banner = within(section).getByTestId("agent-context-default");
    expect(banner.textContent).toContain("Using the default for Claude");
    expect(entryPaths(section)).toEqual([
      "CLAUDE.md",
      ".claude/commands",
      ".claude/hooks",
      ".claude/agents",
      ".claude/skills",
    ]);
    // A default row cannot be removed until the list is the agent's own.
    expect(within(section).queryByRole("button", { name: /^Remove .* from context$/ })).toBeNull();

    const before = revisionOf(SCRIBE)!;
    fireEvent.click(within(banner).getByRole("button", { name: "Customize" }));
    await waitFor(() => expect(within(section).queryByTestId("agent-context-default")).toBeNull());
    expect(updates()).toEqual([
      expect.objectContaining({ key: SCRIBE, baseRevision: before, op: "customize" }),
    ]);
    expect(entryPaths(section)).toContain(".claude/skills");
    expect(
      within(section).getByRole("button", { name: "Remove .claude/skills from context" }),
    ).toBeTruthy();
  });

  it("AC2: a missing prefix saves and shows its warning on the row", async () => {
    const { section } = await contextOf(TRIAGE, "Triage");
    const before = revisionOf(TRIAGE)!;
    fireEvent.change(within(section).getByLabelText("Context path"), {
      target: { value: "docs/nowhere" },
    });
    fireEvent.click(within(section).getByRole("button", { name: "Add to context" }));

    const row = await waitFor(() => {
      const found = within(section)
        .getAllByTestId("agent-context-entry")
        .find((e) => e.dataset.path === "docs/nowhere");
      expect(found).toBeTruthy();
      return found!;
    });
    const warning = await within(row).findByTestId("agent-context-warning");
    expect(warning.dataset.code).toBe("missing");
    expect(warning.textContent).toBe("docs/nowhere: not in team content yet");
    await waitFor(() => expect(revisionOf(TRIAGE)).toBe(before + 1));
    expect(updates().at(-1)).toMatchObject({ op: "add", path: "docs/nowhere", repo: null });
  });

  it("refuses a path the backend would refuse before any round trip", async () => {
    const { section } = await contextOf(TRIAGE, "Triage");
    fireEvent.change(within(section).getByLabelText("Context path"), {
      target: { value: "docs/../secrets" },
    });
    fireEvent.click(within(section).getByRole("button", { name: "Add to context" }));
    expect((await within(section).findByTestId("agent-context-input-error")).textContent).toMatch(
      /may not contain a `\.\.` segment/,
    );
    expect(updates()).toEqual([]);
  });

  it("AC3: a per-repo override changes the footer for that repo only", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe");
    await pickOption(within(section), "Preview in", "api");
    await waitFor(() => expect(footer(section)).toMatch(/in api$/));
    const apiBefore = footer(section);
    await pickOption(within(section), "Preview in", "widgets");
    await waitFor(() => expect(footer(section)).toMatch(/in widgets$/));
    const widgetsBefore = footer(section);
    expect(widgetsBefore).toMatch(/^Scribe gets [\d,]+ files? · .+ in widgets$/);

    const repo = within(section)
      .getAllByTestId("agent-context-repo")
      .find((r) => r.dataset.repo === "github.com/acme/widgets")!;
    fireEvent.change(within(repo).getByLabelText("Path for widgets"), {
      target: { value: "docs/runbooks" },
    });
    fireEvent.click(within(repo).getByRole("button", { name: "Add for widgets" }));
    await waitFor(() => expect(footer(section)).not.toBe(widgetsBefore));
    expect(updates().at(-1)).toMatchObject({
      op: "add",
      path: "docs/runbooks",
      repo: "github.com/acme/widgets",
      viewRepo: "github.com/acme/widgets",
    });
    expect(within(repo).getByText("docs/runbooks")).toBeTruthy();
    // The team-wide list is untouched (still the default).
    expect(within(section).getByTestId("agent-context-default")).toBeTruthy();

    await pickOption(within(section), "Preview in", "api");
    await waitFor(() => expect(footer(section)).toMatch(/in api$/));
    expect(footer(section)).toBe(apiBefore);
  });

  it("drops a repo's remove pattern, a real glob included", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe", {
      contextOverrides: {
        [SCRIBE]: [{ repo: "github.com/acme/widgets", add: [], remove: [".claude/skills/x*"] }],
      },
    });
    const repo = within(section)
      .getAllByTestId("agent-context-repo")
      .find((r) => r.dataset.repo === "github.com/acme/widgets")!;
    fireEvent.click(
      within(repo).getByRole("button", { name: "Stop leaving out .claude/skills/x* in widgets" }),
    );
    await waitFor(() =>
      expect(updates().at(-1)).toMatchObject({
        op: "drop_remove",
        path: ".claude/skills/x*",
        repo: "github.com/acme/widgets",
      }),
    );
    await waitFor(() => expect(within(repo).queryByText(".claude/skills/x*")).toBeNull());
  });

  it("AC4: Accept adds the projections.yml suggestion in one save against baseRevision", async () => {
    const suggestion = {
      source: "projections.yml" as const,
      entries: [{ path: "docs/runbooks" }, { path: "finish-codesense/02-routines", dest: "plans" }],
      fingerprint: "fp-1",
    };
    const { section } = await contextOf(SCRIBE, "Scribe", {
      contextSuggestions: { [SCRIBE]: suggestion },
    });
    const callout = await within(section).findByTestId("agent-context-suggestion");
    expect(callout.textContent).toContain("Add these to Scribe's context?");
    expect(callout.textContent).toContain("finish-codesense/02-routines");
    expect(callout.textContent).toContain("plans");
    const before = revisionOf(SCRIBE)!;
    fireEvent.click(within(callout).getByRole("button", { name: "Add these 2" }));
    await waitFor(() =>
      expect(within(section).queryByTestId("agent-context-suggestion")).toBeNull(),
    );
    expect(updates()).toEqual([
      expect.objectContaining({
        key: SCRIBE,
        baseRevision: before,
        op: "add_entries",
        entries: suggestion.entries,
        accepts: "fp-1",
      }),
    ]);
    await waitFor(() =>
      expect(entryPaths(section)).toEqual(
        expect.arrayContaining(["docs/runbooks", "finish-codesense/02-routines"]),
      ),
    );
    const remapped = within(section)
      .getAllByTestId("agent-context-entry")
      .find((e) => e.dataset.path === "finish-codesense/02-routines")!;
    expect(remapped.textContent).toContain("plans");
  });

  it("Dismiss records the suggestion's fingerprint and changes nothing", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe", {
      contextSuggestions: {
        [SCRIBE]: { source: "projections.yml", entries: [{ path: "docs" }], fingerprint: "fp-9" },
      },
    });
    const callout = await within(section).findByTestId("agent-context-suggestion");
    fireEvent.click(within(callout).getByRole("button", { name: "Dismiss" }));
    await waitFor(() =>
      expect(within(section).queryByTestId("agent-context-suggestion")).toBeNull(),
    );
    expect(calls.find((c) => c.cmd === "agent_dismiss_context_suggestion")?.args).toEqual({
      key: SCRIBE,
      fingerprint: "fp-9",
    });
    expect(updates()).toEqual([]);
    // The suggestion rides on the view: no per-revision agent_get.
    expect(calls.some((c) => c.cmd === "agent_read_agent")).toBe(false);
  });

  it("M1: a nested suggestion with its own dest is added, then never offered again", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe", {
      contextSuggestions: {
        [SCRIBE]: {
          source: "projections.yml",
          // `.claude/skills` (the default) holds these files, but lands them elsewhere.
          entries: [{ path: ".claude/skills/x", dest: "x" }],
          fingerprint: "fp-nested",
        },
      },
    });
    const callout = await within(section).findByTestId("agent-context-suggestion");
    // `.claude/skills` already delivers it at its own path: the callout says it's a copy.
    expect(callout.textContent).toContain("also delivered as x");
    fireEvent.click(within(callout).getByRole("button", { name: "Add this one" }));
    await waitFor(() =>
      expect(within(section).queryByTestId("agent-context-suggestion")).toBeNull(),
    );
    const row = within(section)
      .getAllByTestId("agent-context-entry")
      .find((e) => e.dataset.path === ".claude/skills/x")!;
    expect(row.textContent).toContain("x");
    expect(entryPaths(section)).toContain(".claude/skills");
    // A fresh read of the view offers nothing again.
    await pickOption(within(section), "Preview in", "the team workspace");
    await waitFor(() => expect(footer(section)).toMatch(/in the team workspace$/));
    expect(within(section).queryByTestId("agent-context-suggestion")).toBeNull();

    // N1: the nested remap is its own entry: removing it keeps its parent…
    fireEvent.click(
      within(section).getByRole("button", { name: "Remove .claude/skills/x from context" }),
    );
    await waitFor(() => expect(entryPaths(section)).not.toContain(".claude/skills/x"));
    expect(entryPaths(section)).toContain(".claude/skills");
    expect(within(section).queryByTestId("agent-context-edit-error")).toBeNull();
    // …and accepting dismissed it: removing the entry does not offer it again (one-time).
    expect(within(section).queryByTestId("agent-context-suggestion")).toBeNull();
  });

  it("M2: previewing the team workspace, the rows are what Customize writes", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe");
    await pickOption(within(section), "Preview in", "the team workspace");
    await waitFor(() => expect(footer(section)).toMatch(/in the team workspace$/));
    const runtimeDefault = [
      "CLAUDE.md",
      ".claude/commands",
      ".claude/hooks",
      ".claude/agents",
      ".claude/skills",
    ];
    expect(entryPaths(section)).toEqual(runtimeDefault);
    expect(within(section).getByTestId("agent-context-workspace-default").textContent).toBe(
      "In the team workspace this default delivers: .claude/commands, .claude/skills",
    );
    const banner = within(section).getByTestId("agent-context-default");
    expect(banner.textContent).toContain("writes these 5 entries into Scribe for every launch");
    fireEvent.click(within(banner).getByRole("button", { name: "Customize" }));
    await waitFor(() => expect(within(section).queryByTestId("agent-context-default")).toBeNull());
    expect(updates()).toEqual([expect.objectContaining({ op: "customize", viewRepo: null })]);
    expect(entryPaths(section)).toEqual(runtimeDefault);
    expect(within(section).queryByTestId("agent-context-workspace-default")).toBeNull();
  });

  it("a runtime-default path the team lacks reads 'nothing here yet', never as a warning", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe");
    const hooks = () =>
      within(section)
        .getAllByTestId("agent-context-entry")
        .find((e) => e.dataset.path === ".claude/hooks")!;
    expect(hooks().textContent).toContain("nothing here yet");
    expect(within(hooks()).queryByTestId("agent-context-warning")).toBeNull();
    fireEvent.click(
      within(within(section).getByTestId("agent-context-default")).getByRole("button", {
        name: "Customize",
      }),
    );
    await waitFor(() => expect(within(section).queryByTestId("agent-context-default")).toBeNull());
    // Customize still writes it, and it still reads as ordinary.
    expect(entryPaths(section)).toContain(".claude/hooks");
    expect(hooks().textContent).toContain("nothing here yet");
    expect(within(hooks()).queryByTestId("agent-context-warning")).toBeNull();
  });

  it("drops a repo's add the base already covers without leaving it out", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe", {
      contextOverrides: {
        [SCRIBE]: [
          { repo: "github.com/acme/widgets", add: [{ path: ".claude/skills/x" }], remove: [] },
        ],
      },
    });
    const repo = within(section)
      .getAllByTestId("agent-context-repo")
      .find((r) => r.dataset.repo === "github.com/acme/widgets")!;
    // The previewed repo's adds show what they resolve to.
    expect(within(repo).getByTestId("agent-context-repo-add").textContent).toMatch(/\d+ files? ·/);
    fireEvent.click(
      within(repo).getByRole("button", { name: "Stop adding .claude/skills/x in widgets" }),
    );
    await waitFor(() =>
      expect(updates().at(-1)).toMatchObject({
        op: "drop_add",
        path: ".claude/skills/x",
        repo: "github.com/acme/widgets",
      }),
    );
    await waitFor(() => expect(within(repo).queryByTestId("agent-context-repo-add")).toBeNull());
    expect(within(repo).queryByText("left out")).toBeNull();
  });

  it("a removed entry can be put back with Undo, its dest kept", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe");
    fireEvent.change(within(section).getByLabelText("Context path"), {
      target: { value: "docs/runbooks" },
    });
    fireEvent.change(within(section).getByLabelText("Deliver as (optional)"), {
      target: { value: "runbooks" },
    });
    fireEvent.click(within(section).getByRole("button", { name: "Add to context" }));
    await waitFor(() => expect(entryPaths(section)).toContain("docs/runbooks"));
    fireEvent.click(
      within(section).getByRole("button", { name: "Remove docs/runbooks from context" }),
    );
    const removed = await within(section).findByTestId("agent-context-removed");
    expect(removed.textContent).toContain("Removed docs/runbooks");
    expect(entryPaths(section)).not.toContain("docs/runbooks");
    fireEvent.click(within(removed).getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(entryPaths(section)).toContain("docs/runbooks"));
    expect(updates().at(-1)).toMatchObject({
      op: "add_entries",
      entries: [{ path: "docs/runbooks", dest: "runbooks" }],
      accepts: null,
    });
    expect(within(section).queryByTestId("agent-context-removed")).toBeNull();
  });

  it("a typed path not yet added counts as an unsaved edit", async () => {
    const { section } = await contextOf(SCRIBE, "Scribe");
    expect(useAgentsStore.getState().formDirty).toBe(false);
    fireEvent.change(within(section).getByLabelText("Context path"), {
      target: { value: "docs" },
    });
    await waitFor(() => expect(useAgentsStore.getState().formDirty).toBe(true));
  });

  it("a context edit does not make the form's own Save conflict", async () => {
    const { editor, section } = await contextOf(SCRIBE, "Scribe");
    fireEvent.change(within(editor).getByLabelText("Name"), { target: { value: "Scribe II" } });
    fireEvent.click(
      within(within(section).getByTestId("agent-context-default")).getByRole("button", {
        name: "Customize",
      }),
    );
    await waitFor(() => expect(within(section).queryByTestId("agent-context-default")).toBeNull());
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() =>
      expect(useAgentsStore.getState().details[SCRIBE]?.definition.name).toBe("Scribe II"),
    );
    expect(within(editor).queryByTestId("agent-conflict")).toBeNull();
    // The save kept the context the section wrote.
    await waitFor(() => expect(within(section).queryByTestId("agent-context-default")).toBeNull());
  });
});
