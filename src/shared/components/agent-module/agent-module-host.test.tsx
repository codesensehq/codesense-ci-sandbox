// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, type Mock } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { GitPullRequestArrow } from "lucide-react";

import type { AgentWorkspaceConfig } from "@/shared/lib/run-agent-bind";

import { AgentModuleHost } from "./agent-module-host";
import { useAgentModule } from "./use-agent-module";
import type { AgentModuleConfig, AgentModuleApi } from "./agent-module-config";

// The host is about the prewarm GATE: does it mount the session hook (= engage) or
// not, given `prewarm` × `active`. Stub the hook so "mounted" == "was called".
vi.mock("./use-agent-module", () => ({ useAgentModule: vi.fn() }));

const mockApi: AgentModuleApi = {
  session: { boundSessionId: "sid-1" } as AgentModuleApi["session"],
  isMinimized: true,
  promote: vi.fn(),
  minimize: vi.fn(),
};

function makeConfig(over: Partial<AgentModuleConfig> = {}): AgentModuleConfig {
  const session: AgentWorkspaceConfig = {
    targetWorkspace: "test-agent",
    agentBasename: "claude",
    connect: vi.fn(async () => "sid-1"),
    reconnect: vi.fn(async () => ({ kind: "reattached" }) as never),
    autoConnect: true,
  };
  return { panelId: "p", title: "Agent", icon: GitPullRequestArrow, session, ...over };
}

describe("@P1 AgentModuleHost — prewarm policy gate", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    (useAgentModule as unknown as Mock).mockReturnValue(mockApi);
    cleanup();
  });

  it('prewarm "onHostMount" engages (mounts the session hook) on mount, uncontrolled', () => {
    render(<AgentModuleHost config={makeConfig({ prewarm: "onHostMount" })} />);
    expect(useAgentModule).toHaveBeenCalledTimes(1);
  });

  it('prewarm "lazy" (default) stays dormant — never mounts the hook uncontrolled', () => {
    render(<AgentModuleHost config={makeConfig({ prewarm: "lazy" })} />);
    expect(useAgentModule).not.toHaveBeenCalled();
  });

  it("omitted prewarm defaults to lazy (dormant)", () => {
    render(<AgentModuleHost config={makeConfig()} />);
    expect(useAgentModule).not.toHaveBeenCalled();
  });

  it("controlled active=true engages a lazy host (imperative engage)", () => {
    render(<AgentModuleHost config={makeConfig({ prewarm: "lazy" })} active={true} />);
    expect(useAgentModule).toHaveBeenCalledTimes(1);
  });

  it("controlled active=false keeps an onHostMount host dormant", () => {
    render(<AgentModuleHost config={makeConfig({ prewarm: "onHostMount" })} active={false} />);
    expect(useAgentModule).not.toHaveBeenCalled();
  });

  it("flipping active false→true engages; true→false disengages", () => {
    const config = makeConfig({ prewarm: "onHostMount" });
    const { rerender } = render(<AgentModuleHost config={config} active={false} />);
    expect(useAgentModule).not.toHaveBeenCalled();

    rerender(<AgentModuleHost config={config} active={true} />);
    expect(useAgentModule).toHaveBeenCalledTimes(1);

    rerender(<AgentModuleHost config={config} active={false} />);
    // No new mount; the hook unmounts (session settles in the store, PTY stays live).
    expect(useAgentModule).toHaveBeenCalledTimes(1);
  });

  it("passes the bound api to the children render-prop only while engaged", () => {
    const child = vi.fn((_api: AgentModuleApi) => null);
    const config = makeConfig({ prewarm: "lazy" });
    const { rerender } = render(
      <AgentModuleHost config={config} active={false}>
        {child}
      </AgentModuleHost>,
    );
    expect(child).not.toHaveBeenCalled();

    rerender(
      <AgentModuleHost config={config} active={true}>
        {child}
      </AgentModuleHost>,
    );
    expect(child).toHaveBeenCalledWith(mockApi);
  });

  it("headless warm (no children) engages without throwing", () => {
    expect(() =>
      render(<AgentModuleHost config={makeConfig({ prewarm: "onHostMount" })} />),
    ).not.toThrow();
    expect(useAgentModule).toHaveBeenCalledTimes(1);
  });
});
