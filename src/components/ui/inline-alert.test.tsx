// SPDX-License-Identifier: MIT
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { InlineAlert } from "./inline-alert";

describe("InlineAlert", () => {
  it("renders the message inside a role=alert region", () => {
    render(<InlineAlert severity="warning">Heads up</InlineAlert>);
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Heads up");
  });

  it("applies warning (amber) tokens", () => {
    render(
      <InlineAlert severity="warning" data-testid="a">
        w
      </InlineAlert>,
    );
    const el = screen.getByTestId("a");
    expect(el).toHaveAttribute("data-severity", "warning");
    expect(el.className).toContain("bg-amber-500/10");
    expect(el.className).not.toContain("border-destructive");
  });

  it("applies error (destructive) tokens", () => {
    render(
      <InlineAlert severity="error" data-testid="e">
        e
      </InlineAlert>,
    );
    const el = screen.getByTestId("e");
    expect(el).toHaveAttribute("data-severity", "error");
    expect(el.className).toContain("bg-destructive/10");
    expect(el.className).toContain("border-destructive/30");
  });

  it("defaults to the warning severity", () => {
    render(<InlineAlert data-testid="d">x</InlineAlert>);
    expect(screen.getByTestId("d")).toHaveAttribute("data-severity", "warning");
  });

  it("forwards data-testid and extra props to the container", () => {
    render(
      <InlineAlert severity="warning" data-testid="passthrough" id="my-alert">
        x
      </InlineAlert>,
    );
    expect(screen.getByTestId("passthrough")).toHaveAttribute("id", "my-alert");
  });

  it("renders a trailing action alongside the message when provided", () => {
    render(
      <InlineAlert severity="warning" data-testid="withaction" action={<button>Do it</button>}>
        Heads up
      </InlineAlert>,
    );
    const el = screen.getByTestId("withaction");
    expect(el).toHaveTextContent("Heads up");
    expect(screen.getByRole("button", { name: "Do it" })).toBeInTheDocument();
  });

  it("gives a warning's structured outline action its own foreground", () => {
    render(<InlineAlert action={{ label: "Dismiss", onClick: vi.fn() }}>Heads up</InlineAlert>);
    expect(screen.getByRole("button", { name: "Dismiss" })).toHaveClass("text-foreground");
  });

  it("renders a decorative icon by default and omits it when hideIcon is set", () => {
    const { rerender } = render(
      <InlineAlert severity="warning" data-testid="withicon">
        x
      </InlineAlert>,
    );
    // lucide renders an <svg> with aria-hidden
    expect(screen.getByTestId("withicon").querySelector("svg")).not.toBeNull();

    rerender(
      <InlineAlert severity="warning" data-testid="withicon" hideIcon>
        x
      </InlineAlert>,
    );
    expect(screen.getByTestId("withicon").querySelector("svg")).toBeNull();
  });

  it("says what failed, then the reason in words", () => {
    render(
      <InlineAlert severity="error" title="Couldn't join Acme" data-testid="t">
        A different repo already exists there.
      </InlineAlert>,
    );
    const el = screen.getByTestId("t");
    expect(el.querySelector('[data-slot="inline-alert-title"]')).toHaveTextContent(
      "Couldn't join Acme",
    );
    expect(el).toHaveTextContent("A different repo already exists there.");
  });

  it("reads the reason from a thrown error through the user-facing path, never raw", () => {
    render(
      <InlineAlert
        severity="error"
        title="Couldn't remove acme/web"
        error={new Error("denied: you don't have write access")}
        data-testid="err"
      />,
    );
    const el = screen.getByTestId("err");
    expect(el).toHaveTextContent("you don't have write access");
    expect(el).not.toHaveTextContent("denied:");
    expect(el).not.toHaveTextContent("Error:");
  });

  it("renders a structured next action as the shared outline button", async () => {
    const onClick = vi.fn();
    render(
      <InlineAlert
        severity="error"
        title="Couldn't load the Inbox."
        action={{ label: "Retry", onClick, testId: "retry" }}
      />,
    );
    const button = screen.getByTestId("retry");
    expect(button).toHaveAttribute("data-slot", "button");
    expect(button).toHaveAttribute("data-variant", "outline");
    expect(button).not.toHaveClass("text-foreground");
    await userEvent.click(button);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("shows no reason line for a thrown value that carries none", () => {
    const { rerender } = render(
      <InlineAlert severity="error" title="Couldn't load" error={{ code: 5 }} data-testid="o" />,
    );
    expect(screen.getByTestId("o")).not.toHaveTextContent("[object Object]");
    // SW-FU-B8 review: no filler reason either — the title is the whole message.
    expect(screen.getByTestId("o")).toHaveTextContent(/^Couldn't load$/);
    rerender(
      <InlineAlert severity="error" title="Couldn't load" error={new Error("")} data-testid="o" />,
    );
    expect(screen.getByTestId("o")).toHaveTextContent(/^Couldn't load$/);
    rerender(<InlineAlert severity="error" title="Couldn't load" error={null} data-testid="o" />);
    expect(screen.getByTestId("o")).toHaveTextContent(/^Couldn't load$/);
  });
});
