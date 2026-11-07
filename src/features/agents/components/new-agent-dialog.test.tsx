// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

import { isAgentRuntime, REGISTERED_AGENT_CLIS } from "@/shared/lib/agent-runtimes";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useOrgStore } from "@/shared/stores/org-store";
import type { AgentRow } from "@/tauri/commands";

import { resetAgentsStoreForTests, useAgentsStore } from "../stores/agents-store";
import { NewAgentDialog } from "./new-agent-dialog";

function template(key: string, name: string, runtime: AgentRow["runtime"], description: string) {
  return {
    key,
    name,
    runtime,
    description,
    scope: "builtin",
    archived: false,
  } as unknown as AgentRow;
}

const ROWS = [
  template("builtin/codex-unattended", "Codex, unattended", "codex", "Headless Codex."),
  template("builtin/codex", "Codex", "codex", "Interactive session with your Codex login."),
  template("builtin/claude", "Claude", "claude", "Interactive session in a terminal tab."),
  template("builtin/opencode", "OpenCode", "opencode", "Any provider OpenCode supports."),
  template("builtin/hermes", "Hermes", "hermes", "Hermes Agent preview."),
  template(
    "builtin/hermes-unattended",
    "Hermes (unattended)",
    "hermes",
    "Hermes unattended preview.",
  ),
  { key: "acme/api/mira", name: "Mira", scope: "team", archived: false } as unknown as AgentRow,
];

const copyToMine = vi.fn(async () => {});
const startCreate = vi.fn();
const describeToAgent = vi.fn(
  async (): Promise<{ opened: boolean; message?: string }> => ({
    opened: true,
  }),
);
const onClose = vi.fn();

function renderDialog() {
  return render(
    <TooltipProvider>
      <NewAgentDialog open onClose={onClose} />
    </TooltipProvider>,
  );
}

beforeEach(() => {
  resetAgentsStoreForTests();
  useAgentsStore.setState({
    rows: ROWS.filter((row) => row.scope !== "builtin" || isAgentRuntime(row.runtime)),
    copyToMine,
    startCreate,
    describeToAgent,
  } as never);
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "api" });
  for (const fn of [copyToMine, startCreate, describeToAgent, onClose]) fn.mockClear();
});

afterEach(() => {
  cleanup();
  resetAgentsStoreForTests();
});

const dialog = () => within(screen.getByTestId("new-agent-dialog"));

describe("NewAgentDialog", () => {
  it("lists the templates in launch order with their runtime, and the first is chosen", () => {
    renderDialog();
    const rows = dialog().getAllByTestId("new-agent-template");
    expect(rows.map((r) => r.dataset.templateKey)).toEqual([
      "builtin/claude",
      "builtin/codex",
      "builtin/opencode",
      "builtin/hermes",
      "builtin/codex-unattended",
      "builtin/hermes-unattended",
    ]);
    expect(within(rows[0]).getByTestId("agent-runtime").dataset.runtime).toBe("claude");
    expect(rows[0]).toHaveAttribute("aria-selected", "true");
    expect(rows[1]).toHaveAttribute("aria-selected", "false");
    expect(dialog().getByRole("button", { name: "Use Claude" })).toBeEnabled();
  });

  it("uses the chosen template for you: pick a row, then confirm", async () => {
    renderDialog();
    fireEvent.click(dialog().getAllByTestId("new-agent-template")[1]);
    expect(copyToMine).not.toHaveBeenCalled();
    fireEvent.click(dialog().getByRole("button", { name: "Use Codex" }));
    expect(copyToMine).toHaveBeenCalledWith("builtin/codex", undefined);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("labels Preview exactly the templates whose CLI the registry marks preview", () => {
    useAgentsStore.setState({ rows: ROWS });
    renderDialog();
    const rows = dialog().getAllByTestId("new-agent-template");
    for (const cli of REGISTERED_AGENT_CLIS) {
      const own = rows.filter((row) =>
        row.dataset.templateKey?.startsWith(`builtin/${cli.runtime}`),
      );
      expect(own.length, cli.runtime).toBeGreaterThan(0);
      for (const row of own) {
        const badge = within(row).queryByText("Preview");
        expect(badge !== null, cli.runtime).toBe(cli.preview);
        if (badge)
          expect(badge).toHaveAttribute(
            "title",
            "Some features, such as usage tracking and session resume, arrive in later releases.",
          );
      }
    }
    expect(REGISTERED_AGENT_CLIS.find((cli) => cli.runtime === "hermes")?.preview).toBe(true);
  });

  it("offers Hermes templates exactly when its runtime is listed", () => {
    renderDialog();
    const hermes = dialog()
      .getAllByTestId("new-agent-template")
      .find((row) => row.dataset.templateKey === "builtin/hermes");
    expect(hermes !== undefined).toBe(isAgentRuntime("hermes"));
    if (hermes) fireEvent.click(hermes);
    expect(dialog().queryByRole("button", { name: "Use Hermes" }) !== null).toBe(
      isAgentRuntime("hermes"),
    );
  });

  it("copies a listed Hermes template into the person's agents", async () => {
    useAgentsStore.setState({ rows: ROWS });
    renderDialog();
    fireEvent.click(
      dialog()
        .getAllByTestId("new-agent-template")
        .find((row) => row.dataset.templateKey === "builtin/hermes")!,
    );
    fireEvent.click(dialog().getByRole("button", { name: "Use Hermes" }));
    expect(copyToMine).toHaveBeenCalledWith("builtin/hermes", undefined);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("puts the agent in the active team when Team is chosen", () => {
    renderDialog();
    expect(dialog().getByRole("radio", { name: "Personal" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.click(dialog().getByTestId("new-agent-scope-team"));
    fireEvent.click(dialog().getByRole("button", { name: "Use Claude" }));
    expect(copyToMine).toHaveBeenCalledWith("builtin/claude", { org: "acme", team: "api" });
  });

  it("offers Team only when a team is open, and says why not", () => {
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    renderDialog();
    expect(dialog().getByTestId("new-agent-scope-team")).toBeDisabled();
    expect(dialog().getByText(/Select a team first to share with teammates/)).toBeTruthy();
  });

  it("starts a blank agent in the inline form", () => {
    renderDialog();
    fireEvent.click(dialog().getByTestId("new-agent-blank"));
    fireEvent.click(dialog().getByRole("button", { name: "Use blank agent" }));
    expect(startCreate).toHaveBeenCalled();
    expect(copyToMine).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
  });

  it("hands over to an authoring agent and closes, so the agent it opened is not behind the modal", async () => {
    renderDialog();
    fireEvent.click(dialog().getByRole("button", { name: "describe it to an agent" }));
    expect(describeToAgent).toHaveBeenCalled();
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("stays open and says why when the authoring agent could not be opened", async () => {
    describeToAgent.mockResolvedValueOnce({ opened: false, message: "No team is active." });
    renderDialog();
    fireEvent.click(dialog().getByRole("button", { name: "describe it to an agent" }));
    expect((await dialog().findByRole("alert")).textContent).toContain("No team is active.");
    expect(onClose).not.toHaveBeenCalled();
  });

  it("closes without making anything on Cancel", () => {
    renderDialog();
    fireEvent.click(dialog().getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalled();
    expect(copyToMine).not.toHaveBeenCalled();
    expect(startCreate).not.toHaveBeenCalled();
  });
});
