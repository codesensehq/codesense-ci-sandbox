// SPDX-License-Identifier: MIT
import { act, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { OwningScopeProvider, warmScopes } from "@/shared/lib/warm-scopes";

vi.mock("next-themes", () => ({ useTheme: () => ({ theme: "system", setTheme: vi.fn() }) }));
vi.mock("@/tauri/commands", () => ({
  detectTools: vi.fn().mockResolvedValue([]),
  revealContextRepo: vi.fn().mockResolvedValue(undefined),
  listDirectory: vi.fn().mockResolvedValue([]),
  checkForUpdate: vi.fn().mockResolvedValue(null),
}));
vi.mock("@/shared/lib/codesense-home", () => ({
  getCodesenseHome: () => "/mock/home",
  deriveOrgRoot: (p: string) => p,
}));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: vi.fn().mockResolvedValue("0.0.0-test") }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn().mockResolvedValue(undefined) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
vi.mock("@/app/update-toast", () => ({ runUpdateCheck: vi.fn() }));

import { SettingsPanel } from "./settings-panel";

afterEach(() => {
  for (const k of ["settings:a", "settings:b"]) warmScopes.forget(k);
});

describe("SettingsPanel in a warm team", () => {
  it("the search field takes focus again when its team is shown again, as a remount did", () => {
    act(() => warmScopes.activate("settings:a"));
    render(
      <OwningScopeProvider scope="settings:a">
        <SettingsPanel />
      </OwningScopeProvider>,
    );
    const search = screen.getByTestId("settings-search-input");
    expect(search).toHaveFocus();
    search.blur();
    act(() => warmScopes.activate("settings:b"));
    expect(search).not.toHaveFocus();
    act(() => warmScopes.activate("settings:a"));
    expect(search).toHaveFocus();
  });
});
