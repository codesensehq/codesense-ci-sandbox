// SPDX-License-Identifier: MIT
import { afterEach, describe, expect, it, vi } from "vitest";

import { luminance, readDocTheme } from "./doc-theme";

function frameWith(props: Record<string, string>): Element {
  const el = document.createElement("iframe");
  vi.spyOn(window, "getComputedStyle").mockReturnValue({
    getPropertyValue: (p: string) => props[p] ?? "",
  } as CSSStyleDeclaration);
  return el;
}

afterEach(() => vi.restoreAllMocks());

describe("doc theme", () => {
  it("reads luminance from hex and rgb colours", () => {
    expect(luminance("#fff")).toBeCloseTo(1);
    expect(luminance("rgb(0, 0, 0)")).toBe(0);
    expect(luminance("oklch(0.2 0 0)")).toBeNull();
  });

  it("on a viewer-themed surface the scheme and accent follow that surface, not the app chrome", () => {
    // A light app with a dark viewer theme: the right pane is dark.
    const theme = readDocTheme(
      frameWith({
        "--cs-viewer-bg": "#282a36",
        color: "rgb(248, 248, 242)",
        "--tw-prose-headings": "#bd93f9",
        "--primary": "#1f2328",
        "--primary-foreground": "#ffffff",
      }),
    );
    expect(theme.scheme).toBe("dark");
    expect(theme.tokens["--cs-bg"]).toBe("#282a36");
    expect(theme.tokens["--cs-fg"]).toBe("rgb(248, 248, 242)");
    expect(theme.tokens["--cs-accent"]).toBe("#bd93f9");
    expect(theme.tokens["--cs-accent-fg"]).toBe("#282a36");
  });

  it("with no viewer theme the app's own tokens apply", () => {
    const theme = readDocTheme(
      frameWith({
        "--background": "#ffffff",
        color: "#1f2328",
        "--primary": "#0969da",
        "--primary-foreground": "#ffffff",
        "--tw-prose-headings": "#000000",
      }),
    );
    expect(theme.scheme).toBe("light");
    expect(theme.tokens["--cs-bg"]).toBe("#ffffff");
    expect(theme.tokens["--cs-accent"]).toBe("#0969da");
    expect(theme.tokens["--cs-accent-fg"]).toBe("#ffffff");
    expect(theme.tokens["--cs-danger"]).toBeUndefined();
  });
});

it("pushes editor highlight and code theme tokens without changing the button accent", () => {
  const theme = readDocTheme(
    frameWith({
      "--background": "#fff",
      "--primary": "#111",
      "--accent": "#0969da",
      "--cs-code-comment": "#666",
      "--cs-code-keyword": "#f00",
      "--cs-code-string": "#00f",
    }),
  );
  expect(theme.tokens).toMatchObject({
    "--cs-accent": "#111",
    "--cs-highlight": "#0969da",
    "--cs-code-comment": "#666",
    "--cs-code-keyword": "#f00",
    "--cs-code-string": "#00f",
  });
});

it("keeps light viewer prose neutral in a light app and keeps viewer ink in a dark app", () => {
  const props = { "--cs-viewer-bg": "#fff", "--foreground": "#1f2328", color: "#31435b" };
  expect(readDocTheme(frameWith(props)).tokens["--cs-fg"]).toBe("#1f2328");
  document.documentElement.classList.add("dark");
  try {
    expect(readDocTheme(frameWith({ ...props, "--foreground": "#fff" })).tokens["--cs-fg"]).toBe(
      "#31435b",
    );
  } finally {
    document.documentElement.classList.remove("dark");
  }
});

it("pushes the sidebar colours and the theme's management-tree accents", async () => {
  const { MANAGED_TREE_ACCENTS, getManagedTreeAccent } =
    await import("@/shared/lib/pipeline-graph/epic-colors");
  const dark = readDocTheme(
    frameWith({ "--background": "#000", "--cs-sidebar-bg": "#111", "--cs-sidebar-text": "#eee" }),
  );
  expect(dark.tokens).toMatchObject({ "--cs-sidebar-bg": "#111", "--cs-sidebar-fg": "#eee" });
  MANAGED_TREE_ACCENTS.dark.forEach((color, i) =>
    expect(dark.tokens[`--cs-tree-accent-${i + 1}`]).toBe(color),
  );
  expect(dark.tokens[`--cs-tree-accent-${MANAGED_TREE_ACCENTS.dark.length + 1}`]).toBeUndefined();
  // A page picks `hash(teamKey|treeHandle) % n + 1`, the built-in list's rule.
  expect(Object.values(dark.tokens)).toContain(getManagedTreeAccent("acme:a|lead", false));
  const light = readDocTheme(frameWith({ "--background": "#fff" }));
  expect(light.tokens["--cs-tree-accent-1"]).toBe(MANAGED_TREE_ACCENTS.light[0]);
});

it("pushes the native removed-line color independently of destructive actions", () => {
  const theme = readDocTheme(
    frameWith({ "--cs-status-deleted": "#cf222e", "--destructive": "#f85149" }),
  );
  expect(theme.tokens["--cs-deleted"]).toBe("#cf222e");
  expect(theme.tokens["--cs-danger"]).toBe("#f85149");
});

it("resolves surface-free names at the frame's nearest surface", async () => {
  const { themeTokens } = await import("@/themes");
  const t = themeTokens("nord", "light");
  const frame = frameWith(t);
  const wrapper = document.createElement("div");
  wrapper.dataset.surface = "sidebar";
  wrapper.append(frame);
  const theme = readDocTheme(frame);
  expect(theme.scheme).toBe("light");
  expect(theme.tokens["--cs-bg"]).toBe(t["--theme-surface-sidebar-bg"]);
  expect(theme.tokens["--cs-fg"]).toBe(t["--theme-text-sidebar-primary"]);
  expect(theme.tokens["--cs-warning"]).toBe(t["--theme-severitySurface-sidebar-warning-fg"]);
  expect(theme.tokens["--cs-story-done"]).toBe(t["--theme-severity-success-fg"]);
});
