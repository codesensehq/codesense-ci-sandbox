// SPDX-License-Identifier: MIT
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { FileScope } from "@/features/dashboard/lib/file-scope";

import {
  EditingInScope,
  InheritedFileExplainer,
  ScopeBadge,
  type NavProvenance,
} from "./scope-badge";

const provenance: NavProvenance = {
  glyphMap: new Map([["alderco/codesense", "CS"]]),
  isLightTheme: false,
};

function scope(over: Partial<FileScope> = {}): FileScope {
  return { repoSlug: null, sourceTeam: null, isInherited: false, ...over };
}

describe("ScopeBadge (SW-VE-5 AC-1)", () => {
  it("renders nothing when there is no scope and no slug (degrade)", () => {
    const { container } = render(<ScopeBadge scope={null} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders the named repo tint for a source-repo scope", () => {
    render(<ScopeBadge scope={scope({ repoSlug: "alderco/codesense" })} provenance={provenance} />);
    expect(screen.getByText("CS")).toBeInTheDocument();
  });

  it("preserves the pre-VE-5 tint from the raw sourceRepo when there is no scope", () => {
    render(<ScopeBadge scope={null} sourceRepo="alderco/codesense" provenance={provenance} />);
    expect(screen.getByText("CS")).toBeInTheDocument();
  });

  it("renders nothing for an own-team scope with no repo", () => {
    const { container } = render(<ScopeBadge scope={scope({ sourceTeam: "default" })} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders a dimmed inherited marker naming the parent team (wins over the repo tint)", () => {
    render(
      <ScopeBadge
        scope={scope({
          repoSlug: "alderco/codesense",
          sourceTeam: "platform",
          isInherited: true,
        })}
        provenance={provenance}
      />,
    );
    const badge = screen.getByTestId("scope-badge-inherited");
    expect(badge).toHaveTextContent("platform");
    expect(badge).toHaveAttribute("aria-label", "Inherited from platform");
    // Inherited wins — the repo glyph is not shown.
    expect(screen.queryByText("CS")).not.toBeInTheDocument();
  });
});

describe("EditingInScope (SW-VE-5 AC-2 — VE-3 mounts this)", () => {
  it("renders nothing without a scope", () => {
    const { container } = render(<EditingInScope scope={null} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("names the editable source-repo scope with its tint", () => {
    render(
      <EditingInScope scope={scope({ repoSlug: "alderco/codesense" })} provenance={provenance} />,
    );
    const chip = screen.getByTestId("editing-in-scope");
    expect(chip).toHaveTextContent("Editing in codesense");
    expect(chip).toHaveTextContent("CS");
  });

  it("names the owning team for an own-team scope", () => {
    render(<EditingInScope scope={scope({ sourceTeam: "default" })} />);
    expect(screen.getByTestId("editing-in-scope")).toHaveTextContent("Editing in default");
  });

  it("says where an inherited file's edits go (SW-XS-20)", () => {
    render(<EditingInScope scope={scope({ sourceTeam: "platform", isInherited: true })} />);
    expect(screen.getByTestId("editing-in-scope")).toHaveTextContent(
      "From platform — edits save to platform",
    );
  });
});

describe("InheritedFileExplainer (SW-XS-20)", () => {
  it("renders nothing for a file that is not inherited", () => {
    const { container } = render(<InheritedFileExplainer scope={scope({ isInherited: false })} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("says edits update the parent team and every team inheriting from it, then the action", () => {
    render(
      <InheritedFileExplainer
        scope={scope({ sourceTeam: "platform", isInherited: true })}
        action={<button type="button">Copy into default</button>}
      />,
    );
    const el = screen.getByTestId("inherited-file-explainer");
    expect(el).toHaveTextContent(
      "This file comes from platform. Changes you make here update it for platform and every team that inherits from it.",
    );
    expect(screen.getByRole("button", { name: "Copy into default" })).toBeInTheDocument();
  });
});
