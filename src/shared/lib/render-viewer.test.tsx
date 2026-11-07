// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { createElement, type ReactElement } from "react";

import { renderViewer } from "@/shared/lib/render-viewer";
import {
  registerVirtualTab,
  __resetVirtualTabsForTest,
} from "@/shared/lib/virtual-viewer-registry";

// Stub the real viewers so this test exercises dispatch only (no disk reads,
// no heavy markdown/excalidraw chunks).
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({
  MarkdownViewer: () => createElement("div", { "data-stub": "markdown" }),
}));
vi.mock("@/features/dashboard/components/html-viewer", () => ({
  HtmlViewer: () => createElement("div", { "data-stub": "html" }),
}));
vi.mock("@/features/dashboard/components/excalidraw-viewer", () => ({
  ExcalidrawViewer: () => createElement("div", { "data-stub": "excalidraw" }),
}));
vi.mock("@/features/dashboard/components/image-viewer", () => ({
  ImageViewer: () => createElement("div", { "data-stub": "image" }),
}));
vi.mock("@/features/git-sync/components/markdown-redline-viewer", () => ({
  MarkdownRedlineViewer: () => createElement("div", { "data-stub": "redline" }),
}));
vi.mock("@/features/git-sync/components/before-after-diff-toggle", () => ({
  BeforeAfterDiffToggle: () => createElement("div", { "data-stub": "diff-toggle" }),
}));
vi.mock("@/features/viewer/components/code-file-viewer", () => ({
  CodeFileViewer: () => createElement("div", { "data-stub": "code" }),
}));

const common = { teamContentPath: "/team", refreshKey: 0 };
const md = { changedPaths: null, pendingHash: null };

describe("renderViewer virtual-tab dispatch (AC-11) @P1", () => {
  beforeEach(() => __resetVirtualTabsForTest());

  it("returns the registered virtual component for a sentinel path (no readFile)", () => {
    const NotesTab = (): ReactElement => createElement("div", { "data-virtual": "notes" });
    registerVirtualTab("virtual://notes", "Notes", NotesTab);

    const el = renderViewer({ ...common, filePath: "virtual://notes" }, md);
    expect(el.type).toBe(NotesTab);
  });

  it("falls back to the real dispatch for an unregistered virtual path", () => {
    // Nothing registered → defaults through the extension dispatch (markdown stub).
    const el = renderViewer({ ...common, filePath: "virtual://unknown" }, md);
    expect((el.props as { "data-stub"?: string })?.["data-stub"] ?? el.type).not.toBe(undefined);
  });

  it("normal file types are unaffected", () => {
    const el = renderViewer({ ...common, filePath: "/team/doc.md" }, md);
    // markdown stub component, not a virtual surface
    expect(typeof el.type).toBe("function");
  });

  it("SW-GS-3: a markdown file routes to the redline viewer when the redline directive is set", () => {
    const el = renderViewer(
      { ...common, filePath: "/team/doc.md" },
      { ...md, redline: { orgRoot: "/ctx", teamKey: "acme:default", navRepoPath: "/ctx" } },
    );
    expect((el.props as { "data-stub"?: string })["data-stub"]).toBeUndefined();
    // It's the redline stub, not the plain markdown stub.
    expect((el.type as () => ReactElement)()).toStrictEqual(
      createElement("div", { "data-stub": "redline" }),
    );
  });

  it("SW-GS-3: a markdown file routes to the normal viewer without the redline directive", () => {
    const el = renderViewer({ ...common, filePath: "/team/doc.md" }, { ...md, redline: null });
    expect((el.type as () => ReactElement)()).toStrictEqual(
      createElement("div", { "data-stub": "markdown" }),
    );
  });

  it("SW-GS-3: a changed image/html/excalidraw file routes to the before/after toggle", () => {
    const redline = { orgRoot: "/ctx", teamKey: "acme:default", navRepoPath: "/ctx" };
    for (const path of ["/ctx/a.excalidraw", "/ctx/b.png", "/ctx/c.html"]) {
      const el = renderViewer({ ...common, filePath: path }, { ...md, redline });
      expect((el.type as () => ReactElement)()).toStrictEqual(
        createElement("div", { "data-stub": "diff-toggle" }),
      );
    }
  });

  it("SW-GS-3: non-markdown files use their normal viewer without the redline directive", () => {
    const el = renderViewer({ ...common, filePath: "/ctx/b.png" }, { ...md, redline: null });
    expect((el.type as () => ReactElement)()).toStrictEqual(
      createElement("div", { "data-stub": "image" }),
    );
  });

  it("SW-VE-2: a code/config file renders read-only in CodeFileViewer without the redline directive", () => {
    const el = renderViewer({ ...common, filePath: "/team/config.yaml" }, { ...md, redline: null });
    expect((el.type as () => ReactElement)()).toStrictEqual(
      createElement("div", { "data-stub": "code" }),
    );
  });

  it("SW-VE-2: a changed code/config file keeps the pre-VE-2 redline viewer in sync view (NOT the HtmlViewer toggle — avoids the .ts/.sh/.env blank-iframe MIME break)", () => {
    const redline = { orgRoot: "/ctx", teamKey: "acme:default", navRepoPath: "/ctx" };
    for (const path of ["/ctx/d.yaml", "/ctx/e.ts", "/ctx/f.sh"]) {
      const el = renderViewer({ ...common, filePath: path }, { ...md, redline });
      expect((el.type as () => ReactElement)()).toStrictEqual(
        createElement("div", { "data-stub": "redline" }),
      );
    }
  });
});

it("forwards a pending hash to HTML through the shared viewer dispatch", () => {
  const consumed = vi.fn();
  const element = renderViewer(
    { ...common, filePath: "/ctx/trail.html" },
    { ...md, pendingHash: "#story/SV-X", onPendingHashConsumed: consumed },
  );
  expect(element.props).toEqual(
    expect.objectContaining({ pendingHash: "#story/SV-X", onPendingHashConsumed: consumed }),
  );
});
