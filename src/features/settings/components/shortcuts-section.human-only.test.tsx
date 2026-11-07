// SPDX-License-Identifier: MIT
import { publishPolicyFixture } from "@/test/prefs-policy-fixture";
import { applyFieldPatch } from "@/shared/lib/field-patch";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { setDynamicShortcutActions, type ShortcutAction } from "@/shared/lib/shortcuts/registry";
import { useShortcutsStore } from "@/shared/stores/shortcuts-store";

import { ShortcutsSection } from "./shortcuts-section";

const { appState } = vi.hoisted(() => ({
  appState: { value: "{}", hold: undefined as Promise<void> | undefined },
}));
vi.mock("@/tauri/commands", () => ({
  loadAppState: () => Promise.resolve(appState.value),
  saveAppState: async (state: string) => {
    await appState.hold;
    appState.value = JSON.stringify(applyFieldPatch(JSON.parse(appState.value), JSON.parse(state)));
    publishPolicyFixture(appState.value);
    return Promise.resolve();
  },
  rebuildMenu: () => Promise.resolve(),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn() },
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

const INBOX = "dashboard:private:acme/inbox";
const ARCHIVE = `${INBOX}/archive`;
const DELETE = `${INBOX}/delete`;

function command(id: string, label: string, humanOnly: boolean): ShortcutAction {
  return {
    id: `${INBOX}/${id}`,
    label,
    defaultKeystroke: "",
    scope: "global",
    command: {
      dashboard: INBOX,
      dashboardTitle: "Inbox",
      dashboardScope: "private",
      humanOnly,
      active: true,
    },
    run: () => {},
  };
}

function policyWritten() {
  return JSON.parse(appState.value).dashboardPolicy?.humanOnly;
}
const rowSwitch = (id: string) => screen.getByTestId(`shortcut-row-${id}-human-only`);

async function mount(policy: object = {}) {
  appState.value = JSON.stringify({ dashboardPolicy: policy });
  publishPolicyFixture(appState.value);
  render(<ShortcutsSection query="" />);
  // The policy loads after the first paint.
  await act(async () => {});
}

beforeEach(() => {
  appState.hold = undefined;
  useShortcutsStore.setState({ shortcuts: {}, stateLoaded: true });
  setDynamicShortcutActions([
    command("archive", "Archive thread", false),
    command("delete", "Delete thread", true),
  ]);
});
afterEach(() => setDynamicShortcutActions([]));

describe("Humans only switches", () => {
  it("a manifest suggestion starts on and unlocked; the person can switch it off, and Reset restores it", async () => {
    await mount();
    expect(rowSwitch(DELETE)).toHaveAttribute("data-state", "checked");
    expect(rowSwitch(DELETE)).not.toBeDisabled();
    expect(screen.getByTestId(`shortcut-row-${DELETE}-human-only-note`)).toHaveTextContent(
      "Inbox suggests humans only.",
    );
    expect(rowSwitch(ARCHIVE)).toHaveAttribute("data-state", "unchecked");

    fireEvent.click(rowSwitch(DELETE));
    await waitFor(() => expect(rowSwitch(DELETE)).toHaveAttribute("data-state", "unchecked"));
    expect(screen.queryByTestId(`shortcut-row-${DELETE}-human-only-note`)).toBeNull();
    await waitFor(() => expect(policyWritten()).toEqual({ commands: { [DELETE]: false } }));

    fireEvent.click(screen.getByTestId(`shortcut-row-${DELETE}-reset`));
    await waitFor(() => expect(rowSwitch(DELETE)).toHaveAttribute("data-state", "checked"));
    await waitFor(() => expect(policyWritten()).toEqual({ commands: {} }));
  });

  it("the dashboard's switch turns every command on and locks them", async () => {
    await mount();
    fireEvent.click(screen.getByTestId(`shortcuts-dashboard-${INBOX}-human-only`));
    await waitFor(() => expect(policyWritten()).toEqual({ dashboards: { [INBOX]: true } }));
    for (const id of [ARCHIVE, DELETE]) {
      expect(rowSwitch(id)).toHaveAttribute("data-state", "checked");
      expect(rowSwitch(id)).toBeDisabled();
    }
    expect(rowSwitch(ARCHIVE)).toHaveAccessibleName(
      "Humans only: Archive thread, on for all Inbox commands",
    );
    expect(screen.getByTestId(`shortcuts-dashboard-${INBOX}-human-only-note`)).toHaveTextContent(
      "Agents can't run any Inbox command. You still can, from keys or",
    );
    await waitFor(() => expect(policyWritten()).toEqual({ dashboards: { [INBOX]: true } }));
  });

  it("team-safe locks a private dashboard's commands on, over the person's switches", async () => {
    await mount({ preset: "team-safe", humanOnly: { commands: { [DELETE]: false } } });
    expect(screen.getByTestId(`shortcuts-dashboard-${INBOX}-human-only`)).toBeDisabled();
    expect(screen.getByTestId(`shortcuts-dashboard-${INBOX}-human-only`)).toHaveAttribute(
      "data-state",
      "checked",
    );
    for (const id of [ARCHIVE, DELETE]) {
      expect(rowSwitch(id)).toHaveAttribute("data-state", "checked");
      expect(rowSwitch(id)).toBeDisabled();
    }
    expect(rowSwitch(DELETE)).toHaveAccessibleName(
      "Humans only: Delete thread, set by team-safe sharing",
    );
    expect(screen.getByTestId(`shortcuts-dashboard-${INBOX}-human-only-note`)).toHaveTextContent(
      "Humans only while sharing is team-safe. This is a private dashboard.",
    );
  });
});

it("keeps accepted Humans-only toggles visible while their writes wait", async () => {
  await mount();
  let finish!: () => void;
  appState.hold = new Promise<void>((resolve) => {
    finish = resolve;
  });
  try {
    fireEvent.click(rowSwitch(DELETE));
    expect(rowSwitch(DELETE)).toHaveAttribute("data-state", "unchecked");
    fireEvent.click(rowSwitch(DELETE));
    expect(rowSwitch(DELETE)).toHaveAttribute("data-state", "checked");
  } finally {
    finish();
    await waitFor(() => expect(policyWritten()?.commands?.[DELETE]).toBe(true));
  }
});
