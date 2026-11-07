// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup, fireEvent } from "@testing-library/react";
import { ImageViewer } from "./image-viewer";

// Mock the Tauri port command (URL-served pattern, mirrors html-viewer test).
const mockGetHttpApiPort = vi.fn();
vi.mock("@/tauri/commands", async () => ({
  ...(await import("../../../../tests/support/fixtures/frame")).frameCommands(),
  getHttpApiPort: () => mockGetHttpApiPort(),
}));

// closeTab is only invoked from the placeholder's Close button; stub the store.
vi.mock("@/shared/stores/viewer-store", () => ({
  useViewerStore: { getState: () => ({ closeTab: vi.fn() }) },
}));

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
});

describe("ImageViewer (SW-SC-7)", () => {
  it("shows a loading state while the port resolves", () => {
    mockGetHttpApiPort.mockReturnValue(new Promise(() => {})); // never resolves
    render(<ImageViewer filePath="/repo/shot.png" teamContentPath="/repo" />);
    expect(screen.getByText("Loading…")).toBeInTheDocument();
  });

  it("renders an <img> with the URL-served src (port + path + cache-bust)", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<ImageViewer filePath="/repo/assets/shot.png" refreshKey={5} teamContentPath="/repo" />);
    await waitFor(() => {
      const img = document.querySelector("img");
      expect(img).toBeTruthy();
      const src = img!.getAttribute("src")!;
      expect(src).toContain("http://f-test.localhost:9123/viewer/");
      expect(src).toContain("assets/shot.png");
      expect(src).toContain("v=5");
    });
  });

  it("loads a baseline image through a minted frame origin", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(
      <ImageViewer
        filePath="/repo/shot.png"
        teamContentPath="/repo"
        baselineSource={{ orgRoot: "/repo", relPath: "shot.png" }}
      />,
    );
    await waitFor(() =>
      expect(document.querySelector("img")?.getAttribute("src")).toContain(
        "http://f-test.localhost:9123/viewer-baseline/",
      ),
    );
  });

  it("falls back to a non-fatal placeholder when the image fails to load (AC-5)", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    render(<ImageViewer filePath="/repo/missing.png" teamContentPath="/repo" />);
    const img = await waitFor(() => {
      const el = document.querySelector("img");
      expect(el).toBeTruthy();
      return el!;
    });
    fireEvent.error(img);
    await waitFor(() => {
      expect(screen.getByText("missing.png")).toBeInTheDocument();
      expect(screen.getByText("This file was removed from disk")).toBeInTheDocument();
    });
  });

  it("hot-reloads only when THIS file changed (AC-7)", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const { rerender } = render(
      <ImageViewer
        filePath="/repo/shot.png"
        refreshKey={1}
        teamContentPath="/repo"
        changedPaths={null}
      />,
    );
    const srcAt = async () =>
      await waitFor(() => {
        const img = document.querySelector("img");
        expect(img).toBeTruthy();
        return img!.getAttribute("src")!;
      });
    expect(await srcAt()).toContain("v=1");

    // A watcher tick for an UNRELATED file must not advance the cache-bust.
    rerender(
      <ImageViewer
        filePath="/repo/shot.png"
        refreshKey={2}
        teamContentPath="/repo"
        changedPaths={new Set(["/repo/other.png"])}
      />,
    );
    expect(await srcAt()).toContain("v=1");

    // A watcher tick that INCLUDES this file advances it.
    rerender(
      <ImageViewer
        filePath="/repo/shot.png"
        refreshKey={3}
        teamContentPath="/repo"
        changedPaths={new Set(["/repo/shot.png"])}
      />,
    );
    expect(await srcAt()).toContain("v=3");
  });

  // SW-SC-11: an atomic-temp sibling (`<file>.tmp.<pid>.<hash>`) must not advance
  // the cache-bust; the real rename destination the Rust emit forwards (temp
  // filtered) does. Same shared gate as the canvas — one fix, all four viewers.
  it("ignores an atomic-temp sibling, advances on the real rename target (SW-SC-11)", async () => {
    mockGetHttpApiPort.mockResolvedValue(9123);
    const { rerender } = render(
      <ImageViewer
        filePath="/repo/shot.png"
        refreshKey={1}
        teamContentPath="/repo"
        changedPaths={null}
      />,
    );
    const srcAt = async () =>
      await waitFor(() => {
        const img = document.querySelector("img");
        expect(img).toBeTruthy();
        return img!.getAttribute("src")!;
      });
    expect(await srcAt()).toContain("v=1");

    // Temp sibling → no advance.
    rerender(
      <ImageViewer
        filePath="/repo/shot.png"
        refreshKey={2}
        teamContentPath="/repo"
        changedPaths={new Set(["/repo/shot.png.tmp.99999.deadbeef"])}
      />,
    );
    expect(await srcAt()).toContain("v=1");

    // Real rename target → advance.
    rerender(
      <ImageViewer
        filePath="/repo/shot.png"
        refreshKey={3}
        teamContentPath="/repo"
        changedPaths={new Set(["/repo/shot.png"])}
      />,
    );
    expect(await srcAt()).toContain("v=3");
  });
});
