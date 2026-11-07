// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";

import {
  runCanvasRenderRequest,
  type CanvasRenderDeps,
} from "@/shared/hooks/use-canvas-render-bridge";

function makeDeps(overrides: Partial<CanvasRenderDeps> = {}): {
  deps: CanvasRenderDeps;
  render: ReturnType<typeof vi.fn>;
  resolve: ReturnType<typeof vi.fn>;
} {
  const render = vi.fn(async () => "BASE64PNG");
  const resolve = vi.fn(async () => {});
  const deps: CanvasRenderDeps = { render, resolve, ...overrides };
  return { deps, render, resolve };
}

describe("runCanvasRenderRequest (SW-SC-14)", () => {
  // MCP architectural reframe (2026-10): the bridge resolves with the base64
  // PNG bytes DIRECTLY. The old sidecar step (writeBinaryContentFile) is gone
  // — the Rust verb embeds the bytes inline in its MCP response.
  it("renders and resolves with the base64 bytes directly", async () => {
    const { deps, render, resolve } = makeDeps();

    await runCanvasRenderRequest({ requestId: "canvas-render-0", path: "/x/d.excalidraw" }, deps);

    expect(render).toHaveBeenCalledWith("/x/d.excalidraw");
    // Resolves the bridge with the PNG bytes and no error — NO sidecar write.
    expect(resolve).toHaveBeenCalledWith("canvas-render-0", "BASE64PNG", null);
  });

  it("resolves with an error (not the bytes) when the render throws", async () => {
    const { deps, resolve } = makeDeps({
      render: vi.fn(async () => {
        throw new Error("engine boom");
      }),
    });

    await runCanvasRenderRequest({ requestId: "canvas-render-1", path: "/x/d.excalidraw" }, deps);

    expect(resolve).toHaveBeenCalledWith("canvas-render-1", null, "engine boom");
  });

  it("never rejects — a failure is reported through resolve, not thrown", async () => {
    const { deps } = makeDeps({
      render: vi.fn(async () => {
        throw new Error("boom");
      }),
    });
    await expect(
      runCanvasRenderRequest({ requestId: "canvas-render-2", path: "/x/d.excalidraw" }, deps),
    ).resolves.toBeUndefined();
  });

  it("never rejects even when the resolve IPC itself throws", async () => {
    // A failed reply can't be recovered (the Rust verb times out); it must be
    // swallowed-and-logged, not surface as an unhandled rejection.
    const { deps } = makeDeps({
      resolve: vi.fn(async () => {
        throw new Error("ipc down");
      }),
    });
    await expect(
      runCanvasRenderRequest({ requestId: "canvas-render-3", path: "/x/d.excalidraw" }, deps),
    ).resolves.toBeUndefined();
  });

  it("replies exactly once on success (no double resolve)", async () => {
    const { deps, resolve } = makeDeps();
    await runCanvasRenderRequest({ requestId: "canvas-render-4", path: "/x/d.excalidraw" }, deps);
    expect(resolve).toHaveBeenCalledTimes(1);
  });
});

it("uses the admitted scene without reacquiring active-team file authority", async () => {
  const { deps, render } = makeDeps();
  await runCanvasRenderRequest(
    { requestId: "scoped", path: "/parent/shared.excalidraw", sceneJson: "admitted scene" },
    deps,
  );
  expect(render).toHaveBeenCalledWith("/parent/shared.excalidraw", "admitted scene");
});
