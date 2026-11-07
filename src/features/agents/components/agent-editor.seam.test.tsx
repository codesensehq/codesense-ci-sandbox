// SPDX-License-Identifier: MIT
import { isAgentRuntime } from "@/shared/lib/agent-runtimes";
import userEvent from "@testing-library/user-event";
import { readFileSync } from "node:fs";
import { useRoleConfigsStore } from "@/shared/hooks/use-role-configs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pickOption } from "./pick-option";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { createInvokeRouter } from "@/app/mock/mock-router";
import { MockEventBus } from "@/app/mock/mock-events";
import { buildTauriInternals } from "@/app/mock/tauri-internals";
import { createMockAgents, type MockAgents } from "@/app/mock/handlers/agents";
import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useOrgStore } from "@/shared/stores/org-store";

import { resetAgentsStoreForTests, useAgentsStore } from "../stores/agents-store";
import { AgentsTab } from "./agents-tab";
import { applyAgentDefinitions } from "../lib/agent-definitions";
import { liveHub, type HubSnapshot } from "@/features/dashboard/lib/live-hub";
import type { AgentDetailView } from "@/tauri/commands";

/**
 * The agent editor driven through the same door the app uses (`window.__TAURI_INTERNALS__`),
 * over the RF-29 mock agents backend: every preview, catalog listing, save and restore is a
 * real command round-trip, and every change elsewhere arrives as `agents:changed`.
 */

vi.mock("@/shared/lib/open-management-tab", () => ({ openManagementTab: () => {} }));

const TRIAGE = "acme/backend/triage";
const SCRIBE = "acme/backend/scribe";
const REVIEWER = "acme/backend/reviewer";

let world: MockAgents;

function mount(options: Parameters<typeof createMockAgents>[1] = {}) {
  const events = new MockEventBus();
  world = createMockAgents(events, options);
  const internals = buildTauriInternals(
    createInvokeRouter({ ...events.handlers(), ...world.handlers }),
  );
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
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "backend" });
});

afterEach(() => {
  cleanup();
  resetAgentsStoreForTests();
});

const rowOf = (key: string) =>
  screen.getAllByTestId("agent-row").find((r) => r.dataset.agentKey === key)!;

async function loaded(options?: Parameters<typeof createMockAgents>[1]) {
  mount(options);
  await waitFor(() => expect(screen.getAllByTestId("agent-row").length).toBeGreaterThan(0));
}

/** Open `key`'s Edit view and wait for its first preview. */
async function edit(key: string, name: string) {
  fireEvent.click(within(rowOf(key)).getByRole("button", { name: `Open ${name}` }));
  await screen.findByTestId("agent-overview");
  fireEvent.mouseDown(screen.getByRole("tab", { name: "Edit" }));
  const editor = await screen.findByTestId("agent-editor");
  await within(editor).findByTestId("agent-manifest-totals");
  return editor;
}

const totals = (scope: HTMLElement) =>
  within(scope).getByTestId("agent-manifest-totals").textContent;

const definition = (key: string) => useAgentsStore.getState().details[key]?.definition;

/** The New agent button opens a template picker dialog; "Blank" there starts the inline
 *  create form. These editor tests cover the form itself, not the picker — call the store
 *  action directly so the Radix portal doesn't need to render under happy-dom. The dialog
 *  flow is covered separately by the Agents tab seam test. */
async function clickNewAgentBlank() {
  useAgentsStore.getState().startCreate();
}

describe("Agent editor", () => {
  it.each([false, true])(
    "receives a real file edit with the same revision and preserves focus and dirty=%s",
    async (dirty) => {
      const { before, after } = JSON.parse(
        readFileSync("tests/fixtures/agent-definitions-file.json", "utf8"),
      ) as { before: HubSnapshot; after: HubSnapshot };
      const original = before.snapshot.find((row) => row.rowType === "detail")!
        .detail as unknown as AgentDetailView;
      const key = original.key;
      await loaded({
        seed: () => ({
          [key]: {
            definition: original.definition,
            scope: "team",
            created_by: "alice",
            used_by: [],
          },
        }),
      });
      const editor = await edit(key, original.definition.name);
      const input = within(editor).getByLabelText("Name");
      input.focus();
      if (dirty) fireEvent.change(input, { target: { value: "My pending name" } });
      act(() => {
        liveHub.seed(after);
        applyAgentDefinitions(after);
      });
      await waitFor(() => expect(input).toHaveValue(dirty ? "My pending name" : "External edit"));
      expect(within(editor).getByLabelText("Name")).toBe(input);
      expect(document.activeElement).toBe(input);
      expect(useAgentsStore.getState().rows!.find((row) => row.key === key)!.name).toBe(
        "External edit",
      );
    },
  );

  it("derives Hermes runtime choices from backend listing while endpoint UI stays inactive", async () => {
    await loaded();
    await clickNewAgentBlank();
    const form = await screen.findByTestId("agent-create");
    fireEvent.click(within(form).getByRole("combobox", { name: "Runtime" }));
    await screen.findByRole("option", { name: "OpenCode" });
    // A Preview CLI's option is named with its badge's word.
    expect(screen.queryByRole("option", { name: /^Hermes( Preview)?$/ }) !== null).toBe(
      isAgentRuntime("hermes"),
    );
    expect(within(form).queryByLabelText("Local endpoint")).toBeNull();
  });

  it("keeps Hermes templates out of New agent until the listing gate opens", async () => {
    await loaded();
    fireEvent.click(screen.getByRole("button", { name: "New agent" }));
    const dialog = await screen.findByTestId("new-agent-dialog");
    expect(dialog.querySelector('[data-template-key="builtin/hermes"]') !== null).toBe(
      isAgentRuntime("hermes"),
    );
    expect(dialog.querySelector('[data-template-key="builtin/hermes-unattended"]') !== null).toBe(
      isAgentRuntime("hermes"),
    );
    expect(useAgentsStore.getState().rows?.some((row) => row.runtime === "hermes")).toBe(
      isAgentRuntime("hermes"),
    );
  });

  it("creates an agent from a picked skill folder, previewing its files and what it requires", async () => {
    await loaded();
    await clickNewAgentBlank();
    const form = await screen.findByTestId("agent-create");
    // A new agent starts with only what CodeSense itself needs.
    await waitFor(() => expect(totals(form)).toBe("0 entries · 0 files · 0 B"));
    expect(within(form).getAllByTestId("agent-manifest-essential").length).toBeGreaterThan(0);
    expect(within(form).getByRole("combobox", { name: "Runtime" })).toHaveTextContent("Claude");

    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Explorer" } });
    // Who can use it: the active team by default; this agent is kept for me only.
    const who = within(form).getByRole("combobox", { name: "Who can use it" });
    expect(who).toHaveValue("acme/backend");
    fireEvent.change(who, { target: { value: "only-me" } });
    fireEvent.click(within(form).getByRole("button", { name: /Pick files\/folders/ }));
    const picker = await within(form).findByTestId("agent-startup-picker");
    fireEvent.click(await within(picker).findByRole("button", { name: "Open folder .claude" }));
    fireEvent.click(
      await within(picker).findByRole("button", { name: "Open folder .claude/skills" }),
    );
    fireEvent.click(await within(picker).findByRole("button", { name: "Add .claude/skills/x/" }));

    const item = within(form)
      .getAllByTestId("agent-startup-item")
      .find((i) => i.dataset.path === ".claude/skills/x")!;
    expect(item.textContent).toContain(".claude/skills/x/");
    await waitFor(() => expect(item.textContent).toContain("2 files"));
    const dependency = within(form)
      .getAllByTestId("agent-manifest-implied")
      .find((i) => i.dataset.path === "docs/guide.md")!;
    expect(dependency.textContent).toContain("needed by .claude/skills/x");
    expect(totals(form)).toBe("2 entries · 3 files · 5.8 KB");
    expect(within(form).getByTestId("agent-provides").textContent).toContain("/x (skill)");

    fireEvent.click(within(form).getByRole("button", { name: "Create agent" }));
    await waitFor(() =>
      expect(useAgentsStore.getState().selectedKey).toBe("_personal/1/personal/explorer"),
    );
    expect(screen.queryByTestId("agent-create")).toBeNull();
    await waitFor(() =>
      expect(definition("_personal/1/personal/explorer")?.startup).toEqual([".claude/skills/x"]),
    );
  });

  it("a New agent goes to the open team by default (Who can use it)", async () => {
    await loaded();
    await clickNewAgentBlank();
    const form = await screen.findByTestId("agent-create");
    expect(within(form).getByRole("combobox", { name: "Who can use it" })).toHaveValue(
      "acme/backend",
    );
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Helper" } });
    fireEvent.click(within(form).getByRole("button", { name: "Create agent" }));
    await waitFor(() => expect(useAgentsStore.getState().selectedKey).toBe("acme/backend/helper"));
  });

  it("with no team open, a New agent defaults only to a team of the open org — else asks", async () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: null });
    await loaded();
    await clickNewAgentBlank();
    let form = await screen.findByTestId("agent-create");
    expect(within(form).getByRole("combobox", { name: "Who can use it" })).toHaveValue(
      "acme/backend",
    );
    fireEvent.click(within(form).getByRole("button", { name: "Cancel" }));

    useOrgStore.setState({ activeOrgSlug: "other/org", activeTeamName: null });
    await clickNewAgentBlank();
    form = await screen.findByTestId("agent-create");
    expect(within(form).getByRole("combobox", { name: "Who can use it" })).toHaveValue("");
    fireEvent.change(within(form).getByLabelText("Name"), { target: { value: "Nobody" } });
    expect(within(form).getByRole("button", { name: "Create agent" })).toBeDisabled();
  });

  it("SW-AGR-19 PR-6: turns on resuming after a terminal server update, and saves it", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    const box = within(editor).getByLabelText("Resume after a terminal server update");
    expect(box).toHaveProperty("checked", false);
    fireEvent.click(box);
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(definition(TRIAGE)?.resumeAfterRestart).toBe(true));
  });

  it("keeps the edit on a stale save, reloads, shows who changed what, and restores", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    fireEvent.change(within(editor).getByLabelText("Description"), {
      target: { value: "My edit." },
    });
    // An agent changes it through its MCP tools while the edit is open.
    act(() =>
      world.editAs(
        TRIAGE,
        { description: "The bot's edit.", startup: ["CLAUDE.md", "docs/guide.md"] },
        { kind: "agent", name: "Triage bot" },
      ),
    );
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    const conflict = await within(editor).findByTestId("agent-conflict");
    expect(conflict.textContent).toContain("Changed elsewhere — reload");
    expect(conflict.textContent).toContain("revision 4");
    expect(within(editor).getByLabelText("Description")).toHaveProperty("value", "My edit.");

    fireEvent.click(within(conflict).getByRole("button", { name: "Reload" }));
    await waitFor(() =>
      expect(within(editor).getByLabelText("Description")).toHaveProperty(
        "value",
        "The bot's edit.",
      ),
    );
    expect(within(editor).queryByTestId("agent-conflict")).toBeNull();

    fireEvent.mouseDown(screen.getByRole("tab", { name: "History" }));
    const history = await screen.findByTestId("agent-history");
    const newest = within(history).getAllByTestId("agent-revision")[0];
    expect(newest.dataset.revision).toBe("4");
    expect(within(newest).getByTestId("agent-revision-author").textContent).toContain(
      "by Triage bot (agent)",
    );
    const diff = within(newest).getByTestId("agent-revision-diff").textContent ?? "";
    expect(diff).toContain("removed .claude/skills/triage-slack");
    expect(diff).toContain("added docs/guide.md");

    const first = within(history)
      .getAllByTestId("agent-revision")
      .find((r) => r.dataset.revision === "1")!;
    fireEvent.click(within(first).getByRole("button", { name: "Restore revision 1" }));
    await waitFor(() =>
      expect(within(history).getAllByTestId("agent-revision")[0].dataset.revision).toBe("5"),
    );
    const restored = within(history).getAllByTestId("agent-revision")[0];
    expect(within(restored).getByTestId("agent-revision-author").textContent).toContain("by alice");
    expect(definition(TRIAGE)?.startup).toEqual([".claude/skills/triage-slack", "CLAUDE.md"]);
  });

  it("lets anyone edit a team agent, and saves an agent whose manifest has errors", async () => {
    await loaded();
    // Scribe was created by a teammate; its standup command is in no layer.
    const editor = await edit(SCRIBE, "Scribe");
    expect(within(editor).getByTestId("agent-editor-bootstrap-note")).toBeTruthy();
    const errors = within(editor).getAllByTestId("agent-startup-error");
    expect(errors[0].dataset.code).toBe("missing");

    fireEvent.change(within(editor).getByLabelText("Startup path"), {
      target: { value: "../escape.md" },
    });
    fireEvent.click(within(editor).getByRole("button", { name: "Add path" }));
    expect(within(editor).getByTestId("agent-startup-path-error").textContent).toContain(
      "may not contain a `..` segment",
    );

    fireEvent.change(within(editor).getByLabelText("Startup path"), {
      target: { value: "docs/nowhere.md" },
    });
    fireEvent.click(within(editor).getByRole("button", { name: "Add path" }));
    await waitFor(() =>
      expect(
        within(editor)
          .getAllByTestId("agent-startup-item")
          .find((i) => i.dataset.path === "docs/nowhere.md")?.textContent,
      ).toContain("missing"),
    );
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(definition(SCRIBE)?.revision).toBe(2));
    expect(definition(SCRIBE)?.startup).toContain("docs/nowhere.md");
    expect(within(editor).queryByTestId("agent-invalid")).toBeNull();
  });

  it("edits Claude's own allow/deny rules; CodeSense's required allows stay and a blocking deny is flagged", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    const permissions = within(editor).getByTestId("agent-permissions");
    const allow = within(permissions).getByTestId("agent-rules-allow");
    expect(within(allow).getAllByTestId("agent-rule-required")).toHaveLength(3);
    expect(
      within(allow).queryByRole("button", { name: /Remove allow rule mcp__codesense/ }),
    ).toBeNull();

    fireEvent.change(within(allow).getByLabelText("New allow rule"), {
      target: { value: "Bash(npm test:*)" },
    });
    fireEvent.click(within(allow).getByRole("button", { name: "Add allow rule" }));
    const deny = within(permissions).getByTestId("agent-rules-deny");
    fireEvent.change(within(deny).getByLabelText("New deny rule"), {
      target: { value: "mcp__codesense__*" },
    });
    fireEvent.click(within(deny).getByRole("button", { name: "Add deny rule" }));
    expect(within(deny).getByTestId("agent-rule-invalid").textContent).toContain("invalid");

    await waitFor(() =>
      expect(within(permissions).getByTestId("agent-effective-settings").textContent).toContain(
        "Bash(npm test:*)",
      ),
    );
    await waitFor(() =>
      expect(
        within(editor)
          .getAllByTestId("agent-manifest-warning")
          .some((w) => w.textContent?.includes("denies codesense tools")),
      ).toBe(true),
    );
    await pickOption(within(permissions), "Default mode", "bypassPermissions");
    await waitFor(() =>
      expect(within(editor).getAllByTestId("agent-auto-approves").length).toBeGreaterThan(0),
    );

    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(definition(TRIAGE)?.revision).toBe(4));
    expect(definition(TRIAGE)?.permissions?.claude).toMatchObject({
      allow: ["Bash(gh issue:*)", "Read", "Edit", "WebFetch", "Grep", "Bash(npm test:*)"],
      deny: ["Bash(git push --force:*)", "Bash(rm -rf:*)", "mcp__codesense__*"],
      default_mode: "bypassPermissions",
    });
  });

  it("round-trips Codex's selects and keeps each runtime's block across a switch", async () => {
    await loaded();
    const editor = await edit(REVIEWER, "Reviewer");
    const permissions = within(editor).getByTestId("agent-permissions");
    expect(within(permissions).getByTestId("agent-codex-hint").textContent).toBe(
      "headless default: never · workspace-write",
    );
    await pickOption(within(permissions), "approval_policy", "never");
    await pickOption(within(permissions), "sandbox_mode", "danger-full-access");
    await waitFor(() =>
      expect(within(editor).getAllByTestId("agent-auto-approves").length).toBeGreaterThan(0),
    );

    // Over to Claude and back: the Codex values are still there.
    await pickOption(within(editor), "Runtime", "Claude");
    expect(within(editor).queryByRole("combobox", { name: "approval_policy" })).toBeNull();
    await pickOption(within(editor), "Runtime", "Codex");
    expect(within(editor).getByRole("combobox", { name: "approval_policy" })).toHaveTextContent(
      "never",
    );

    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(definition(REVIEWER)?.revision).toBe(2));
    expect(definition(REVIEWER)?.permissions?.codex).toEqual({
      approval_policy: "never",
      sandbox_mode: "danger-full-access",
    });
  });

  it("keeps the OpenCode permission input mounted while its pattern is edited", async () => {
    await loaded();
    const editor = await edit(REVIEWER, "Reviewer");
    await pickOption(within(editor), "Runtime", "OpenCode");
    fireEvent.click(within(editor).getByRole("button", { name: "Add OpenCode permission" }));
    const pattern = within(editor).getByLabelText("OpenCode tool pattern 1");
    pattern.focus();
    let value = "";
    for (const character of "bash") {
      value += character;
      fireEvent.change(pattern, { target: { value } });
      expect(document.activeElement).toBe(pattern);
    }
    expect(pattern).toHaveProperty("value", "bash");
  });

  it("pages a folder bigger than one listing and filters it, so any entry can be picked", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    fireEvent.click(within(editor).getByRole("button", { name: /Pick files\/folders/ }));
    const picker = await within(editor).findByTestId("agent-startup-picker");
    fireEvent.click(await within(picker).findByRole("button", { name: "Open folder archive" }));
    const more = await within(picker).findByTestId("agent-picker-more");
    expect(more.textContent).toContain("Showing 500 of 520");
    expect(within(picker).queryByRole("button", { name: "Add archive/note-0519.md" })).toBeNull();
    fireEvent.click(within(more).getByRole("button", { name: "Show more" }));
    await within(picker).findByRole("button", { name: "Add archive/note-0519.md" });
    expect(within(picker).queryByTestId("agent-picker-more")).toBeNull();

    fireEvent.change(within(picker).getByLabelText("Filter this folder"), {
      target: { value: "NOTE-0517" },
    });
    await waitFor(() =>
      expect(within(picker).getAllByTestId("agent-picker-entry")).toHaveLength(1),
    );
    fireEvent.click(within(picker).getByRole("button", { name: "Add archive/note-0517.md" }));
    expect(
      within(editor)
        .getAllByTestId("agent-startup-item")
        .some((i) => i.dataset.path === "archive/note-0517.md"),
    ).toBe(true);
    // Renders 520 picker rows: ~2s alone, past the 5s default under a full suite.
  }, 20_000);

  it("SW-AN-50: tells the user why the bridge is alone when nothing is checked", async () => {
    await loaded();
    // Reviewer carries no MCP servers; the team offers slack/github/linear.
    const editor = await edit(REVIEWER, "Reviewer");
    const hint = await within(editor).findByTestId("agent-offered-servers-hint");
    expect(hint.textContent).toContain("Nothing checked");
    expect(hint.textContent).toContain("the team offers: slack, github, linear");
    expect(hint.textContent).toContain("Granting MCP servers per agent");
  });

  it("offers the servers the preview's team has, and keeps quoted args as written words", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    for (const id of ["slack", "github", "linear"]) {
      await within(editor).findByLabelText(`MCP server ${id}`);
    }
    expect(within(editor).queryByLabelText("MCP server codesense")).toBeNull();
    // SW-AN-50: once some are checked, the hint names the full offered set, not the
    // bridge-only guidance; the "Granting MCP servers per agent" docs reference stays.
    const hint = await within(editor).findByTestId("agent-offered-servers-hint");
    expect(hint.textContent).toContain("The team offers: slack, github, linear");
    expect(hint.textContent).toContain("Granting MCP servers per agent");
    expect(hint.textContent).not.toContain("Nothing checked");

    fireEvent.change(within(editor).getByLabelText("Args"), {
      target: { value: `--append-system-prompt 'Be terse, it'\\''s fine' --k="a b"` },
    });
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() =>
      expect(definition(TRIAGE)?.args).toEqual([
        "--append-system-prompt",
        "Be terse, it's fine",
        "--k=a b",
        "--model",
        "opus",
      ]),
    );
    // Saved as typed: the form holds no edit left to save.
    expect(within(editor).getByRole("button", { name: "Save changes" })).toHaveProperty(
      "disabled",
      true,
    );
  });

  it("reads --model from the args and writes it back, keeping every other arg", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    const model = within(editor).getByLabelText("Model");
    expect(model).toHaveValue("opus");
    expect(within(editor).getByLabelText("Args")).toHaveValue("");
    fireEvent.change(within(editor).getByLabelText("Args"), { target: { value: "--effort high" } });
    expect(model).toHaveValue("opus");
    // Untouched, the field shows no "was"; edited, it says what it was.
    expect(within(editor).queryByTestId("agent-model-was")).toBeNull();
    fireEvent.change(model, { target: { value: "sonnet" } });
    expect(within(editor).getByTestId("agent-model-was").textContent).toBe("was opus");
    fireEvent.change(model, { target: { value: "opus" } });
    expect(within(editor).queryByTestId("agent-model-was")).toBeNull();
    fireEvent.change(model, { target: { value: "sonnet" } });
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() =>
      expect(definition(TRIAGE)?.args).toEqual(["--effort", "high", "--model", "sonnet"]),
    );
    // The list row names the model.
    await waitFor(() =>
      expect(within(rowOf(TRIAGE)).getByTestId("agent-row-line").textContent).toContain("sonnet"),
    );
    // CLI default removes the one flag and keeps the rest.
    fireEvent.change(model, { target: { value: "" } });
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(definition(TRIAGE)?.args).toEqual(["--effort", "high"]));
    await waitFor(() =>
      expect(within(rowOf(TRIAGE)).getByTestId("agent-row-line").textContent).toContain(
        "CLI default",
      ),
    );
  });

  it("types Extra args one key at a time: spaces and an unfinished quote stay as typed", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    const args = within(editor).getByLabelText("Args");
    await userEvent.type(args, "--effort high");
    expect(args).toHaveValue("--effort high");
    await userEvent.type(args, " --k='a b");
    expect(args).toHaveValue("--effort high --k='a b");
    await userEvent.type(args, "'");
    expect(args).toHaveValue("--effort high --k='a b'");
    // The Model field rides along, and a trailing space is kept while typing.
    await userEvent.type(args, " ");
    expect(args).toHaveValue("--effort high --k='a b' ");
    expect(within(editor).getByLabelText("Model")).toHaveValue("opus");
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() =>
      expect(definition(TRIAGE)?.args).toEqual(["--effort", "high", "--k=a b", "--model", "opus"]),
    );
  });

  it("jumps to a section of this editor, never to a same-named one of another team's editor", async () => {
    await loaded();
    // A retained team's editor sits earlier in the document, with the same section ids.
    const decoy = document.createElement("div");
    decoy.hidden = true;
    decoy.innerHTML = '<div id="ed-permissions">other team</div>';
    document.body.prepend(decoy);
    const jumped: Element[] = [];
    const original = Element.prototype.scrollIntoView;
    Element.prototype.scrollIntoView = function (this: Element) {
      jumped.push(this);
    };
    try {
      const editor = await edit(TRIAGE, "Triage");
      fireEvent.click(
        within(within(editor).getByTestId("agent-editor-nav")).getByRole("button", {
          name: "Permissions",
        }),
      );
      expect(jumped).toHaveLength(1);
      expect(editor.contains(jumped[0])).toBe(true);
      expect(decoy.contains(jumped[0])).toBe(false);
    } finally {
      Element.prototype.scrollIntoView = original;
      decoy.remove();
    }
  });

  it("counts unsaved changes by section in the save bar and the nav, and Discard clears them", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    const bar = within(editor).getByTestId("agent-savebar");
    expect(within(bar).getByRole("button", { name: "Save changes" })).toBeDisabled();
    fireEvent.change(within(editor).getByLabelText("Name"), { target: { value: "Renamed" } });
    fireEvent.change(within(editor).getByLabelText("Model"), { target: { value: "haiku" } });
    expect(within(bar).getByTestId("agent-savebar-msg").textContent).toContain(
      "2 unsaved changes in Basics and Runs on",
    );
    expect(
      within(within(editor).getByTestId("agent-editor-nav")).getAllByRole("img", {
        name: "Edited",
      }),
    ).toHaveLength(2);
    fireEvent.click(within(bar).getByRole("button", { name: "Discard" }));
    expect(within(editor).getByLabelText("Name")).toHaveValue("Triage");
    expect(within(editor).getByLabelText("Model")).toHaveValue("opus");
    expect(within(bar).getByRole("button", { name: "Save changes" })).toBeDisabled();
  });

  it("asks before leaving an unsaved edit, and drops it only when told", async () => {
    await loaded();
    const editor = await edit(TRIAGE, "Triage");
    fireEvent.change(within(editor).getByLabelText("Name"), { target: { value: "Renamed" } });
    await waitFor(() => expect(useAgentsStore.getState().formDirty).toBe(true));

    fireEvent.click(within(rowOf(SCRIBE)).getByRole("button", { name: "Open Scribe" }));
    const confirm = await screen.findByTestId("agent-discard-confirm");
    expect(useAgentsStore.getState().selectedKey).toBe(TRIAGE);
    fireEvent.click(within(confirm).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByTestId("agent-discard-confirm")).toBeNull());
    expect(within(editor).getByLabelText("Name")).toHaveProperty("value", "Renamed");

    await clickNewAgentBlank();
    fireEvent.click(
      within(await screen.findByTestId("agent-discard-confirm")).getByRole("button", {
        name: "Discard",
      }),
    );
    await screen.findByTestId("agent-create");
    expect(definition(TRIAGE)?.name).toBe("Triage");
  });

  it("describes an agent to an authoring agent, or says why it could not open", async () => {
    await loaded();
    await clickNewAgentBlank();
    fireEvent.click(await screen.findByRole("button", { name: "Describe it to an agent" }));
    await waitFor(() => expect(world.authoringOpened()).toBe(1));
    expect(screen.queryByTestId("agents-notice")).toBeNull();
    cleanup();
    resetAgentsStoreForTests();

    await loaded({ authoringOpenError: "No team is active." });
    await clickNewAgentBlank();
    fireEvent.click(await screen.findByRole("button", { name: "Describe it to an agent" }));
    expect((await screen.findByTestId("agents-notice")).textContent).toContain(
      "No team is active.",
    );
    expect(world.authoringOpened()).toBe(0);
  });
  it("edits and saves a team agent named controller like any other agent", async () => {
    mount();
    // A teammate saved a team agent with the id `controller`: nothing special about it.
    act(() => {
      world.handlers.agent_save({
        form: {
          name: "Controller",
          runtime: "claude",
          scope: "team",
          org: "acme",
          team: "backend",
        },
      });
    });
    const own = "acme/backend/controller";
    await waitFor(() => expect(rowOf(own)).toBeTruthy());
    // No starter stands in for it, and its row is a plain, editable team agent.
    expect(
      screen.getAllByTestId("agent-row").some((r) => r.dataset.agentKey === "builtin/controller"),
    ).toBe(false);
    const editor = await edit(own, "Controller");
    fireEvent.change(within(editor).getByLabelText("Name"), { target: { value: "Ricky" } });
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(definition(own)?.name).toBe("Ricky"));
    // The save stays on the same agent: no other agent was created or selected.
    expect(useAgentsStore.getState().selectedKey).toBe(own);
    expect(definition(own)?.revision).toBe(2);
  });
});

it("saves and clears the default queue independently of its handler setting", async () => {
  const config = { id: "reviewer", name: "Reviewer", icon: null, defaultHandler: null };
  useRoleConfigsStore.setState({ byTeam: { "acme:backend": [config] }, errors: {} });
  await loaded();
  const editor = await edit(TRIAGE, "Triage");
  await pickOption(within(editor), "Default role", "Reviewer");
  fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
  await waitFor(() => expect(definition(TRIAGE)?.role).toBe("reviewer"));
  expect(useRoleConfigsStore.getState().byTeam["acme:backend"][0].defaultHandler).toBeNull();
  await pickOption(within(editor), "Default role", "No role (joins no queue)");
  fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));
  await waitFor(() => expect(definition(TRIAGE)?.role).toBeNull());
});
