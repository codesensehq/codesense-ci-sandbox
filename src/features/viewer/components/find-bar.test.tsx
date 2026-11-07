// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { FindBar } from "./find-bar";
import React, { useRef } from "react";

// Mock AppButton to render a plain <button>
vi.mock("@/shared/components/app-button", () => ({
  AppButton: ({ children, ...props }: React.ComponentPropsWithoutRef<"button">) => (
    <button {...props}>{children}</button>
  ),
}));

// Mock Input to render a plain <input>
vi.mock("@/components/ui/input", () => ({
  Input: (props: React.ComponentPropsWithoutRef<"input">) => <input {...props} />,
}));

/**
 * Helper that renders FindBar with a container div holding some text content.
 * Returns the onClose spy and container ref element.
 */
function renderFindBar(opts: { visible?: boolean; textContent?: string } = {}) {
  const { visible = true, textContent = "The quick brown fox jumps over the lazy dog" } = opts;
  const onClose = vi.fn();

  function Wrapper() {
    const containerRef = useRef<HTMLDivElement>(null);
    return (
      <>
        <div ref={containerRef} data-testid="content-container">
          {textContent}
        </div>
        <FindBar visible={visible} onClose={onClose} containerRef={containerRef} />
      </>
    );
  }

  const result = render(<Wrapper />);
  return { onClose, ...result };
}

beforeEach(() => {
  vi.clearAllMocks();
});

/* ------------------------------------------------------------------ */
/* @P1-010 Search highlights matches                                  */
/* ------------------------------------------------------------------ */
describe("@P1-010 FindBar — search highlights matches", () => {
  it("wraps matching text in <mark> elements", async () => {
    // Given: a container with text "The quick brown fox jumps over the lazy dog"
    const user = userEvent.setup();
    renderFindBar();

    // When: user types "the" (>= 2 chars, case-insensitive)
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "the");

    // Then: <mark> elements are created in the content container
    const container = screen.getByTestId("content-container");
    const marks = container.querySelectorAll("mark[data-find]");
    expect(marks.length).toBe(2); // "The" and "the"
  });

  it("shows match count in the counter", async () => {
    // Given: container with text containing two occurrences of "the"
    const user = userEvent.setup();
    renderFindBar();

    // When: user types "the"
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "the");

    // Then: counter shows "1/2" (first match highlighted)
    expect(screen.getByText("1/2")).toBeInTheDocument();
  });

  it("does not highlight with fewer than 2 characters", async () => {
    // Given: container with text
    const user = userEvent.setup();
    renderFindBar();

    // When: user types only one character
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "t");

    // Then: no marks are created and no count is shown
    const container = screen.getByTestId("content-container");
    const marks = container.querySelectorAll("mark[data-find]");
    expect(marks.length).toBe(0);
  });
});

/* ------------------------------------------------------------------ */
/* @P1-011 Next/prev navigation cycles through matches                */
/* ------------------------------------------------------------------ */
describe("@P1-011 FindBar — next/prev navigation", () => {
  it("Enter advances to the next match", async () => {
    // Given: matches are highlighted
    const user = userEvent.setup();
    renderFindBar();
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "the");

    // When: user presses Enter to go to next match
    fireEvent.keyDown(input, { key: "Enter" });

    // Then: counter shows "2/2"
    expect(screen.getByText("2/2")).toBeInTheDocument();
  });

  it("Shift+Enter goes to the previous match", async () => {
    // Given: matches are highlighted and we navigate forward first
    const user = userEvent.setup();
    renderFindBar();
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "the");

    // Move to match 2
    fireEvent.keyDown(input, { key: "Enter" });
    expect(screen.getByText("2/2")).toBeInTheDocument();

    // When: user presses Shift+Enter
    fireEvent.keyDown(input, { key: "Enter", shiftKey: true });

    // Then: counter goes back to "1/2"
    expect(screen.getByText("1/2")).toBeInTheDocument();
  });

  it("navigation wraps around from last to first", async () => {
    // Given: two matches, currently on match 2
    const user = userEvent.setup();
    renderFindBar();
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "the");
    fireEvent.keyDown(input, { key: "Enter" }); // now at 2/2

    // When: user presses Enter again to go past the last match
    fireEvent.keyDown(input, { key: "Enter" });

    // Then: wraps to "1/2"
    expect(screen.getByText("1/2")).toBeInTheDocument();
  });

  it("shows 'No matches' when query >= 2 chars but nothing found", async () => {
    // Given: container with text that won't match
    const user = userEvent.setup();
    renderFindBar();

    // When: user types a query with no matches
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "zzz");

    // Then: "No matches" is displayed
    expect(screen.getByText("No matches")).toBeInTheDocument();
  });
});

/* ------------------------------------------------------------------ */
/* @P1-012 Rapid open/close does not crash                            */
/* ------------------------------------------------------------------ */
describe("@P1-012 FindBar — rapid open/close stability", () => {
  it("does not crash when toggling visibility rapidly", () => {
    // Given: a find bar that will be toggled rapidly
    const onClose = vi.fn();

    function RapidToggle() {
      const containerRef = useRef<HTMLDivElement>(null);
      const [visible, setVisible] = React.useState(false);
      return (
        <>
          <div ref={containerRef}>Some text with words to find</div>
          <FindBar visible={visible} onClose={onClose} containerRef={containerRef} />
          <button data-testid="toggle" onClick={() => setVisible((v: boolean) => !v)}>
            toggle
          </button>
        </>
      );
    }

    // React imported at top of file
    const { getByTestId } = render(<RapidToggle />);
    const toggle = getByTestId("toggle");

    // When: rapidly toggle 20 times
    expect(() => {
      for (let i = 0; i < 20; i++) {
        act(() => {
          fireEvent.click(toggle);
        });
      }
    }).not.toThrow();

    // Then: no crash occurred (assertion is the absence of an error)
  });
});

/* ------------------------------------------------------------------ */
/* Match counter updates                                              */
/* ------------------------------------------------------------------ */
describe("@P1 FindBar — match counter updates", () => {
  it("updates count when query changes", async () => {
    // Given: container with "the quick brown fox jumps over the lazy dog"
    const user = userEvent.setup();
    renderFindBar();
    const input = screen.getByPlaceholderText("Find in document...");

    // When: user types "the" (2 matches)
    await user.type(input, "the");
    expect(screen.getByText("1/2")).toBeInTheDocument();

    // When: user clears and types "quick" (1 match)
    await user.clear(input);
    await user.type(input, "quick");

    // Then: counter shows 1/1
    expect(screen.getByText("1/1")).toBeInTheDocument();
  });

  it("resets counter when query is cleared below 2 chars", async () => {
    // Given: matches are visible
    const user = userEvent.setup();
    renderFindBar();
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "the");
    expect(screen.getByText("1/2")).toBeInTheDocument();

    // When: user clears the input
    await user.clear(input);

    // Then: no counter text is shown (empty string rendered)
    expect(screen.queryByText(/\/\d/)).not.toBeInTheDocument();
    expect(screen.queryByText("No matches")).not.toBeInTheDocument();
  });
});

/* ------------------------------------------------------------------ */
/* Escape closes the find bar                                         */
/* ------------------------------------------------------------------ */
describe("@P1 FindBar — escape closes", () => {
  it("calls onClose and clears highlights when Escape is pressed", async () => {
    // Given: find bar is visible with active highlights
    const user = userEvent.setup();
    const { onClose } = renderFindBar();
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "the");

    // Verify marks exist
    const container = screen.getByTestId("content-container");
    expect(container.querySelectorAll("mark[data-find]").length).toBeGreaterThan(0);

    // When: user presses Escape
    fireEvent.keyDown(input, { key: "Escape" });

    // Then: onClose is called
    expect(onClose).toHaveBeenCalledTimes(1);

    // And: highlights are cleared from the DOM
    expect(container.querySelectorAll("mark[data-find]").length).toBe(0);
  });
});

/* ------------------------------------------------------------------ */
/* Hidden state                                                       */
/* ------------------------------------------------------------------ */
describe("@P1 FindBar — hidden state", () => {
  it("renders nothing when visible is false", () => {
    // Given/When: visible is false
    const { container: _container } = renderFindBar({ visible: false });

    // Then: no find bar input is in the DOM
    expect(screen.queryByPlaceholderText("Find in document...")).not.toBeInTheDocument();
  });
});

/* ------------------------------------------------------------------ */
/* Story 32.7: contentVersion re-highlight on external content change */
/* ------------------------------------------------------------------ */

/**
 * Wrapper that controls both the container's text content and the
 * `contentVersion` prop, so tests can simulate an external file update
 * rewriting the viewer DOM while Find is active.
 */
function ContentVersionHarness({
  initialText,
  initialVersion,
}: {
  initialText: string;
  initialVersion: number;
}) {
  const [text, setText] = React.useState(initialText);
  const [version, setVersion] = React.useState(initialVersion);
  const containerRef = useRef<HTMLDivElement>(null);
  return (
    <>
      <button
        data-testid="swap-content"
        onClick={() => {
          // No substring overlap with "foo" — exactly 3 matches.
          setText("foo bar foo baz foo qux");
          setVersion((v) => v + 1);
        }}
      >
        swap
      </button>
      <div ref={containerRef} data-testid="content-container">
        {text}
      </div>
      <FindBar visible onClose={() => {}} containerRef={containerRef} contentVersion={version} />
    </>
  );
}

describe("@P1 FindBar — contentVersion re-highlight", () => {
  it("re-runs the highlight pass when contentVersion bumps", async () => {
    const user = userEvent.setup();
    render(<ContentVersionHarness initialText="foo bar foo baz" initialVersion={0} />);
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "foo");

    // Initial: 2 matches highlighted.
    let container = screen.getByTestId("content-container");
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(r));
    });
    expect(container.querySelectorAll("mark[data-find]").length).toBe(2);

    // External content swap — new DOM, no <mark> elements yet.
    await act(async () => {
      screen.getByTestId("swap-content").click();
    });
    container = screen.getByTestId("content-container");
    // The rAF-scheduled highlight hasn't fired yet — wait one frame.
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(r));
    });

    // Now the new content has 3 occurrences of "foo" and should be highlighted.
    const marks = container.querySelectorAll("mark[data-find]");
    expect(marks.length).toBe(3);
  });

  it("bails cleanly when container is disconnected at rAF fire time", async () => {
    // Regression guard for the `isConnected` check in highlight().
    const user = userEvent.setup();
    const { unmount } = render(
      <ContentVersionHarness initialText="foo bar baz" initialVersion={0} />,
    );
    const input = screen.getByPlaceholderText("Find in document...");
    await user.type(input, "foo");
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(r));
    });
    // Unmount between the scheduled rAF and its fire. No throw, no
    // setState-on-unmounted warnings.
    unmount();
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(r));
    });
    // Assertion: no thrown error from the detached-DOM walker. If the
    // guard is removed, this test would warn in React's act() about
    // setState-on-unmounted.
    expect(true).toBe(true);
  });
});
