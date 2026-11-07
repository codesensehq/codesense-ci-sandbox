// SPDX-License-Identifier: MIT
/**
 * The "Agent CLIs" settings row — the resolved agent binary per CLI, where it came
 * from, other installs found and not used, and a Re-check.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, render, screen, fireEvent, waitFor, within } from "@/test-utils";
import { PREVIEW_RUNTIME_NOTE, REGISTERED_AGENT_CLIS } from "@/shared/lib/agent-runtimes";

import type { DependencyStatus, ShadowedAgentInstall } from "@/tauri/commands";

const h = vi.hoisted(() => ({
  getDependencyStatus: vi.fn(),
  recheckDependencies: vi.fn(),
  listen: vi.fn(),
}));

vi.mock("next-themes", () => ({
  useTheme: () => ({ theme: "system", setTheme: vi.fn() }),
}));
vi.mock("@/tauri/commands", () => ({
  detectTools: vi.fn().mockResolvedValue({ editors: [], terminals: [] }),
  checkForUpdate: vi.fn().mockResolvedValue(null),
  terminalShellInfo: vi.fn().mockResolvedValue(null),
  setTerminalShellOverride: vi.fn(),
  getDependencyStatus: h.getDependencyStatus,
  recheckDependencies: h.recheckDependencies,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: h.listen }));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: vi.fn().mockResolvedValue("0.0.0-test"),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));

import { GeneralSection } from "./general-section";

function status(
  shadowed: ShadowedAgentInstall[],
  range: { qualified: boolean | null; supported: string | null } = {
    qualified: null,
    supported: null,
  },
): DependencyStatus {
  return {
    git: { available: true, path: "/usr/bin/git", version: "2.50.0" },
    agents: {
      claude: {
        available: true,
        path: "/Users/me/.local/bin/claude",
        resolving: false,
        version: "2.1.296",
        source: "loginPath",
        shadowed,
        ...range,
      },
    },
  };
}

describe("GeneralSection — Agent CLIs row", () => {
  beforeEach(() => {
    h.getDependencyStatus.mockReset();
    h.recheckDependencies.mockReset();
    h.listen.mockReset().mockResolvedValue(() => {});
  });

  it("renders dependency rows in the canonical registry order", async () => {
    const dependency = status([]);
    const row = dependency.agents.claude;
    dependency.agents = { hermes: row, opencode: row, codex: row, claude: row };
    h.getDependencyStatus.mockResolvedValue(dependency);
    render(<GeneralSection query="agent cli" />);
    await screen.findByTestId("settings-agent-cli-hermes");
    const entries = screen
      .getByTestId("settings-agent-clis")
      .querySelectorAll('[data-testid^="settings-agent-cli-"]');
    expect(
      [...entries]
        .filter((entry) =>
          /^settings-agent-cli-(claude|codex|opencode|hermes)$/.test(
            entry.getAttribute("data-testid") ?? "",
          ),
        )
        .map((entry) => entry.getAttribute("data-testid")),
    ).toEqual([
      "settings-agent-cli-claude",
      "settings-agent-cli-codex",
      "settings-agent-cli-opencode",
      "settings-agent-cli-hermes",
    ]);
    // The Preview label follows the registry's fact, for every CLI it lists.
    for (const cli of REGISTERED_AGENT_CLIS) {
      const entry = screen.getByTestId(`settings-agent-cli-${cli.binary}`);
      expect(within(entry).queryByText("Preview") !== null, cli.runtime).toBe(cli.preview);
      // Standing alone, it is a button the keyboard reaches, named by its word and described
      // by its note.
      if (cli.preview) {
        const badge = within(entry).getByRole("button", { name: "Preview" });
        expect(badge).toHaveAccessibleDescription(PREVIEW_RUNTIME_NOTE);
      }
    }
  });

  it("shows Hermes missing-install guidance and its supported-version warning", async () => {
    const dependency = status([]);
    dependency.agents = {
      hermes: {
        available: false,
        path: null,
        resolving: false,
        version: null,
        source: null,
        shadowed: [],
        qualified: null,
        supported: "Hermes Agent 0.22.5",
      },
    };
    h.getDependencyStatus.mockResolvedValue(dependency);
    const first = render(<GeneralSection query="hermes" />);
    expect((await screen.findByTestId("settings-agent-cli-hermes")).textContent).toContain(
      "Setting up agents → Hermes (preview)",
    );
    expect(
      within(screen.getByTestId("settings-agent-cli-hermes")).getByRole("button", {
        name: "Preview",
      }),
    ).toHaveAccessibleDescription(PREVIEW_RUNTIME_NOTE);
    expect(screen.getByTestId("settings-agent-cli-hermes")).toHaveTextContent("Preview");
    first.unmount();
    dependency.agents.hermes = {
      ...dependency.agents.hermes,
      available: true,
      path: "/bin/hermes",
      version: "0.22.6",
      qualified: false,
    };
    h.getDependencyStatus.mockResolvedValue(dependency);
    render(<GeneralSection query="hermes" />);
    expect(
      (await screen.findByTestId("settings-agent-cli-hermes-version-note")).textContent,
    ).toContain("supported: Hermes Agent 0.22.5");
  });

  it("shows the resolved path, version, source and the shadowed installs", async () => {
    h.getDependencyStatus.mockResolvedValue(
      status([
        { path: "/opt/homebrew/bin/claude", version: "2.1.141", onPath: true },
        { path: "/Users/me/.bun/bin/claude", version: null, onPath: false },
      ]),
    );
    render(<GeneralSection query="agent cli" />);
    const entry = await screen.findByTestId("settings-agent-cli-claude");
    expect(entry.textContent).toContain("/Users/me/.local/bin/claude");
    expect(entry.textContent).toContain("2.1.296");
    expect(screen.getByTestId("settings-agent-cli-claude-source").textContent).toBe(
      "from your shell PATH",
    );
    const warning = screen.getByTestId("settings-agent-cli-claude-shadowed");
    expect(warning.textContent).toContain("Other installs found and not used:");
    expect(warning.textContent).toContain("/opt/homebrew/bin/claude (2.1.141)");
    expect(warning.textContent).not.toContain("/opt/homebrew/bin/claude (2.1.141) (not on PATH)");
    expect(warning.textContent).toContain(
      "/Users/me/.bun/bin/claude (unknown version) (not on PATH)",
    );
    expect(h.listen).toHaveBeenCalledWith("dep-status-changed", expect.any(Function));
  });

  it("says whether the installed version is one CodeSense is qualified on", async () => {
    const range = "Claude Code 2.1.141 or newer";
    h.getDependencyStatus.mockResolvedValue(status([], { qualified: false, supported: range }));
    const { unmount } = render(<GeneralSection query="agent cli" />);
    const note = await screen.findByTestId("settings-agent-cli-claude-version-note");
    expect(note.textContent).toBe(`Not a supported version (supported: ${range})`);
    expect(note).toHaveAttribute("role", "note");
    unmount();

    h.getDependencyStatus.mockResolvedValue(status([], { qualified: true, supported: range }));
    const second = render(<GeneralSection query="agent cli" />);
    expect((await screen.findByTestId("settings-agent-cli-claude-version-note")).textContent).toBe(
      "Supported version",
    );
    second.unmount();

    h.getDependencyStatus.mockResolvedValue(status([]));
    render(<GeneralSection query="agent cli" />);
    await screen.findByTestId("settings-agent-cli-claude");
    expect(screen.queryByTestId("settings-agent-cli-claude-version-note")).toBeNull();
  });

  it("renders no warning when nothing is shadowed", async () => {
    h.getDependencyStatus.mockResolvedValue(status([]));
    render(<GeneralSection query="claude" />);
    await screen.findByTestId("settings-agent-cli-claude");
    expect(screen.queryByTestId("settings-agent-cli-claude-shadowed")).toBeNull();
  });

  it("Re-check calls recheckDependencies once and shows its result", async () => {
    h.getDependencyStatus.mockResolvedValue(
      status([{ path: "/opt/homebrew/bin/claude", version: "2.1.141", onPath: true }]),
    );
    h.recheckDependencies.mockResolvedValue(status([]));
    render(<GeneralSection query="dependencies" />);
    await screen.findByTestId("settings-agent-cli-claude-shadowed");
    fireEvent.click(screen.getByTestId("settings-agent-clis-recheck"));
    await waitFor(() =>
      expect(screen.queryByTestId("settings-agent-cli-claude-shadowed")).toBeNull(),
    );
    expect(h.recheckDependencies).toHaveBeenCalledTimes(1);
  });

  it("reads after subscribing and refreshes when dependency status changes", async () => {
    let subscribed!: () => void;
    let changed!: () => void;
    h.listen.mockImplementation((_event, callback) => {
      changed = callback;
      return new Promise<() => void>((resolve) => {
        subscribed = () => resolve(() => {});
      });
    });
    h.getDependencyStatus.mockResolvedValue(status([]));
    render(<GeneralSection query="agent cli" />);
    await waitFor(() => expect(h.listen).toHaveBeenCalled());
    expect(h.getDependencyStatus).not.toHaveBeenCalled();
    await act(async () => subscribed());
    await screen.findByTestId("settings-agent-cli-claude");
    h.getDependencyStatus.mockResolvedValue(
      status([{ path: "/other/claude", version: "2.1.141", onPath: true }]),
    );
    await act(async () => changed());
    expect(await screen.findByTestId("settings-agent-cli-claude-shadowed")).toHaveTextContent(
      "/other/claude",
    );
  });

  it("reports a failed check and clears the error after retry", async () => {
    h.getDependencyStatus.mockRejectedValue(new Error("offline"));
    h.recheckDependencies.mockResolvedValue(status([]));
    render(<GeneralSection query="agent cli" />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not check agent CLIs");
    fireEvent.click(screen.getByTestId("settings-agent-clis-recheck"));
    await screen.findByTestId("settings-agent-cli-claude");
    expect(screen.queryByRole("alert")).toBeNull();
    h.recheckDependencies.mockRejectedValue(new Error("offline"));
    await waitFor(() =>
      expect(screen.getByTestId("settings-agent-clis-recheck")).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByTestId("settings-agent-clis-recheck"));
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not check agent CLIs");
    expect(screen.getByTestId("settings-agent-cli-claude")).toBeInTheDocument();
  });

  it("ignores an older snapshot that finishes after Re-check", async () => {
    let finishOld!: (value: DependencyStatus) => void;
    h.getDependencyStatus.mockReturnValue(
      new Promise<DependencyStatus>((resolve) => {
        finishOld = resolve;
      }),
    );
    h.recheckDependencies.mockResolvedValue(status([]));
    render(<GeneralSection query="agent cli" />);
    await waitFor(() => expect(h.getDependencyStatus).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByTestId("settings-agent-clis-recheck"));
    await screen.findByTestId("settings-agent-cli-claude");
    await act(async () =>
      finishOld(status([{ path: "/old/claude", version: "2.1.141", onPath: true }])),
    );
    expect(screen.queryByTestId("settings-agent-cli-claude-shadowed")).toBeNull();
  });
});

describe("GeneralSection — opened at the Agent CLIs row", () => {
  it("scrolls the row into view and focuses it, once", async () => {
    const { useUiStore } = await import("@/shared/stores/ui-store");
    h.getDependencyStatus.mockResolvedValue(status([]));
    h.listen.mockResolvedValue(() => {});
    const scrolled = vi.fn();
    Element.prototype.scrollIntoView = scrolled;
    useUiStore.setState({ settingsInitialRow: "agentClis" });
    render(<GeneralSection query="" />);
    const row = (await screen.findByTestId("settings-agent-clis")).closest(
      '[data-setting-row="agentClis"]',
    ) as HTMLElement;
    await waitFor(() => expect(row).toHaveFocus());
    expect(scrolled).toHaveBeenCalled();
    expect(useUiStore.getState().settingsInitialRow).toBeNull();
  });
});
