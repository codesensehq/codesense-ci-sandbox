// SPDX-License-Identifier: MIT
import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import { act, render, screen, cleanup, fireEvent } from "@testing-library/react";

import type { ContextRepoStatus } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { resetContextGitStoreForTests, useContextGitStore } from "../store/use-context-git-store";
import type { NavChange } from "../lib/nav-changes";
import { DiffSideStrip, formatLineSummary } from "./diff-side-strip";
import { DEFAULT_NAV_LENS } from "@/shared/lib/nav-lens";

const TEAM = "default";
const FILE = "/org/default/content/a.md";

function seedStatus(over: Partial<ContextRepoStatus> = {}): void {
  const status: ContextRepoStatus = {
    branch: "main",
    headSha: "aaaaaaa",
    hasUpstream: true,
    upstream: "origin/main",
    remoteProvider: "github",
    ahead: 0,
    behind: 0,
    sync: { state: "ok", lastSyncedAt: null },
    conflicts: [],
    ...over,
  };
  useContextGitStore.getState().put(TEAM, {
    orgRoot: "/org",
    changes: [],
    byPath: new Map(),
    status,
    loading: false,
    historyMoves: 0,
  });
}

function change(over: Partial<NavChange> = {}): NavChange {
  return {
    absPath: FILE,
    relPath: "default/content/a.md",
    kind: "modified",
    added: 87,
    removed: 12,
    ...over,
  };
}

describe("formatLineSummary (SW-VE-16 AC-3)", () => {
  it("emits both numbers with the lines unit when both are nonzero", () => {
    expect(formatLineSummary(87, 12)).toBe("+87 −12 lines");
  });

  it("omits the removed side when it is zero", () => {
    expect(formatLineSummary(87, 0)).toBe("+87 lines");
  });

  it("omits the added side when it is zero", () => {
    expect(formatLineSummary(0, 12)).toBe("−12 lines");
  });

  it("reads '0 changes' when both sides are zero (binary / rename-only)", () => {
    expect(formatLineSummary(0, 0)).toBe("0 changes");
    expect(formatLineSummary(undefined, undefined)).toBe("0 changes");
  });
});

describe("DiffSideStrip remote-label (SW-VE-16 AC-2)", () => {
  afterEach(() => {
    useViewerStore.setState({ navLens: {} });
    resetContextGitStoreForTests();
    cleanup();
  });

  it.each(["clone", "private"] as const)(
    "shows a private file's comparison only when the nav shows Private (nav=%s)",
    (kind) => {
      seedStatus();
      useViewerStore.setState({ navLens: { [TEAM]: { ...DEFAULT_NAV_LENS, source: { kind } } } });
      render(
        <DiffSideStrip
          teamKey={TEAM}
          filePath="/home/users/42/scratch/orgs/acme/plans/content/a.md"
          change={undefined}
          diff={true}
        />,
      );
      if (kind === "clone") {
        expect(screen.queryByTestId("diff-side-strip")).toBeNull();
        return;
      }
      const text = screen.getByTestId("diff-side-strip").textContent ?? "";
      expect(text).toContain("vs HEAD · local only");
      expect(text).not.toMatch(/origin|GitHub/);
    },
  );

  it("hides a clone comparison in Private and reads fresh labels after switching back", () => {
    seedStatus({ remoteProvider: "gitlab", branch: "feat/plans", upstream: "origin/feat/plans" });
    const { rerender } = render(
      <DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={true} />,
    );
    const cloneSnapshot = useContextGitStore.getState().byTeam[TEAM];
    act(() => {
      useContextGitStore.getState().put(TEAM, {
        ...cloneSnapshot,
        orgRoot: "/home/users/42/scratch",
        source: { kind: "private" },
        status: {
          ...cloneSnapshot.status!,
          branch: "private-branch",
          upstream: null,
          hasUpstream: false,
          remoteProvider: null,
        },
      });
      useViewerStore.setState({
        navLens: { [TEAM]: { ...DEFAULT_NAV_LENS, source: { kind: "private" } } },
      });
    });
    rerender(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={true} />);
    expect(screen.queryByTestId("diff-side-strip")).toBeNull();
    act(() => {
      useViewerStore.setState({ navLens: { [TEAM]: DEFAULT_NAV_LENS } });
      seedStatus({ remoteProvider: "github", branch: "feat/new", upstream: "origin/feat/new" });
    });
    const text = screen.getByTestId("diff-side-strip").textContent ?? "";
    expect(text).toMatch(/vs\s+GitHub\s+origin\/feat\/new/);
    expect(text).not.toContain("feat/plans");
    expect(text).not.toContain("local only");
  });

  it("renders `vs GitHub origin/main` when the origin provider is `github`", () => {
    // SW-VE-16 Clean minimal / IDE-native (Mockup #2) drops the parens around the ref —
    // the mono weight + the muted color is enough to separate it from the host label.
    // R4 N6: ORDER MATTERS — a regression that rendered `vs origin/main GitHub` (ref ↔ host
    // swapped) must fail here, so match the whole phrase in order rather than three
    // independent `.toContain` calls.
    seedStatus({ remoteProvider: "github", upstream: "origin/main" });
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    expect(screen.getByTestId("diff-side-strip").textContent ?? "").toMatch(
      /vs\s+GitHub\s+origin\/main/,
    );
  });

  it("renders `vs GitLab origin/main` when the origin provider is `gitlab`", () => {
    seedStatus({ remoteProvider: "gitlab", upstream: "origin/main" });
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    expect(screen.getByTestId("diff-side-strip").textContent ?? "").toMatch(
      /vs\s+GitLab\s+origin\/main/,
    );
  });

  it("falls back to `vs origin/main` when the remote host is not a recognized forge", () => {
    seedStatus({ remoteProvider: null, upstream: "origin/main" });
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    const text = screen.getByTestId("diff-side-strip").textContent ?? "";
    expect(text).toContain("vs origin/main");
    expect(text).not.toMatch(/GitHub|GitLab|Bitbucket/);
  });

  it("tracks a non-default base: shows `origin/feat/foo` when the branch is stacked", () => {
    seedStatus({ remoteProvider: "github", upstream: "origin/feat/foo", branch: "feat/foo" });
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    expect(screen.getByTestId("diff-side-strip").textContent ?? "").toMatch(
      /vs\s+GitHub\s+origin\/feat\/foo/,
    );
  });

  it("falls back to `vs origin` when the repo has no upstream (unborn / local-only branch)", () => {
    seedStatus({ remoteProvider: null, upstream: null, hasUpstream: false });
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    const text = screen.getByTestId("diff-side-strip").textContent ?? "";
    expect(text).toContain("vs origin");
    expect(text).not.toContain("origin/");
  });

  it("N2: synthesizes `origin/<branch>` when `upstream` is null but `hasUpstream` is true", () => {
    // Rust-side types permit {hasUpstream: true, upstream: null} independently. The toolbar
    // should still name a ref the user can relate to rather than collapsing to bare `origin`.
    seedStatus({
      remoteProvider: "github",
      upstream: null,
      hasUpstream: true,
      branch: "feat/foo",
    });
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    expect(screen.getByTestId("diff-side-strip").textContent ?? "").toMatch(
      /vs\s+GitHub\s+origin\/feat\/foo/,
    );
  });

  it("N7: an unmapped provider id (future forge, drift) collapses to the plain `vs origin/<branch>` fallback", () => {
    // Rust-side `provider_for_host` might add e.g. `gitea` before the FE map learns it. The
    // FE must not leak a lowercase provider leak (`vs gitea (origin/main)`) — collapse to the
    // plain `vs origin/<branch>` baseline we already use for an unrecognized host.
    seedStatus({ remoteProvider: "gitea", upstream: "origin/main" });
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    const text = screen.getByTestId("diff-side-strip").textContent ?? "";
    expect(text).toContain("vs origin/main");
    expect(text).not.toMatch(/gitea|GitHub|GitLab|Bitbucket/);
  });
});

describe("DiffSideStrip summary click (SW-VE-16 AC-3/AC-10)", () => {
  beforeEach(() => {
    seedStatus();
  });

  afterEach(() => {
    resetContextGitStoreForTests();
    cleanup();
  });

  it("renders the readable `+87 −12 lines` summary beside the remote label", () => {
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    expect(screen.getByTestId("diff-side-summary")).toHaveTextContent("+87 −12 lines");
  });

  it("clicking the summary opens the Changes view (same seam as pressing Changes)", () => {
    const setTabDiffSpy = vi.spyOn(useViewerStore.getState(), "setTabDiff");
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    fireEvent.click(screen.getByTestId("diff-side-summary"));
    expect(setTabDiffSpy).toHaveBeenCalledWith(TEAM, FILE, true);
    setTabDiffSpy.mockRestore();
  });

  it("pressing the Changes segment also opens the Changes view", () => {
    const setTabDiffSpy = vi.spyOn(useViewerStore.getState(), "setTabDiff");
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    fireEvent.click(screen.getByTestId("diff-side-changes"));
    expect(setTabDiffSpy).toHaveBeenCalledWith(TEAM, FILE, true);
    setTabDiffSpy.mockRestore();
  });

  it("SW-VE-16 R5 N-R4-2: segment hover resolves to `--ibx-lifted`; track rest-bg uses `--ibx-muted`", () => {
    // Locks the token-palette honesty fix for both symptoms the reviewer caught:
    // - SEG hover was `--ibx-raise` (R4) → invisible in light (equals `--ibx-muted`).
    // - Active-pressed was `--ibx-card` (R4) → sunk below `--ibx-muted` in dark.
    // Both consolidate on `--ibx-lifted`, which sits ABOVE `--ibx-muted` in both themes.
    // The track itself stays `--ibx-muted` (sunken relative to the lifted active pill).
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={change()} diff={null} />);
    const seg = screen.getByTestId("diff-side-file");
    expect(seg.className).toMatch(/hover:bg-\[var\(--ibx-lifted\)\]/);
    expect(seg.className).toMatch(/aria-pressed:bg-\[var\(--ibx-lifted\)\]/);
    const track = seg.parentElement;
    expect(track).not.toBeNull();
    expect((track as HTMLElement).style.getPropertyValue("background-color")).toBe(
      "var(--ibx-muted)",
    );
  });

  it("N1: `0 changes` (binary / rename-only) renders as inert text, NOT a clickable button", () => {
    render(
      <DiffSideStrip
        teamKey={TEAM}
        filePath={FILE}
        change={change({ added: 0, removed: 0 })}
        diff={null}
      />,
    );
    const summary = screen.getByTestId("diff-side-summary");
    expect(summary.tagName).toBe("SPAN");
    expect(summary).toHaveTextContent("0 changes");
    // The aria-label hazard from AC-10's live affordance ("Open changes: 0 changes") must not
    // exist — the inert text has no aria-label at all.
    expect(summary).not.toHaveAttribute("aria-label");
  });
});

describe("DiffSideStrip CommitLine (SW-VE-16 M1 — out-of-scope regression)", () => {
  afterEach(() => {
    resetContextGitStoreForTests();
    cleanup();
  });

  it("hides the trailing count segment on a binary / rename-only commit (preserves pre-SW-VE-16 behavior)", () => {
    // CommitLine was never in scope for the "+87 → +87 −12 lines" refresh — only the
    // vs-remote summary beside the File/Changes switch was. A History-selected
    // binary/rename-only commit tab must NOT now read `· 0 changes` next to the author.
    seedStatus();
    const commitDiff = {
      sha: "aaaaaaa1111111",
      shortSha: "aaaaaaa",
      pr: 23,
      subject: "Rename roadmap",
      who: "maya",
      committedAt: Date.now() - 5 * 60_000,
      added: 0,
      removed: 0,
      showFile: false,
    } as const;
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={undefined} diff={commitDiff} />);
    const commit = screen.getByTestId("diff-side-commit");
    expect(commit).toHaveTextContent("#23 · Rename roadmap");
    expect(commit).toHaveTextContent("maya");
    expect(commit.textContent).not.toContain("0 changes");
  });

  it("still renders the trailing `+N −M lines` segment on a normal commit", () => {
    seedStatus();
    const commitDiff = {
      sha: "bbbbbbb2222222",
      shortSha: "bbbbbbb",
      pr: 23,
      subject: "Migrate dashboards",
      who: "maya",
      committedAt: Date.now() - 5 * 60_000,
      added: 12,
      removed: 3,
      showFile: false,
    } as const;
    render(<DiffSideStrip teamKey={TEAM} filePath={FILE} change={undefined} diff={commitDiff} />);
    expect(screen.getByTestId("diff-side-commit")).toHaveTextContent("+12 −3 lines");
  });
});
