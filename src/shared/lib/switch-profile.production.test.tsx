// SPDX-License-Identifier: MIT
import { createRequire } from "node:module";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

/**
 * The packaged WebDriver build runs production React, whose `<Profiler>` never calls back.
 * This file loads the production renderer (a fresh CommonJS graph with NODE_ENV=production) and
 * proves the switch marks are still recorded there.
 */
const nodeRequire = createRequire(import.meta.url);
const REACT_PATH = /node_modules[\\/](react|react-dom|scheduler)[\\/]/;
const previousEnv = process.env.NODE_ENV;

function purgeReact(): void {
  for (const key of Object.keys(nodeRequire.cache))
    if (REACT_PATH.test(key)) delete nodeRequire.cache[key];
}

beforeAll(() => {
  purgeReact();
  process.env.NODE_ENV = "production";
  vi.resetModules();
});
afterAll(() => {
  process.env.NODE_ENV = previousEnv;
  purgeReact();
});

describe("the switch marks under production React", () => {
  it("record the commit, the settled effects and the reveal while a Profiler gets no callback", async () => {
    vi.stubEnv("MODE", "webdriver");
    vi.spyOn(console, "debug").mockImplementation(() => {});
    vi.useFakeTimers({
      toFake: ["setTimeout", "clearTimeout", "requestAnimationFrame", "performance"],
    });
    const React = nodeRequire("react") as typeof import("react");
    const { createRoot } = nodeRequire("react-dom/client") as typeof import("react-dom/client");
    const { flushSync } = nodeRequire("react-dom") as typeof import("react-dom");
    // Production React has no `act`; a Profiler that never fires is the control.
    expect((React as { act?: unknown }).act).toBeUndefined();
    const { markSwitchCommitted, startSwitchProfile, markSwitchRevealed } =
      await import("./switch-profile");
    const { useOrgStore } = await import("@/shared/stores/org-store");
    useOrgStore.setState({
      orgs: [{ slug: "acme", teams: [{ name: "a" }, { name: "b" }] }] as never,
      activeOrgSlug: "acme",
      activeTeamName: "a",
    });
    let profilerCalls = 0;
    // The hook is one layout effect around markSwitchCommitted; the module's own `react` import
    // stays the development copy, so the production component places the same effect itself.
    function Probe({ team }: { team: string }) {
      React.useLayoutEffect(() => markSwitchCommitted(), [team]);
      return React.createElement("p", null, team);
    }
    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    const stop = startSwitchProfile();
    const render = (team: string) =>
      flushSync(() =>
        root.render(
          React.createElement(
            React.Profiler,
            { id: "control", onRender: () => void (profilerCalls += 1) },
            React.createElement(Probe, { team }),
          ),
        ),
      );
    render("a");
    useOrgStore.getState().setActiveTeam("b");
    render("b");
    markSwitchRevealed();
    await vi.advanceTimersByTimeAsync(300);
    stop();
    expect(host.textContent).toBe("b");
    root.unmount();
    expect(profilerCalls).toBe(0);
    const switchEntry = window.__csDebug!.find((e) => e.kind === "switch");
    expect(switchEntry).toMatchObject({ to: "acme:b" });
    expect(switchEntry!.writeToFirstCommitMs).not.toBeNull();
    expect(switchEntry!.writeToSettledMs).not.toBeNull();
    expect(switchEntry!.writeToRevealedMs).not.toBeNull();
    vi.useRealTimers();
    vi.unstubAllEnvs();
  });
});
