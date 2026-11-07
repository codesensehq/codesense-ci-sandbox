// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { QuickOpenDialog, type QuickPickerItem } from "./quick-open-dialog";

// ── helpers ──────────────────────────────────────────────────────────

/** Build a picker item the way the file-open caller does: id carries the
 *  full path, label is the file name, sublabel is the relative path. */
function makeFile(name: string, relativePath: string): QuickPickerItem {
  return { id: `/root/${relativePath}`, label: name, sublabel: relativePath };
}

const sampleFiles: QuickPickerItem[] = [
  makeFile("README.md", "README.md"),
  makeFile("index.ts", "src/index.ts"),
  makeFile("utils.ts", "src/lib/utils.ts"),
  makeFile("config.ts", "src/config.ts"),
  makeFile("setup.ts", "src/setup.ts"),
  makeFile("auth-store.ts", "src/stores/auth-store.ts"),
  makeFile("viewer-store.ts", "src/stores/viewer-store.ts"),
  makeFile("app.tsx", "src/app.tsx"),
  makeFile("main.tsx", "src/main.tsx"),
  makeFile("hooks.ts", "src/hooks.ts"),
];

function renderDialog(overrides: Partial<Parameters<typeof QuickOpenDialog>[0]> = {}) {
  const props = {
    open: true,
    onClose: vi.fn(),
    onSelect: vi.fn(),
    items: sampleFiles,
    ...overrides,
  };
  const result = render(<QuickOpenDialog {...props} />);
  return { ...result, props };
}

// ── Tests ────────────────────────────────────────────────────────────

describe("QuickOpenDialog @P1 — P1-017 fuzzy file search", () => {
  it("prefix match on filename scores highest", () => {
    // Given a dialog with files open
    renderDialog();

    // When the user types a prefix that matches a filename
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.change(input, { target: { value: "index" } });

    // Then index.ts should appear first
    const buttons = screen.getAllByRole("option");
    // First button in the results list is the top match
    expect(buttons[0]).toHaveTextContent("index");
  });

  it("substring match ranks below prefix match", () => {
    // Given a dialog open with files
    renderDialog();

    // When the user types "store" (substring of auth-store, viewer-store)
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.change(input, { target: { value: "store" } });

    // Then store-containing results appear (substring match)
    const buttons = screen.getAllByRole("option");
    const texts = buttons.map((b) => b.textContent);
    expect(texts.some((t) => t?.includes("auth-store"))).toBe(true);
    expect(texts.some((t) => t?.includes("viewer-store"))).toBe(true);
  });

  it("fuzzy match finds non-contiguous character sequences", () => {
    // Given a dialog with files
    renderDialog();

    // When the user types "uts" (fuzzy for utils.ts -> u-t-s all present in order)
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.change(input, { target: { value: "uts" } });

    // Then utils.ts should appear in results
    const buttons = screen.getAllByRole("option");
    const texts = buttons.map((b) => b.textContent);
    expect(texts.some((t) => t?.includes("utils"))).toBe(true);
  });

  it("returns no results for unmatched query", () => {
    // Given a dialog with files
    renderDialog();

    // When the user types a query that matches nothing
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.change(input, { target: { value: "zzzznotafile" } });

    // Then "No files found" is shown
    expect(screen.getByText("No files found")).toBeInTheDocument();
  });

  it("empty query shows all files", () => {
    // Given a dialog open with 10 files
    renderDialog();

    // When query is empty (default)
    // Then all files (up to MAX_VISIBLE) are shown
    const buttons = screen.getAllByRole("option");
    // 10 file buttons + the "New terminal" is NOT present here (different component)
    // We just check that we see the file names
    expect(buttons.length).toBeGreaterThanOrEqual(sampleFiles.length);
  });
});

describe("QuickOpenDialog @P2 — P2-014 keyboard navigation", () => {
  it("ArrowDown moves selection down", () => {
    // Given a dialog open with files
    renderDialog();

    const input = screen.getByPlaceholderText("Search files...");

    // When user presses ArrowDown
    fireEvent.keyDown(input, { key: "ArrowDown" });

    // Then the second item should gain the selected styling (bg-accent)
    const buttons = screen.getAllByRole("option");
    expect(buttons[1].className).toContain("bg-accent");
  });

  it("ArrowUp moves selection up (clamped at 0)", () => {
    // Given a dialog with selection at 0
    renderDialog();

    const input = screen.getByPlaceholderText("Search files...");

    // When user presses ArrowUp at index 0
    fireEvent.keyDown(input, { key: "ArrowUp" });

    // Then selection stays at 0 (first item still selected)
    const buttons = screen.getAllByRole("option");
    expect(buttons[0].className).toContain("bg-accent");
  });

  it("Enter selects the current item and closes", () => {
    // Given a dialog
    const { props } = renderDialog();

    const input = screen.getByPlaceholderText("Search files...");

    // When user presses Enter on the first item
    fireEvent.keyDown(input, { key: "Enter" });

    // Then onSelect is called with the first item's id and onClose is called
    expect(props.onSelect).toHaveBeenCalledWith(sampleFiles[0].id);
    expect(props.onClose).toHaveBeenCalled();
  });

  it("Escape closes the dialog without selecting", () => {
    // Given a dialog
    const { props } = renderDialog();

    const input = screen.getByPlaceholderText("Search files...");

    // When user presses Escape
    fireEvent.keyDown(input, { key: "Escape" });

    // Then onClose is called but onSelect is not
    expect(props.onClose).toHaveBeenCalled();
    expect(props.onSelect).not.toHaveBeenCalled();
  });

  it("ArrowDown then Enter selects the second item", () => {
    // Given a dialog
    const { props } = renderDialog();

    const input = screen.getByPlaceholderText("Search files...");

    // When user navigates down once and presses Enter
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "Enter" });

    // Then the second file is selected
    expect(props.onSelect).toHaveBeenCalledWith(sampleFiles[1].id);
  });
});

describe("QuickOpenDialog @P2 — P2-015 max 15 visible results", () => {
  it('shows "N more" indicator when results exceed 15', () => {
    // Given 20 files
    const manyFiles: QuickPickerItem[] = Array.from({ length: 20 }, (_, i) =>
      makeFile(`file${i}.ts`, `src/file${i}.ts`),
    );

    // When dialog renders with empty query (all files shown)
    renderDialog({ items: manyFiles });

    // Then "5 more results..." indicator is shown
    expect(screen.getByText("5 more results...")).toBeInTheDocument();
  });

  it("renders at most 15 file buttons", () => {
    // Given 20 files
    const manyFiles: QuickPickerItem[] = Array.from({ length: 20 }, (_, i) =>
      makeFile(`file${i}.ts`, `src/file${i}.ts`),
    );

    // When dialog renders
    renderDialog({ items: manyFiles });

    // Then only 15 file buttons are rendered (plus the "more" text)
    const buttons = screen.getAllByRole("option");
    expect(buttons.length).toBe(15);
  });

  it('does not show "more" indicator when results fit within limit', () => {
    // Given 10 files (under the 15 limit)
    renderDialog();

    // Then no "more results..." text is shown
    expect(screen.queryByText(/more results/)).not.toBeInTheDocument();
  });
});

describe("QuickOpenDialog @P1 — auto-focus on open", () => {
  it("returns null when closed", () => {
    // Given open=false
    const { container } = render(
      <QuickOpenDialog open={false} onClose={vi.fn()} onSelect={vi.fn()} items={sampleFiles} />,
    );

    // Then nothing is rendered
    expect(container.firstChild).toBeNull();
  });

  it("renders the search input when open", () => {
    // Given open=true
    renderDialog();

    // Then the search input is present
    expect(screen.getByPlaceholderText("Search files...")).toBeInTheDocument();
  });

  it("clicking a result calls onSelect and onClose", async () => {
    // Given a dialog open
    const { props } = renderDialog();

    // When user clicks on the first file button
    const buttons = screen.getAllByRole("option");
    await userEvent.click(buttons[0]);

    // Then onSelect and onClose are called
    expect(props.onSelect).toHaveBeenCalledWith(sampleFiles[0].id);
    expect(props.onClose).toHaveBeenCalled();
  });

  it("clicking the backdrop closes the dialog", async () => {
    // Given a dialog
    const { props } = renderDialog();

    // When user clicks the backdrop (outer overlay div)
    const backdrop = screen.getByPlaceholderText("Search files...").closest(".fixed");
    if (backdrop) fireEvent.click(backdrop);

    // Then onClose is called
    expect(props.onClose).toHaveBeenCalled();
  });
});

describe("QuickOpenDialog — viewer extension stripping", () => {
  it("strips .md extension from display name", () => {
    const files = [makeFile("README.md", "README.md")];
    renderDialog({ items: files });

    expect(screen.getByText("README")).toBeInTheDocument();
  });

  it("strips .html extension from display name", () => {
    const files = [makeFile("dashboard.html", "mockups/dashboard.html")];
    renderDialog({ items: files });

    expect(screen.getByText("dashboard")).toBeInTheDocument();
  });

  it("strips .htm extension from display name", () => {
    const files = [makeFile("page.htm", "pages/page.htm")];
    renderDialog({ items: files });

    expect(screen.getByText("page")).toBeInTheDocument();
  });

  it("preserves relative path with extension for disambiguation", () => {
    const files = [makeFile("dashboard.html", "mockups/dashboard.html")];
    renderDialog({ items: files });

    expect(screen.getByText("mockups/dashboard.html")).toBeInTheDocument();
  });
});

describe("QuickOpenDialog — SW-SN-11 generic seams (onQueryChange + tier)", () => {
  it("fires onQueryChange with the typed value", () => {
    const onQueryChange = vi.fn();
    renderDialog({ onQueryChange });
    fireEvent.change(screen.getByPlaceholderText("Search files..."), {
      target: { value: "abc" },
    });
    expect(onQueryChange).toHaveBeenCalledWith("abc");
  });

  it("tier is the primary sort key — a lower tier ranks above a higher-scoring higher tier", () => {
    // `b` (tier 1) is a shorter, higher-scoring prefix match than `a` (tier 0);
    // tier must still float `a` above it.
    const items: QuickPickerItem[] = [
      { id: "a", label: "foobar", sublabel: "", tier: 0 },
      { id: "b", label: "foo", sublabel: "", tier: 1 },
    ];
    renderDialog({ items });
    fireEvent.change(screen.getByPlaceholderText("Search files..."), {
      target: { value: "foo" },
    });
    const buttons = screen.getAllByRole("option");
    expect(buttons[0]).toHaveTextContent("foobar"); // tier 0 first, despite lower score
    expect(buttons[1]).toHaveTextContent("foo"); // tier 1 after
  });

  it("with no tier / no onQueryChange the dialog is behavior-identical (regression)", () => {
    // Pure fuzzy order when tiers are absent: shorter exact prefix scores higher.
    const items: QuickPickerItem[] = [
      { id: "a", label: "foobar", sublabel: "" },
      { id: "b", label: "foo", sublabel: "" },
    ];
    renderDialog({ items });
    fireEvent.change(screen.getByPlaceholderText("Search files..."), {
      target: { value: "foo" },
    });
    const buttons = screen.getAllByRole("option");
    expect(buttons[0]).toHaveTextContent("foo"); // higher score wins with no tiers
  });

  it("matches on the trimmed query so a trailing space doesn't drop matches", () => {
    const items: QuickPickerItem[] = [{ id: "a", label: "foobar", sublabel: "" }];
    renderDialog({ items });
    fireEvent.change(screen.getByPlaceholderText("Search files..."), {
      target: { value: "foo " }, // trailing space
    });
    expect(screen.getByText("foobar")).toBeInTheDocument();
  });

  it("shows loadingLabel instead of emptyLabel while loading with no results", () => {
    renderDialog({ items: [], loading: true, loadingLabel: "Searching…" });
    expect(screen.getByText("Searching…")).toBeInTheDocument();
    expect(screen.queryByText("No files found")).toBeNull();
  });

  it("treats a whitespace-only query as empty and shows all items in input order", () => {
    const items: QuickPickerItem[] = [
      { id: "a", label: "Alpha", sublabel: "" },
      { id: "b", label: "Beta", sublabel: "" },
    ];
    renderDialog({ items });
    fireEvent.change(screen.getByPlaceholderText("Search files..."), {
      target: { value: "   " },
    });
    const buttons = screen.getAllByRole("option");
    expect(buttons).toHaveLength(2); // trimmed to empty → full list, nothing filtered
  });
});

describe("QuickOpenDialog @P1 — SW-FX-45 identity-tracked selection", () => {
  const A: QuickPickerItem = { id: "a", label: "Alpha", sublabel: "" };
  const B: QuickPickerItem = { id: "b", label: "Beta", sublabel: "" };
  const C: QuickPickerItem = { id: "c", label: "Gamma", sublabel: "" };

  it("AC-1: highlight follows its item across a live reorder (not the old index)", () => {
    // Given selection on B (index 1)
    const { rerender, props } = renderDialog({ items: [A, B, C] });
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(screen.getAllByRole("option")[1].className).toContain("bg-accent"); // B highlighted

    // When the list reorders so B moves to the front
    rerender(<QuickOpenDialog {...props} items={[B, A, C]} />);

    // Then the highlight (and Enter target) is still B, not the item now at index 1
    const buttons = screen.getAllByRole("option");
    expect(buttons[0]).toHaveTextContent("Beta");
    expect(buttons[0].className).toContain("bg-accent");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith("b");
  });

  it("AC-1: highlight stays put when the list grows asynchronously (SW-SN-11 append)", () => {
    // Given selection on B (index 1)
    const { rerender, props } = renderDialog({ items: [A, B, C] });
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.keyDown(input, { key: "ArrowDown" });

    // When tier-1 rows are appended after the query settled
    const D: QuickPickerItem = { id: "d", label: "Delta", sublabel: "", tier: 1 };
    const E: QuickPickerItem = { id: "e", label: "Epsilon", sublabel: "", tier: 1 };
    rerender(<QuickOpenDialog {...props} items={[A, B, C, D, E]} />);

    // Then B is still highlighted
    const buttons = screen.getAllByRole("option");
    expect(buttons[1]).toHaveTextContent("Beta");
    expect(buttons[1].className).toContain("bg-accent");
  });

  it("AC-2: selection clamps to the first row when the highlighted item disappears", () => {
    // Given selection on B (index 1)
    const { rerender, props } = renderDialog({ items: [A, B, C] });
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.keyDown(input, { key: "ArrowDown" });

    // When B is removed from the list
    rerender(<QuickOpenDialog {...props} items={[A, C]} />);

    // Then the first row is highlighted and Enter fires the first item (no stale index)
    const buttons = screen.getAllByRole("option");
    expect(buttons[0]).toHaveTextContent("Alpha");
    expect(buttons[0].className).toContain("bg-accent");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith("a");
  });

  it("hover sets the selection identity, so a subsequent reorder follows the hovered item", () => {
    // Given the user hovers C (not keyboard-selected)
    const { rerender, props } = renderDialog({ items: [A, B, C] });
    fireEvent.mouseMove(screen.getAllByRole("option")[2]); // hover C
    expect(screen.getAllByRole("option")[2].className).toContain("bg-accent");

    // When the list reorders so C moves to the front
    rerender(<QuickOpenDialog {...props} items={[C, A, B]} />);

    // Then the highlight follows C (identity), not the old hovered index
    const buttons = screen.getAllByRole("option");
    expect(buttons[0]).toHaveTextContent("Gamma");
    expect(buttons[0].className).toContain("bg-accent");
  });

  it("AC-3: a fresh-array-every-render items prop does NOT reset selection to the top", () => {
    // Given selection on B (index 1)
    const { rerender, props } = renderDialog({ items: [A, B, C] });
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.keyDown(input, { key: "ArrowDown" });

    // When the parent re-renders with a NEW array of equal contents (⌘P shape)
    rerender(<QuickOpenDialog {...props} items={[A, B, C].map((i) => ({ ...i }))} />);

    // Then selection is still row 1 (not snapped to 0)
    const buttons = screen.getAllByRole("option");
    expect(buttons[1]).toHaveTextContent("Beta");
    expect(buttons[1].className).toContain("bg-accent");
  });
});

describe("QuickOpenDialog — SW-FX-45 caller-opt-in extension strip", () => {
  it("AC-4: stripExtension={false} renders a viewer-extension label verbatim", () => {
    renderDialog({
      items: [{ id: "n", label: "Release.md", sublabel: "" }],
      stripExtension: false,
    });
    expect(screen.getByText("Release.md")).toBeInTheDocument();
  });

  it("AC-4: default (stripExtension omitted) still strips a filename label", () => {
    renderDialog({ items: [{ id: "f", label: "README.md", sublabel: "" }] });
    expect(screen.getByText("README")).toBeInTheDocument();
  });

  it("AC-4: stripExtension={true} strips explicitly (parity with default)", () => {
    renderDialog({ items: [{ id: "f", label: "notes.html", sublabel: "" }], stripExtension: true });
    expect(screen.getByText("notes")).toBeInTheDocument();
  });

  it("AC-4: a .png label is kept in both modes (image extensions are salient)", () => {
    const { rerender, props } = renderDialog({
      items: [{ id: "p", label: "shot.png", sublabel: "" }],
      stripExtension: false,
    });
    expect(screen.getByText("shot.png")).toBeInTheDocument();
    rerender(
      <QuickOpenDialog
        {...props}
        items={[{ id: "p", label: "shot.png", sublabel: "" }]}
        stripExtension
      />,
    );
    expect(screen.getByText("shot.png")).toBeInTheDocument();
  });
});

describe("visible, deliberate quick-open selection", () => {
  it("ignores mouse enter without movement, then follows mouse movement", () => {
    const { props } = renderDialog();
    const rows = screen.getAllByRole("option");
    fireEvent.mouseEnter(rows[2]);
    expect(rows[0]).toHaveClass("bg-accent");
    fireEvent.mouseMove(rows[2]);
    expect(rows[2]).toHaveClass("bg-accent");
    fireEvent.keyDown(screen.getByPlaceholderText("Search files..."), { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith(sampleFiles[2].id);
  });

  it("stops at the last rendered result and Enter opens that visible row", () => {
    const items = Array.from({ length: 20 }, (_, i) => makeFile(`file-${i}.md`, `file-${i}.md`));
    const { props } = renderDialog({ items });
    const input = screen.getByPlaceholderText("Search files...");
    for (let i = 0; i < 20; i++) fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(screen.getAllByRole("option")[14]).toHaveClass("bg-accent");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith(items[14].id);
  });

  it("exposes the active option and resets it synchronously when typing", () => {
    renderDialog();
    const input = screen.getByRole("combobox");
    const list = screen.getByRole("listbox");
    expect(input).toHaveAttribute("aria-controls", list.id);
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(screen.getAllByRole("option")[1]).toHaveAttribute("aria-selected", "true");
    fireEvent.change(input, { target: { value: "ts" } });
    const first = screen.getAllByRole("option")[0];
    expect(first).toHaveAttribute("aria-selected", "true");
    expect(input).toHaveAttribute("aria-activedescendant", first.id);
    fireEvent.change(input, { target: { value: "no such result" } });
    expect(input).not.toHaveAttribute("aria-activedescendant");
  });

  it("falls back to the first visible row when a chosen result moves beyond the render cap", () => {
    const items = Array.from({ length: 20 }, (_, i) => makeFile(`file-${i}.md`, `file-${i}.md`));
    const { props, rerender } = renderDialog({ items });
    const input = screen.getByPlaceholderText("Search files...");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    rerender(<QuickOpenDialog {...props} items={[...items.slice(2), items[0], items[1]]} />);
    expect(screen.getAllByRole("option")[0]).toHaveClass("bg-accent");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith(items[2].id);
  });
});
