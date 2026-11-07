// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

// The app mounts only once the CodeSense home is known, so no synchronous
// `getCodesenseHome()` reader (resolver context, epic opener, surface retry,
// watch bridge, workspace sync) can ever run against an unresolved home — even
// when orgs load before the OS home read would have finished.
const render = vi.fn();
let resolveHome: (dir: string) => void = () => {};
let homeAtRender: string | null | undefined;

vi.mock("@tauri-apps/api/path", () => ({
  homeDir: () => new Promise<string>((done) => (resolveHome = done)),
}));
vi.mock("react-dom/client", () => ({
  default: {
    createRoot: () => ({
      render: async (...args: unknown[]) => {
        const { getCodesenseHome } = await import("@/shared/lib/codesense-home");
        homeAtRender = getCodesenseHome();
        render(...args);
      },
    }),
  },
}));
vi.mock("./app/App", () => ({ default: () => null }));
vi.mock("./app/install-global-error-handlers", () => ({ installGlobalErrorHandlers: vi.fn() }));
vi.mock("./app/renderer-heartbeat", () => ({ startRendererHeartbeat: vi.fn() }));
vi.mock("./app/mock/install", () => ({ installMockBackend: vi.fn() }));
vi.mock("@/shared/lib/logger", () => ({
  initLogging: vi.fn(async () => {}),
  logger: { warn: vi.fn(), info: vi.fn(), error: vi.fn(), debug: vi.fn() },
}));

describe("main", () => {
  it("renders the app only after the CodeSense home resolves", async () => {
    document.body.innerHTML = '<div id="root"></div>';
    await import("./main");
    await new Promise((done) => setTimeout(done, 0));
    expect(render).not.toHaveBeenCalled();

    resolveHome("/Users/me");
    await vi.waitFor(() => expect(render).toHaveBeenCalledTimes(1));
    expect(homeAtRender).toBe("/Users/me/.codesense");
  });
});
