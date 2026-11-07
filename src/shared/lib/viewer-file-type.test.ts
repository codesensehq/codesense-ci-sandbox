// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import {
  VIEWER_EXTENSIONS,
  VIEWER_LINK_RE,
  buildViewerUrl,
  buildPaneViewerUrl,
  buildMessageViewerUrl,
  buildBaselineViewerUrl,
  canvasPngPath,
  getViewerFileType,
  isViewerFile,
  isViewerFilePath,
  isFrontmatterEditable,
  isKnownBinaryPath,
  isPreviewableFilePath,
  isDerivedRenderArtifact,
  isEditableTextFile,
  isLivePreviewEligible,
  resolveViewerLinkPath,
  snapshotArchivePath,
  snapshotTimestamp,
  stripViewerExtension,
} from "./viewer-file-type";

// ---------------------------------------------------------------------------
// SW-VE-8: isEditableTextFile — the text-ness half of the widened edit gate
// ---------------------------------------------------------------------------
describe("isEditableTextFile", () => {
  it("is true for markdown docs (widens VE-3 code-only edit)", () => {
    expect(isEditableTextFile("README.md")).toBe(true);
    expect(isEditableTextFile("notes.markdown")).toBe(true);
  });

  it("is true for html docs", () => {
    expect(isEditableTextFile("dashboard.html")).toBe(true);
    expect(isEditableTextFile("page.htm")).toBe(true);
  });

  it("is true for code/config and unknown-text (catch-all → code)", () => {
    expect(isEditableTextFile("main.rs")).toBe(true);
    expect(isEditableTextFile("config.yaml")).toBe(true);
    expect(isEditableTextFile("data.json")).toBe(true);
    expect(isEditableTextFile("notes.txt")).toBe(true); // unknown text → code
    expect(isEditableTextFile("LICENSE")).toBe(true); // extensionless → code
  });

  it("is FALSE for images / excalidraw / known binaries (AC-6)", () => {
    expect(isEditableTextFile("logo.png")).toBe(false);
    expect(isEditableTextFile("photo.jpg")).toBe(false);
    expect(isEditableTextFile("design.excalidraw")).toBe(false);
    expect(isEditableTextFile("archive.zip")).toBe(false); // catch-alls to code but not previewable
    expect(isEditableTextFile("app.exe")).toBe(false);
    expect(isEditableTextFile("font.woff2")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// SW-VE-10: isLivePreviewEligible — markdown-only gate for the rendered preview
// ---------------------------------------------------------------------------
describe("isLivePreviewEligible", () => {
  it("is true for markdown docs only", () => {
    expect(isLivePreviewEligible("README.md")).toBe(true);
    expect(isLivePreviewEligible("notes.markdown")).toBe(true);
  });

  it("is FALSE for html / code / config / unknown-text (raw-source fallback)", () => {
    expect(isLivePreviewEligible("dashboard.html")).toBe(false);
    expect(isLivePreviewEligible("main.rs")).toBe(false);
    expect(isLivePreviewEligible("config.yaml")).toBe(false);
    expect(isLivePreviewEligible("notes.txt")).toBe(false);
    expect(isLivePreviewEligible("LICENSE")).toBe(false);
  });

  it("is FALSE for images / binaries and for null/undefined", () => {
    expect(isLivePreviewEligible("logo.png")).toBe(false);
    expect(isLivePreviewEligible("archive.zip")).toBe(false);
    expect(isLivePreviewEligible(null)).toBe(false);
    expect(isLivePreviewEligible(undefined)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// VIEWER_EXTENSIONS map
// ---------------------------------------------------------------------------
describe("VIEWER_EXTENSIONS", () => {
  it("maps md to markdown", () => {
    expect(VIEWER_EXTENSIONS.md).toBe("markdown");
  });

  it("maps html to html", () => {
    expect(VIEWER_EXTENSIONS.html).toBe("html");
  });

  it("maps htm to html", () => {
    expect(VIEWER_EXTENSIONS.htm).toBe("html");
  });
});

// ---------------------------------------------------------------------------
// getViewerFileType
// ---------------------------------------------------------------------------
describe("getViewerFileType", () => {
  it("returns 'markdown' for .md files", () => {
    expect(getViewerFileType("/repo/docs/readme.md")).toBe("markdown");
  });

  it("returns 'html' for .html files", () => {
    expect(getViewerFileType("/repo/mockup.html")).toBe("html");
  });

  it("returns 'html' for .htm files", () => {
    expect(getViewerFileType("/repo/page.htm")).toBe("html");
  });

  it("handles uppercase extensions (.HTML)", () => {
    expect(getViewerFileType("/repo/DASHBOARD.HTML")).toBe("html");
  });

  it("handles mixed case extensions (.Md)", () => {
    expect(getViewerFileType("/repo/README.Md")).toBe("markdown");
  });

  it("SW-VE-2: defaults to 'code' when no extension (never markdown prose)", () => {
    expect(getViewerFileType("/repo/Makefile")).toBe("code");
  });

  it("uses the final extension for double extensions (.test.html)", () => {
    expect(getViewerFileType("/repo/file.test.html")).toBe("html");
  });

  it("handles paths with dots in directories", () => {
    expect(getViewerFileType("/repo/v1.2/notes.md")).toBe("markdown");
  });

  it("SW-VE-2: unknown text extension falls back to 'code', not 'markdown' prose", () => {
    expect(getViewerFileType("/repo/app.conf")).toBe("code");
    expect(getViewerFileType("/repo/db.properties")).toBe("code");
  });
});

// ---------------------------------------------------------------------------
// isViewerFile
// ---------------------------------------------------------------------------
describe("isViewerFile", () => {
  it("returns true for 'md'", () => {
    expect(isViewerFile("md")).toBe(true);
  });

  it("returns true for 'html'", () => {
    expect(isViewerFile("html")).toBe(true);
  });

  it("returns true for 'htm'", () => {
    expect(isViewerFile("htm")).toBe(true);
  });

  it("is case-insensitive — 'HTML' returns true", () => {
    expect(isViewerFile("HTML")).toBe(true);
  });

  it("is case-insensitive — 'MD' returns true", () => {
    expect(isViewerFile("MD")).toBe(true);
  });

  it("returns false for null", () => {
    expect(isViewerFile(null)).toBe(false);
  });

  it("returns false for empty string", () => {
    expect(isViewerFile("")).toBe(false);
  });

  it("SW-VE-2: returns true for code/config extensions (json/yaml/toml/…)", () => {
    expect(isViewerFile("json")).toBe(true);
    expect(isViewerFile("yaml")).toBe(true);
    expect(isViewerFile("toml")).toBe(true);
    expect(isViewerFile("ts")).toBe(true);
  });

  it("returns false for 'txt' (not in the explicit map — catch-all handles it at render)", () => {
    expect(isViewerFile("txt")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// isViewerFilePath (SW-VE-1)
// ---------------------------------------------------------------------------
describe("isViewerFilePath", () => {
  it("returns true for viewer-supported paths (md / html / image / excalidraw)", () => {
    expect(isViewerFilePath("/repo/docs/readme.md")).toBe(true);
    expect(isViewerFilePath("/repo/mockup.html")).toBe(true);
    expect(isViewerFilePath("/repo/shot.png")).toBe(true);
    expect(isViewerFilePath("/repo/design.excalidraw")).toBe(true);
  });

  it("SW-VE-2: returns true for code/config files (now viewer-supported)", () => {
    expect(isViewerFilePath("/repo/core.yaml")).toBe(true);
    expect(isViewerFilePath("/repo/agents.toml")).toBe(true);
    expect(isViewerFilePath("/repo/data.json")).toBe(true);
  });

  it("returns false for a true binary (archive) — routed to the placeholder", () => {
    expect(isViewerFilePath("/repo/archive.zip")).toBe(false);
  });

  it("returns false for an extensionless file (not in the explicit viewer map)", () => {
    expect(isViewerFilePath("/repo/Makefile")).toBe(false);
  });

  it("uses the final extension and is case-insensitive", () => {
    expect(isViewerFilePath("/repo/v1.2/notes.MD")).toBe(true);
    expect(isViewerFilePath("/repo/file.test.HTML")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// stripViewerExtension
// ---------------------------------------------------------------------------
describe("stripViewerExtension", () => {
  it("strips .md", () => {
    expect(stripViewerExtension("readme.md")).toBe("readme");
  });

  it("strips .html", () => {
    expect(stripViewerExtension("dashboard.html")).toBe("dashboard");
  });

  it("strips .htm", () => {
    expect(stripViewerExtension("page.htm")).toBe("page");
  });

  it("is case-insensitive — strips .MD", () => {
    expect(stripViewerExtension("README.MD")).toBe("README");
  });

  it("is case-insensitive — strips .HTML", () => {
    expect(stripViewerExtension("REPORT.HTML")).toBe("REPORT");
  });

  it("only strips the final extension (.test.html → file.test)", () => {
    expect(stripViewerExtension("file.test.html")).toBe("file.test");
  });

  it("returns the name unchanged when no extension", () => {
    expect(stripViewerExtension("Makefile")).toBe("Makefile");
  });

  it("returns the name unchanged for unrecognised extension", () => {
    expect(stripViewerExtension("data.json")).toBe("data.json");
  });

  it("handles names with only a dot", () => {
    expect(stripViewerExtension(".md")).toBe("");
  });
});

// ---------------------------------------------------------------------------
// SW-SD-1 — snapshotTimestamp: compact, sortable, filename-safe UTC stamp
// ---------------------------------------------------------------------------
describe("snapshotTimestamp", () => {
  it("produces a compact, colon/dot/dash-free UTC stamp", () => {
    const ts = snapshotTimestamp(new Date("2026-07-24T18:30:00.123Z"));
    expect(ts).toBe("20260724T183000123Z");
  });

  it("is filename-safe (no chars that need escaping in a path segment)", () => {
    const ts = snapshotTimestamp(new Date("2026-01-02T03:04:05.006Z"));
    expect(ts).toMatch(/^[A-Za-z0-9]+$/);
  });

  it("is lexically sortable in chronological order", () => {
    const earlier = snapshotTimestamp(new Date("2026-07-24T18:30:00.000Z"));
    const later = snapshotTimestamp(new Date("2026-07-24T18:30:01.000Z"));
    expect(earlier < later).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// SW-SD-1 — snapshotArchivePath: ABSOLUTE, context-root-anchored archive path
// (a relative path would be rejected by Rust validate_context_path — the whole
// reason this helper exists; see the story's Validation review #1).
// ---------------------------------------------------------------------------
describe("snapshotArchivePath", () => {
  const ROOT = "/home/u/.codesense/users/1/orgs/acme/ctx/_team/default/content";

  it("builds an ABSOLUTE path rooted at teamContentPath", () => {
    const p = snapshotArchivePath(
      ROOT,
      `${ROOT}/dash/report.html`,
      undefined,
      "20260724T183000123Z",
    );
    expect(p).toBe(`${ROOT}/dashboards/snapshots/report/20260724T183000123Z.html`);
    expect(p.startsWith("/")).toBe(true);
  });

  it("derives the slug from the file stem (extension stripped) when no name given", () => {
    const p = snapshotArchivePath(ROOT, `${ROOT}/a/b/sales-board.html`, undefined, "TS");
    expect(p).toBe(`${ROOT}/dashboards/snapshots/sales-board/TS.html`);
  });

  it("prefers an explicit name over the file stem", () => {
    const p = snapshotArchivePath(ROOT, `${ROOT}/a/report.html`, "Q3 Metrics", "TS");
    expect(p).toBe(`${ROOT}/dashboards/snapshots/Q3-Metrics/TS.html`);
  });

  it("tolerates a trailing slash on teamContentPath (no doubled separator)", () => {
    const p = snapshotArchivePath(`${ROOT}/`, `${ROOT}/report.html`, undefined, "TS");
    expect(p).toBe(`${ROOT}/dashboards/snapshots/report/TS.html`);
  });

  it("sanitizes a traversal attempt in the name to a single safe segment", () => {
    const p = snapshotArchivePath(ROOT, `${ROOT}/report.html`, "../../etc/passwd", "TS");
    // no separators, no `..`, no leading dot survive in the slug segment
    const slug = p.slice(`${ROOT}/dashboards/snapshots/`.length, p.lastIndexOf("/"));
    expect(slug).not.toContain("/");
    expect(slug).not.toContain("..");
    expect(slug.startsWith(".")).toBe(false);
  });

  it("sanitizes separators/spaces in the name to dashes", () => {
    const p = snapshotArchivePath(ROOT, `${ROOT}/x.html`, "my cool dash/v2", "TS");
    expect(p).toBe(`${ROOT}/dashboards/snapshots/my-cool-dash-v2/TS.html`);
  });

  it("falls back to a non-empty slug when the name sanitizes to nothing", () => {
    const p = snapshotArchivePath(ROOT, `${ROOT}/....html`, "...", "TS");
    const slug = p.slice(`${ROOT}/dashboards/snapshots/`.length, p.lastIndexOf("/"));
    expect(slug.length).toBeGreaterThan(0);
  });

  it("is deterministic given a fixed ts (pure — no clock read)", () => {
    const a = snapshotArchivePath(ROOT, `${ROOT}/r.html`, undefined, "FIXED");
    const b = snapshotArchivePath(ROOT, `${ROOT}/r.html`, undefined, "FIXED");
    expect(a).toBe(b);
  });
});

// ---------------------------------------------------------------------------
// getViewerFileType — routing dispatch coverage
// ---------------------------------------------------------------------------
describe("getViewerFileType routing dispatch", () => {
  it(".html files route to 'html' (HtmlViewer)", () => {
    expect(getViewerFileType("/repo/dashboard.html")).toBe("html");
  });

  it(".htm files route to 'html' (HtmlViewer)", () => {
    expect(getViewerFileType("/repo/page.htm")).toBe("html");
  });

  it(".md files route to 'markdown' (MarkdownViewer)", () => {
    expect(getViewerFileType("/repo/readme.md")).toBe("markdown");
  });

  it("html and markdown are different routes", () => {
    expect(getViewerFileType("/repo/file.html")).not.toBe(getViewerFileType("/repo/file.md"));
  });
});

// ---------------------------------------------------------------------------
// VIEWER_LINK_RE
// ---------------------------------------------------------------------------
describe("VIEWER_LINK_RE", () => {
  it.each(["file.md", "file.html", "file.htm", "FILE.MD", "FILE.HTML"])("matches %s", (href) => {
    expect(VIEWER_LINK_RE.test(href)).toBe(true);
  });

  // `.markdown` is a viewer type (renders as prose) but deliberately NOT
  // link-navigable — the `md` alternation must not over-match it (mirrors images
  // being viewable-but-not-navigable). Guarded here + in rehype-blocks.test.ts.
  it.each(["file.txt", "file.json", "file.md5", "file.markdown", "file.html5", "file.htmlx"])(
    "does not match %s",
    (href) => {
      expect(VIEWER_LINK_RE.test(href)).toBe(false);
    },
  );

  it("matches .md/ (directory-like path segment)", () => {
    expect(VIEWER_LINK_RE.test("docs.md/page")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// resolveViewerLinkPath
// ---------------------------------------------------------------------------
describe("resolveViewerLinkPath", () => {
  it("resolves relative path against owning file directory", () => {
    expect(resolveViewerLinkPath("../notes/setup.md", "/repo/docs/index.html", "")).toBe(
      "/repo/notes/setup.md",
    );
  });

  it("resolves sibling relative path", () => {
    expect(resolveViewerLinkPath("other.html", "/repo/docs/index.html", "")).toBe(
      "/repo/docs/other.html",
    );
  });

  it("anchors absolute path to content/ repo root", () => {
    expect(resolveViewerLinkPath("/overview.md", "/repo/content/docs/page.html", "")).toBe(
      "/repo/overview.md",
    );
  });

  it("keeps an absolute href that already names a file under the root", () => {
    const team = "/u/orgs/acme/ctx/default";
    // A pane's link owner sits at its content root; the href is a row's full path.
    expect(
      resolveViewerLinkPath(
        `${team}/content/e/SW-1.md#ac`,
        `${team}/content/index.html`,
        `${team}/content`,
      ),
    ).toBe(`${team}/content/e/SW-1.md`);
    expect(resolveViewerLinkPath("/x/SW-1.md", "/x/docs/page.html", "/x")).toBe("/x/SW-1.md");
  });

  it("falls back to teamContentPath when no content/ in owning path", () => {
    expect(resolveViewerLinkPath("/overview.md", "/repo/docs/page.html", "/team-content")).toBe(
      "/team-content/overview.md",
    );
  });

  // SF-1 regression guard: the absolute-href + missing-CONTENT_DIR_SEGMENT
  // fallback path must keep working for markdown-owning files after the
  // viewer's `contextRepoRoot` prop is retired. `MarkdownViewer` does not
  // consume `contextRepoRoot`; its link map calls `resolveViewerLinkPath`
  // with `teamContentPath` directly. If this branch ever regresses, every
  // markdown file living outside a `/content/`-segmented path would lose
  // absolute-link nav.
  it("SF-1: anchors absolute href to teamContentPath when markdown owner lacks /content/ segment", () => {
    expect(
      resolveViewerLinkPath(
        "/docs/overview.md",
        "/repo/legacy/notes/intro.md",
        "/home/user/.codesense/users/42/orgs/codesense/_team/default/content",
      ),
    ).toBe("/home/user/.codesense/users/42/orgs/codesense/_team/default/content/docs/overview.md");
  });

  // SF-1 regression guard: when the owning markdown file lives under a
  // `/content/` segment, absolute hrefs anchor to the parent of `/content/`
  // (carry-over from pre-PTS-2 `/_bmad-output/` strip semantics — the same
  // contract the existing "anchors absolute path to content/ repo root"
  // test documents). The `/content/` anchor takes precedence over the
  // `teamContentPath` fallback so both are exercised here.
  it("SF-1: anchors absolute href to parent of /content/ when owning markdown sits inside it", () => {
    expect(
      resolveViewerLinkPath(
        "/sprint-1/readme.md",
        "/home/user/.codesense/users/42/orgs/codesense/_team/default/content/in-progress/story.md",
        "/home/user/.codesense/users/42/orgs/codesense/_team/default/content",
      ),
    ).toBe("/home/user/.codesense/users/42/orgs/codesense/_team/default/sprint-1/readme.md");
  });

  it("normalizes ./ segments", () => {
    expect(resolveViewerLinkPath("./sub/other.md", "/repo/docs/index.html", "")).toBe(
      "/repo/docs/sub/other.md",
    );
  });

  it("normalizes ../ segments through multiple levels", () => {
    expect(resolveViewerLinkPath("../../root.md", "/repo/a/b/page.html", "")).toBe("/repo/root.md");
  });

  it("normalizes mixed ./ and ../ segments", () => {
    expect(resolveViewerLinkPath("./sub/../other.md", "/repo/docs/index.html", "")).toBe(
      "/repo/docs/other.md",
    );
  });

  it("returns null for non-viewable extension", () => {
    expect(resolveViewerLinkPath("data.json", "/repo/docs/index.html", "")).toBeNull();
  });

  it("returns null for .pdf", () => {
    expect(resolveViewerLinkPath("report.pdf", "/repo/docs/index.html", "")).toBeNull();
  });

  it("strips fragment before resolving", () => {
    expect(resolveViewerLinkPath("page.md#section", "/repo/docs/index.html", "")).toBe(
      "/repo/docs/page.md",
    );
  });

  it("strips fragment from relative path with parent traversal", () => {
    expect(resolveViewerLinkPath("../readme.md#overview", "/repo/docs/sub/page.html", "")).toBe(
      "/repo/docs/readme.md",
    );
  });

  it("handles .htm extension", () => {
    expect(resolveViewerLinkPath("legacy.htm", "/repo/docs/index.html", "")).toBe(
      "/repo/docs/legacy.htm",
    );
  });

  it("returns null for empty href path (fragment-only after split)", () => {
    expect(resolveViewerLinkPath("#heading", "/repo/docs/index.html", "")).toBeNull();
  });

  it("returns null for href with no extension", () => {
    expect(resolveViewerLinkPath("Makefile", "/repo/docs/index.html", "")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// buildViewerUrl
// ---------------------------------------------------------------------------
describe("buildBaselineViewerUrl (SW-GS-3)", () => {
  it("targets the /viewer-baseline route with org_root + org-relative path", () => {
    const url = buildBaselineViewerUrl(
      "http://f-baseline.localhost:9123",
      "/home/user/ctx",
      "docs/diagram.png",
      { v: 7 },
    );
    const b64root = btoa("/home/user/ctx")
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
    expect(url).toBe(
      `http://f-baseline.localhost:9123/viewer-baseline/${b64root}/docs/diagram.png?v=7`,
    );
  });

  it("tolerates a leading slash on the relative path", () => {
    const url = buildBaselineViewerUrl("http://f-baseline.localhost:9123", "/ctx", "/a/b.png");
    expect(url).toContain("/viewer-baseline/");
    expect(url).not.toContain("//a");
  });

  it("SW-GS-10 — appends a tree OID for the conflict resolved-vs-ours redline", () => {
    const url = buildBaselineViewerUrl("http://f-baseline.localhost:9123", "/ctx", "README.md", {
      tree: "abc123",
    });
    expect(url).toContain("/viewer-baseline/");
    expect(url).toContain("/README.md?tree=abc123");
  });

  it("SW-GS-10 — combines v and tree params", () => {
    const url = buildBaselineViewerUrl("http://f-baseline.localhost:9123", "/ctx", "README.md", {
      v: 4,
      tree: "abc123",
    });
    expect(url.endsWith("?v=4&tree=abc123")).toBe(true);
  });
});

describe("buildViewerUrl", () => {
  it("builds basic URL with no params", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/docs/index.html");
    const b64root = btoa("/home/user/repo")
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
    expect(url).toBe(`http://localhost:9123/viewer/${b64root}/docs/index.html`);
  });

  it("appends cache-bust v param", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/page.html", { v: 42 });
    expect(url).toContain("?v=42");
  });

  it("omits query string when no params provided", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/file.css");
    expect(url).not.toContain("?");
  });

  // Frame identity is carried by the origin; URL params carry no credential.
  it("uses the registered frame origin without a capability query", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/dash.html", {
      origin: "http://f-test.localhost:9123",
    });
    expect(new URL(url).origin).toBe("http://f-test.localhost:9123");
    expect(url).not.toContain("cap=");
  });

  it("keeps cache busting without appending credentials", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/dash.html", {
      v: 3,
      origin: "http://f-test.localhost:9123",
    });
    expect(url).toContain("?v=3");
    expect(url).not.toContain("cap=");
  });

  it("omits cap when not provided", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/page.html", { v: 1 });
    expect(url).not.toContain("cap=");
  });

  it("handles root with trailing slash", () => {
    const url = buildViewerUrl(9123, "/home/user/repo/", "/home/user/repo/docs/index.html");
    expect(url).toContain("/docs/index.html");
    expect(url).not.toContain("//docs");
  });

  it("handles nested subdirectory paths", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/a/b/c/deep.html");
    expect(url).toContain("/a/b/c/deep.html");
  });

  it("includes v=0", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/page.html", { v: 0 });
    expect(url).toContain("v=0");
  });

  it("encodes spaces in file paths", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/my docs/file name.html");
    expect(url).toContain("/my%20docs/file%20name.html");
    expect(url).not.toContain(" ");
  });

  it("encodes special characters (#, ?, &) in path segments", () => {
    const url = buildViewerUrl(9123, "/home/user/repo", "/home/user/repo/docs/Q&A#1.html");
    expect(url).toContain("/docs/Q%26A%231.html");
  });
});

describe("viewer theme query encoding", () => {
  const theme = JSON.stringify({ scheme: "dark", tokens: { "--cs-bg": "#0d1117" } });

  it("encodes the same theme for pane and hosted-message viewer routes", () => {
    const pane = new URL(buildPaneViewerUrl(9123, "/ctx", "/ctx/panes", "inbox.html", { theme }));
    const message = new URL(buildMessageViewerUrl(9123, "acme", "mail", 7, { theme }));

    expect(JSON.parse(pane.searchParams.get("theme")!)).toEqual(JSON.parse(theme));
    expect(JSON.parse(message.searchParams.get("theme")!)).toEqual(JSON.parse(theme));
  });
});

// ---------------------------------------------------------------------------
// SW-SC-1 AC-2: `.excalidraw` viewer file type registration (TS half of the
// 3-step contract; the Rust VIEWABLE_EXTENSIONS half lives in file_index.rs).
// ---------------------------------------------------------------------------
describe("excalidraw file type", () => {
  it("maps the excalidraw extension to the excalidraw type", () => {
    expect(VIEWER_EXTENSIONS.excalidraw).toBe("excalidraw");
  });

  it("routes .excalidraw files to the excalidraw renderer", () => {
    expect(getViewerFileType("/repo/design.excalidraw")).toBe("excalidraw");
    expect(getViewerFileType("/repo/DESIGN.EXCALIDRAW")).toBe("excalidraw");
  });

  it("treats excalidraw as a viewer file for tree filtering", () => {
    expect(isViewerFile("excalidraw")).toBe(true);
    expect(isViewerFile("EXCALIDRAW")).toBe(true);
  });

  it("strips the .excalidraw extension for the tab name", () => {
    expect(stripViewerExtension("sketch.excalidraw")).toBe("sketch");
  });

  it("matches .excalidraw as an internal-link target", () => {
    // VIEWER_LINK_RE is applied after the #fragment is stripped (see
    // resolveViewerLinkPath), so the suffix is `(\/|$)`, not `#…`.
    expect(VIEWER_LINK_RE.test("./design.excalidraw")).toBe(true);
    expect(VIEWER_LINK_RE.test("/content/design.excalidraw")).toBe(true);
  });

  it("resolves a relative .excalidraw link to an absolute path", () => {
    expect(
      resolveViewerLinkPath("./design.excalidraw", "/root/content/story.md", "/root/content"),
    ).toBe("/root/content/design.excalidraw");
  });
});

// ---------------------------------------------------------------------------
// SW-SC-7: image viewer file type + derived-render guard
// ---------------------------------------------------------------------------
describe("image file type (SW-SC-7)", () => {
  const exts = ["png", "jpg", "jpeg", "gif", "webp", "svg"] as const;

  it("maps every image extension to the image type", () => {
    for (const ext of exts) {
      expect((VIEWER_EXTENSIONS as Record<string, string>)[ext]).toBe("image");
    }
  });

  it("routes image files to the image renderer (case-insensitive)", () => {
    expect(getViewerFileType("/repo/shot.png")).toBe("image");
    expect(getViewerFileType("/repo/MOCKUP.JPG")).toBe("image");
    expect(getViewerFileType("/repo/diagram.svg")).toBe("image");
  });

  it("treats images as viewer files for tree filtering", () => {
    expect(isViewerFile("png")).toBe(true);
    expect(isViewerFile("WEBP")).toBe(true);
  });

  it("keeps the extension on image labels (AC-8 — does NOT strip)", () => {
    expect(stripViewerExtension("screenshot.png")).toBe("screenshot.png");
    expect(stripViewerExtension("LOGO.SVG")).toBe("LOGO.SVG");
    // contrast: doc types still strip
    expect(stripViewerExtension("readme.md")).toBe("readme");
  });

  it("does NOT treat images as navigable link targets (AC-1b)", () => {
    expect(VIEWER_LINK_RE.test("x.png")).toBe(false);
    expect(VIEWER_LINK_RE.test("./shot.jpg")).toBe(false);
    expect(VIEWER_LINK_RE.test("/content/diagram.svg")).toBe(false);
  });
});

describe("isDerivedRenderArtifact (SW-SC-7 AC-9)", () => {
  it("matches a derived canvas render", () => {
    expect(isDerivedRenderArtifact("design.excalidraw.png")).toBe(true);
    expect(isDerivedRenderArtifact("DESIGN.EXCALIDRAW.PNG")).toBe(true);
  });

  it("does not match a plain image or a canvas source", () => {
    expect(isDerivedRenderArtifact("screenshot.png")).toBe(false);
    expect(isDerivedRenderArtifact("design.excalidraw")).toBe(false);
    expect(isDerivedRenderArtifact("notes.md")).toBe(false);
  });
});

describe("canvasPngPath (SW-SC-2)", () => {
  // Suffix-append, never an extension swap — so it filters as a derived
  // artifact (isDerivedRenderArtifact) and never collides with a user's own
  // `<name>.png`. Shared by SW-SC-2's auto-render and SW-SC-14's render bridge.
  it("appends .png to the full .excalidraw path", () => {
    expect(canvasPngPath("/ctx/team/design.excalidraw")).toBe("/ctx/team/design.excalidraw.png");
  });
});

// ---------------------------------------------------------------------------
// SW-VE-2: code viewer file type — the named code/config/data set + catch-all
// ---------------------------------------------------------------------------
describe("code file type (SW-VE-2)", () => {
  const codeExts = [
    "yaml",
    "yml",
    "toml",
    "json",
    "xml",
    "ini",
    "env",
    "csv",
    "sh",
    "py",
    "rs",
    "ts",
    "js",
  ] as const;

  it("maps every named code/config/data extension to the code type", () => {
    for (const ext of codeExts) {
      expect((VIEWER_EXTENSIONS as Record<string, string>)[ext]).toBe("code");
    }
  });

  it("routes each representative extension to the code renderer (AC-1)", () => {
    expect(getViewerFileType("/repo/config.yaml")).toBe("code");
    expect(getViewerFileType("/repo/Cargo.toml")).toBe("code");
    expect(getViewerFileType("/repo/data.json")).toBe("code");
    expect(getViewerFileType("/repo/pom.xml")).toBe("code");
    expect(getViewerFileType("/repo/.env")).toBe("code");
    expect(getViewerFileType("/repo/rows.csv")).toBe("code");
    expect(getViewerFileType("/repo/deploy.sh")).toBe("code");
    expect(getViewerFileType("/repo/main.py")).toBe("code");
    expect(getViewerFileType("/repo/lib.rs")).toBe("code");
    expect(getViewerFileType("/repo/app.ts")).toBe("code");
    expect(getViewerFileType("/repo/index.js")).toBe("code");
  });

  it("AC-3: markdown STILL wins for .md AND .markdown (catch-all never swallows real markdown)", () => {
    expect(getViewerFileType("/repo/readme.md")).toBe("markdown");
    expect(getViewerFileType("/repo/readme.markdown")).toBe("markdown");
    expect(getViewerFileType("/repo/README.MD")).toBe("markdown");
  });

  it("keeps the extension on code labels (like images — avoids config.yaml/config.json collision)", () => {
    expect(stripViewerExtension("config.yaml")).toBe("config.yaml");
    expect(stripViewerExtension("Cargo.toml")).toBe("Cargo.toml");
    // contrast: doc types still strip
    expect(stripViewerExtension("readme.md")).toBe("readme");
  });

  it("does NOT treat code files as navigable internal-link targets", () => {
    expect(VIEWER_LINK_RE.test("config.yaml")).toBe(false);
    expect(VIEWER_LINK_RE.test("./data.json")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// SW-VE-2: isFrontmatterEditable — the metadata-chip regression guard (AC-7)
// ---------------------------------------------------------------------------
describe("isFrontmatterEditable (SW-VE-2 AC-7)", () => {
  it("is true ONLY for markdown and html docs", () => {
    expect(isFrontmatterEditable("/repo/story.md")).toBe(true);
    expect(isFrontmatterEditable("/repo/page.html")).toBe(true);
    expect(isFrontmatterEditable("/repo/page.htm")).toBe(true);
  });

  it("is false for code/config files (so the metadata editor never corrupts them)", () => {
    expect(isFrontmatterEditable("/repo/core.yaml")).toBe(false);
    expect(isFrontmatterEditable("/repo/data.json")).toBe(false);
    expect(isFrontmatterEditable("/repo/Cargo.toml")).toBe(false);
    expect(isFrontmatterEditable("/repo/main.py")).toBe(false);
  });

  it("is false for images, excalidraw, binaries, and extensionless files", () => {
    expect(isFrontmatterEditable("/repo/shot.png")).toBe(false);
    expect(isFrontmatterEditable("/repo/design.excalidraw")).toBe(false);
    expect(isFrontmatterEditable("/repo/archive.zip")).toBe(false);
    expect(isFrontmatterEditable("/repo/Makefile")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// SW-VE-2: previewability gate — known binaries gate to placeholder up front
// ---------------------------------------------------------------------------
describe("isKnownBinaryPath / isPreviewableFilePath (SW-VE-2 AC-5)", () => {
  it("flags archives, executables, fonts, media, and binary docs as binary", () => {
    for (const p of [
      "/a/x.zip",
      "/a/x.tar.gz",
      "/a/x.wasm",
      "/a/x.dylib",
      "/a/x.woff2",
      "/a/x.mp4",
      "/a/x.pdf",
      "/a/x.sqlite",
      "/a/x.ico",
    ]) {
      expect(isKnownBinaryPath(p)).toBe(true);
      expect(isPreviewableFilePath(p)).toBe(false);
    }
  });

  it("does NOT flag text/code/doc types or unknown text as binary (they preview)", () => {
    for (const p of [
      "/a/readme.md",
      "/a/config.yaml",
      "/a/data.json",
      "/a/app.conf", // unknown text → catch-all → previewable
      "/a/Makefile", // extensionless → previewable
      "/a/shot.png", // image has its own viewer, not a "no-preview" binary
    ]) {
      expect(isKnownBinaryPath(p)).toBe(false);
      expect(isPreviewableFilePath(p)).toBe(true);
    }
  });
});
