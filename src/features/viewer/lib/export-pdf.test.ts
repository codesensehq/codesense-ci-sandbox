// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { canExportToPdf, exportViewerToPdf } from "./export-pdf";

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

const mockSave = vi.fn();
vi.mock("@tauri-apps/plugin-dialog", () => ({
  save: (...args: unknown[]) => mockSave(...args),
}));

const mockGetHttpApiPort = vi.fn();
const mockExportPdfFromUrl = vi.fn();
const mockExportPdfFromHtml = vi.fn();
const mockReadFile = vi.fn();
vi.mock("@/tauri/commands", async () => ({
  ...(await import("../../../../tests/support/fixtures/frame")).frameCommands(),
  getHttpApiPort: () => mockGetHttpApiPort(),
  exportPdfFromUrl: (...args: unknown[]) => mockExportPdfFromUrl(...args),
  exportPdfFromHtml: (...args: unknown[]) => mockExportPdfFromHtml(...args),
  readFile: (...args: unknown[]) => mockReadFile(...args),
}));

const mockParseToExportHtml = vi.fn();
vi.mock("@/shared/lib/markdown-worker/client", () => ({
  parseToExportHtml: (...args: unknown[]) => mockParseToExportHtml(...args),
}));

vi.mock("sonner", () => {
  const toastFn = Object.assign(vi.fn(), {
    success: vi.fn(),
    error: vi.fn(),
    loading: vi.fn(() => "toast-id"),
    dismiss: vi.fn(),
  });
  return { toast: toastFn };
});

beforeEach(() => {
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// SW-VE-2: canExportToPdf — export is offered ONLY for doc types (md/html)
// ---------------------------------------------------------------------------

describe("canExportToPdf (SW-VE-2)", () => {
  it("is true for markdown and html docs", () => {
    expect(canExportToPdf("/x/readme.md")).toBe(true);
    expect(canExportToPdf("/x/readme.markdown")).toBe(true);
    expect(canExportToPdf("/x/dash.html")).toBe(true);
    expect(canExportToPdf("/x/page.htm")).toBe(true);
  });

  it("is false for code/config, images, and unknown text (the markdown worker would mangle them)", () => {
    expect(canExportToPdf("/x/config.yaml")).toBe(false);
    expect(canExportToPdf("/x/data.json")).toBe(false);
    expect(canExportToPdf("/x/app.ts")).toBe(false);
    expect(canExportToPdf("/x/shot.png")).toBe(false);
    expect(canExportToPdf("/x/notes.conf")).toBe(false);
    expect(canExportToPdf("/x/Makefile")).toBe(false);
  });

  it("guards the entry point — a code file never reaches the export worker", async () => {
    await exportViewerToPdf("/x/config.yaml", "/x");
    expect(mockSave).not.toHaveBeenCalled();
    expect(mockReadFile).not.toHaveBeenCalled();
    expect(mockParseToExportHtml).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// AC-15b: PDF export root threading
// ---------------------------------------------------------------------------

describe("exportViewerToPdf — root threading (SF-1 AC-15)", () => {
  it("HTML branch: encodes teamContentPath as the buildViewerUrl root (not the legacy contextRepoPath)", async () => {
    mockSave.mockResolvedValue("/save/dest.pdf");
    mockGetHttpApiPort.mockResolvedValue(9123);
    mockExportPdfFromUrl.mockResolvedValue(undefined);

    const teamContentPath = "/home/u/.codesense/users/42/orgs/org/_team/default/content";
    const htmlFile = `${teamContentPath}/sprint-1/mockups/dialog.html`;

    await exportViewerToPdf(htmlFile, teamContentPath);

    expect(mockExportPdfFromUrl).toHaveBeenCalledTimes(1);
    const [url, savePath] = mockExportPdfFromUrl.mock.calls[0];
    expect(savePath).toBe("/save/dest.pdf");

    // base64url-encode the team content path the same way buildViewerUrl
    // does (RFC 4648 §5, no padding).
    const bytes = new TextEncoder().encode(teamContentPath);
    const bin = Array.from(bytes, (b) => String.fromCharCode(b)).join("");
    const expectedRoot = btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

    expect(url).toContain(`http://f-test.localhost:9123/viewer/${expectedRoot}/`);
    expect(url).toContain("sprint-1/mockups/dialog.html");
    // Negative: the legacy contextRepoPath shape (`.../repos/{owner}/{repo}`)
    // must not have been base64-encoded into the URL.
    expect(url).not.toContain("/repos/");
  });

  it("save dialog cancellation short-circuits — no Tauri export call", async () => {
    mockSave.mockResolvedValue(null);

    await exportViewerToPdf("/team/content/x.html", "/team/content");

    expect(mockExportPdfFromUrl).not.toHaveBeenCalled();
    expect(mockExportPdfFromHtml).not.toHaveBeenCalled();
  });

  it("markdown branch: teamContentPath param is unused — exportPdfFromHtml called with parsed body", async () => {
    mockSave.mockResolvedValue("/save/dest.pdf");
    mockReadFile.mockResolvedValue("# Markdown body");
    mockParseToExportHtml.mockReturnValue({ promise: Promise.resolve("<h1>Markdown body</h1>") });
    mockExportPdfFromHtml.mockResolvedValue(undefined);

    await exportViewerToPdf("/team/content/doc.md", "/team/content");

    expect(mockExportPdfFromHtml).toHaveBeenCalledTimes(1);
    // Pin the body content actually reaches the export call (not just that
    // the export was triggered). Catches regressions that drop the worker
    // output or fail to await `.promise` before forwarding to the Tauri
    // command.
    expect(mockExportPdfFromHtml).toHaveBeenCalledWith(
      expect.stringContaining("<h1>Markdown body</h1>"),
      "/save/dest.pdf",
    );
    expect(mockExportPdfFromUrl).not.toHaveBeenCalled();
    expect(mockGetHttpApiPort).not.toHaveBeenCalled();
  });
});
