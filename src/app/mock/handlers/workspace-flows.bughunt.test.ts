// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { workspaceFlowHandlers } from "./workspace-flows";
import { JOIN_REMOTE_CONFORMANCE, type JoinRemoteConformanceCase } from "./join-remote-conformance";
import type { JoinRemoteClassification } from "@/tauri/commands";

/**
 * SW-OC-B3 — the `dev:mock` `classify_join_remote_cmd` handler must MIRROR the
 * real Rust `classify_join_remote` (`src-tauri/src/commands/org.rs`). The mock
 * is what a developer sees when they run `pnpm dev:mock` to build the
 * join/create-org UX; if the mock accepts an input the real backend rejects
 * (or vice-versa), the UX is validated against a contract the shipped app does
 * not honor.
 *
 * This file was the OC bug-hunt repro (originally pinning the divergences on
 * `chore/oc-onboarding-bughunt`). It's been FLIPPED to assert the FIXED
 * behavior: every row of the shared `JOIN_REMOTE_CONFORMANCE` battery — the
 * one URL battery both the mock and the Rust classifier answer identically —
 * now matches. If the mock ever drifts from the Rust contract again, a row
 * fails here.
 */

function classify(url: string): JoinRemoteClassification {
  const handler = workspaceFlowHandlers().classify_join_remote_cmd!;
  return handler({ remoteUrl: url }) as JoinRemoteClassification;
}

/** Assert one conformance row against the mock handler's output. */
function assertMatchesCase(result: JoinRemoteClassification, c: JoinRemoteConformanceCase): void {
  expect(result.kind, `${c.desc}: ${c.url}`).toBe(c.kind);
  if (result.kind === "supported") {
    expect(result.provider).toBe(c.provider);
    expect(result.provider_name).toBe(c.providerName);
    expect(result.canonical_url).toBe(c.canonicalUrl);
    // SW-FU-B1: the form joins with `clone_url`; it must be clone-able.
    expect(result.clone_url).toBe(c.cloneUrl ?? `https://${c.canonicalUrl}`);
  } else if (result.kind === "comingSoon") {
    expect(result.provider).toBe(c.provider);
    expect(result.provider_name).toBe(c.providerName);
  } else if (result.kind === "unsupported") {
    for (const needle of c.messageContains ?? []) {
      expect(result.message.toLowerCase()).toContain(needle.toLowerCase());
    }
  }
}

describe("SW-OC-B3 — mock classify_join_remote mirrors the real Rust contract", () => {
  it.each(JOIN_REMOTE_CONFORMANCE.map((c) => [c.desc, c] as const))("%s", (_desc, c) => {
    assertMatchesCase(classify(c.url), c);
  });

  // Headline regressions the bug-hunt pinned — now asserting the FIXED behavior.

  it("ACCEPTS www.github.com (was a false-positive divergence, now correct — SW-OC-B1)", () => {
    expect(classify("https://www.github.com/org/context").kind).toBe("supported");
  });

  it("REJECTS an enterprise/self-hosted github host (SW-OC-B4)", () => {
    const result = classify("https://github.mycorp.com/org/context");
    expect(result.kind).toBe("unsupported");
    if (result.kind === "unsupported") {
      expect(result.message).toContain("Self-hosted GitHub Enterprise isn't supported yet");
    }
  });

  it("REJECTS an org-only URL (MissingOwnerRepo — actionable message)", () => {
    const result = classify("https://github.com/org");
    expect(result.kind).toBe("unsupported");
    if (result.kind === "unsupported") {
      expect(result.message.toLowerCase()).toContain("owner");
    }
  });

  it("REJECTS a web tree URL (UnexpectedPathSegments)", () => {
    expect(classify("https://github.com/org/context/tree/main").kind).toBe("unsupported");
  });

  it("classifies bitbucket.org as comingSoon, not a dead-end (SW-OC-B2)", () => {
    const result = classify("https://bitbucket.org/org/context");
    expect(result.kind).toBe("comingSoon");
    if (result.kind === "comingSoon") {
      expect(result.provider).toBe("bitbucket");
      expect(result.provider_name).toBe("Bitbucket");
    }
  });
});

describe("SW-FU-B1 — the connect argument conforms to the real clone", () => {
  const connect = (remoteUrl: string) =>
    workspaceFlowHandlers().connect_to_org_cmd!({
      remoteUrl,
      githubUserId: 1,
      credentialRef: { provider: "github", account: "work" },
    });

  it("refuses the scheme-less comparison key, as the real clone did", () => {
    const key = (classify("https://github.com/org/context") as { canonical_url: string })
      .canonical_url;
    expect(() => connect(key)).toThrow();
  });

  it.each(
    JOIN_REMOTE_CONFORMANCE.filter((c) => c.kind === "supported").map((c) => [c.url] as const),
  )("joins with the clone URL the form sends for %s", (url) => {
    const result = classify(url);
    if (result.kind !== "supported") throw new Error("expected supported");
    expect(() => connect(result.clone_url)).not.toThrow();
  });
});
