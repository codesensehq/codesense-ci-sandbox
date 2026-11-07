// SPDX-License-Identifier: MIT
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import {
  CODESENSE_DOCS_DISPLAY_NAME,
  CODESENSE_DOCS_README,
  CODESENSE_DOCS_SLUG,
  CODESENSE_DOCS_TEAM,
  classifyConnectError,
  defaultContextRepoLocation,
  folderName,
  orderContextRepos,
  pickLaunchOrg,
} from "./codesense-docs";

const docs = { slug: CODESENSE_DOCS_SLUG, display_name: CODESENSE_DOCS_DISPLAY_NAME };
const acme = { slug: "local/acme", display_name: "Acme" };
const beta = { slug: "local/beta", display_name: "Beta" };

describe("the bundled CodeSense docs identity", () => {
  it("matches codesense-docs/bundle.json, the generator's and backend's source", () => {
    const bundle = JSON.parse(
      readFileSync(join(process.cwd(), "codesense-docs", "bundle.json"), "utf8"),
    );
    expect(bundle.slug).toBe(CODESENSE_DOCS_SLUG);
    expect(bundle.team).toBe(CODESENSE_DOCS_TEAM);
    expect(bundle.readme).toBe(CODESENSE_DOCS_README);
    expect(bundle.displayName).toBe(CODESENSE_DOCS_DISPLAY_NAME);
  });

  it("is labelled CodeSense Docs, not plain CodeSense, so it never reads as the user's own repo", () => {
    expect(CODESENSE_DOCS_DISPLAY_NAME).toBe("CodeSense Docs");
  });
});

describe("pickLaunchOrg — first launch vs later launches", () => {
  it("very first launch (nothing persisted): the docs, with the README", () => {
    expect(pickLaunchOrg({ orgs: [docs], activeOrgSlug: null })).toEqual({
      slug: CODESENSE_DOCS_SLUG,
      firstLaunch: true,
    });
  });

  it("first launch even when other context repos are already connected: still the docs", () => {
    expect(pickLaunchOrg({ orgs: [acme, docs], activeOrgSlug: null })).toEqual({
      slug: CODESENSE_DOCS_SLUG,
      firstLaunch: true,
    });
  });

  it("later launches reopen the last opened context repo", () => {
    expect(pickLaunchOrg({ orgs: [docs, acme, beta], activeOrgSlug: "local/beta" })).toEqual({
      slug: "local/beta",
      firstLaunch: false,
    });
  });

  it("the docs as the last opened one reopen without the first-launch README", () => {
    expect(pickLaunchOrg({ orgs: [docs, acme], activeOrgSlug: CODESENSE_DOCS_SLUG })).toEqual({
      slug: CODESENSE_DOCS_SLUG,
      firstLaunch: false,
    });
  });

  it("a just-connected context repo wins over the persisted one", () => {
    expect(
      pickLaunchOrg({
        orgs: [docs, acme, beta],
        activeOrgSlug: "local/acme",
        newlyConnected: "local/beta",
      }),
    ).toEqual({ slug: "local/beta", firstLaunch: false });
  });

  it("a last-opened repo that is gone falls back to the docs (never a dead end)", () => {
    expect(pickLaunchOrg({ orgs: [docs, acme], activeOrgSlug: "local/gone" })).toEqual({
      slug: CODESENSE_DOCS_SLUG,
      firstLaunch: false,
    });
  });

  it("without the docs (placement failed) falls back to the first one, or none", () => {
    expect(pickLaunchOrg({ orgs: [acme], activeOrgSlug: null })).toEqual({
      slug: "local/acme",
      firstLaunch: false,
    });
    expect(pickLaunchOrg({ orgs: [], activeOrgSlug: null })).toEqual({
      slug: null,
      firstLaunch: false,
    });
  });
});

describe("switcher order and connect states", () => {
  it("pins the docs first, then alphabetical", () => {
    expect(orderContextRepos([beta, docs, acme]).map((o) => o.slug)).toEqual([
      CODESENSE_DOCS_SLUG,
      "local/acme",
      "local/beta",
    ]);
  });

  it("sorts backend refusals into the not-a-context-repo state or a plain error", () => {
    for (const message of [
      "Selected folder is not a Git checkout: could not find repository",
      "Select the checkout root, not a subdirectory or bare repository",
      "Cannot read selected context's org.yml: No such file or directory",
      "Invalid context org.yml: missing field `slug`",
    ]) {
      expect(classifyConnectError(new Error(message))).toEqual({
        kind: "not-a-context-repo",
        message,
      });
    }
    expect(
      classifyConnectError("This checkout is already attached under another principal"),
    ).toEqual({
      kind: "error",
      message: "This checkout is already attached under another principal",
    });
  });

  it("names folders and defaults new context repos to ~/CodeSense/<name>", () => {
    expect(folderName("/Users/me/acme-context/")).toBe("acme-context");
    expect(defaultContextRepoLocation("/Users/me", "Acme Billing")).toBe(
      "/Users/me/CodeSense/acme-billing",
    );
    expect(defaultContextRepoLocation("/Users/me/", "!!!")).toBe("/Users/me/CodeSense/context");
    expect(defaultContextRepoLocation("/Users/me", "..")).toBe("/Users/me/CodeSense/context");
  });
});
