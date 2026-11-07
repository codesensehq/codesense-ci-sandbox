// SPDX-License-Identifier: MIT
/**
 * SW-OC-B3 — shared join-org URL conformance battery.
 *
 * ONE URL battery both the `dev:mock` `classify_join_remote_cmd` handler and
 * the real Rust `classify_join_remote` (`src-tauri/src/commands/org.rs`) are
 * asserted against, so the mock can't silently drift from the shipped
 * classifier again. Each row pins the OBSERVABLE `JoinRemoteClassification`
 * decision the real backend makes (after SW-OC-B1/B2/B4), expressed as the
 * discriminant `kind` plus the load-bearing fields for that kind.
 *
 * The Rust side asserts these same decisions in
 * `join_remote_classification_uses_canonical_forge_and_provider_registries`,
 * `oc_b1_*`, `oc_b2_*`, and `oc_b4_*` (org.rs). If a decision changes on one
 * side, the row here must change too — that's the conformance contract.
 */

/** The discriminant of the real `JoinRemoteClassification` enum. */
export type JoinRemoteKind = "supported" | "comingSoon" | "unavailable" | "unsupported";

/** One conformance row: an input URL + the decision the real Rust makes. */
export interface JoinRemoteConformanceCase {
  /** What the row demonstrates — mirrors the Rust test intent. */
  readonly desc: string;
  /** The raw URL a user pastes into the join field. */
  readonly url: string;
  /** The classification `kind` the real backend returns. */
  readonly kind: JoinRemoteKind;
  /** For `supported`/`comingSoon`, the resolved provider id. */
  readonly provider?: string;
  /** For `supported`/`comingSoon`, the human provider name. */
  readonly providerName?: string;
  /** For `supported`, the canonical `host/owner/repo` URL (a key, no scheme). */
  readonly canonicalUrl?: string;
  /**
   * For `supported`, the `https://` URL the join form hands to
   * `connect_to_org_cmd` (SW-FU-B1: never the scheme-less key). Defaults to
   * `https://` + `canonicalUrl` when the input's path is already lowercase.
   */
  readonly cloneUrl?: string;
  /**
   * For `unsupported`, substrings the message MUST contain (case-insensitive),
   * so a self-hosted-aware vs generic rejection can be asserted without
   * pinning exact copy.
   */
  readonly messageContains?: readonly string[];
}

/**
 * The battery. Rows cover: canonical accept forms (incl. `www.`, SSH, `http`,
 * whitespace, `.git`, query/fragment), the `comingSoon` bitbucket dead-end
 * fix, and the reject cases (org-only, extra path segments, self-hosted
 * enterprise families, truly-unknown host, empty/garbage input).
 */
export const JOIN_REMOTE_CONFORMANCE: readonly JoinRemoteConformanceCase[] = [
  // --- Supported (github.com) ---
  {
    desc: "canonical https github url",
    url: "https://github.com/org/context",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "github url with .git suffix",
    url: "https://github.com/org/context.git",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "github url with trailing slash",
    url: "https://github.com/org/context/",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "ssh shorthand github url",
    url: "git@github.com:org/context.git",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "ssh:// github url",
    url: "ssh://git@github.com/org/context.git",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "github url with surrounding whitespace",
    url: "  https://github.com/org/context  ",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "bare host github url (no protocol)",
    url: "github.com/org/context",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "github url with fragment",
    url: "https://github.com/org/context#readme",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "github url with query string",
    url: "https://github.com/org/context?tab=readme",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    // SW-OC-B1: the browser-canonical www. form (301-redirects to bare host).
    desc: "www. github url (SW-OC-B1)",
    url: "https://www.github.com/org/context",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "http:// github url",
    url: "http://github.com/org/context",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/org/context",
  },
  {
    desc: "mixed-case ssh github url normalizes to lowercase canonical",
    url: "git@GitHub.com:Acme/Context.git",
    kind: "supported",
    provider: "github",
    providerName: "GitHub",
    canonicalUrl: "github.com/acme/context",
    cloneUrl: "https://github.com/Acme/Context",
  },

  // --- Supported (gitlab.com) ---
  {
    desc: "canonical gitlab url",
    url: "https://gitlab.com/acme/context",
    kind: "supported",
    provider: "gitlab",
    providerName: "GitLab",
    canonicalUrl: "gitlab.com/acme/context",
  },
  {
    desc: "www. gitlab url (SW-OC-B1)",
    url: "https://www.gitlab.com/org/context",
    kind: "supported",
    provider: "gitlab",
    providerName: "GitLab",
    canonicalUrl: "gitlab.com/org/context",
  },

  // --- ComingSoon (bitbucket.org — transports but no OAuth provider yet) ---
  {
    // SW-OC-B2: was a message-less Unavailable dead-end; now honest ComingSoon.
    desc: "bitbucket url is ComingSoon, not a dead-end (SW-OC-B2)",
    url: "https://bitbucket.org/org/context",
    kind: "comingSoon",
    provider: "bitbucket",
    providerName: "Bitbucket",
  },
  {
    desc: "www. bitbucket url is ComingSoon (SW-OC-B1 + B2)",
    url: "https://www.bitbucket.org/org/context",
    kind: "comingSoon",
    provider: "bitbucket",
    providerName: "Bitbucket",
  },

  // --- Unsupported: org-only (MissingOwnerRepo) ---
  {
    desc: "org-only github url rejects (MissingOwnerRepo)",
    url: "https://github.com/org",
    kind: "unsupported",
    messageContains: ["owner"],
  },

  // --- Unsupported: extra path segments (UnexpectedPathSegments) ---
  {
    desc: "web tree url rejects (UnexpectedPathSegments)",
    url: "https://github.com/org/context/tree/main",
    kind: "unsupported",
    messageContains: ["extra path segments"],
  },

  // --- Unsupported: self-hosted / enterprise (self-hosted-aware copy, SW-OC-B4) ---
  {
    desc: "github enterprise host gets self-hosted-aware message (SW-OC-B4)",
    url: "https://github.mycorp.com/org/context",
    kind: "unsupported",
    messageContains: ["Self-hosted GitHub Enterprise isn't supported yet"],
  },
  {
    desc: "gitlab self-hosted host gets self-hosted-aware message (SW-OC-B4)",
    url: "https://gitlab.mycorp.com/org/context",
    kind: "unsupported",
    messageContains: ["Self-hosted GitLab Enterprise isn't supported yet"],
  },
  {
    desc: "bitbucket self-hosted host gets self-hosted-aware message (SW-OC-B4)",
    url: "https://bitbucket.mycorp.com/org/context",
    kind: "unsupported",
    messageContains: ["Self-hosted Bitbucket Enterprise isn't supported yet"],
  },

  // --- Unsupported: truly-unknown host (generic supported-forge list) ---
  {
    desc: "unknown host gets the generic supported-forge list",
    url: "https://gitea.example.com/org/context",
    kind: "unsupported",
    messageContains: ["github.com", "gitlab.com", "bitbucket.org"],
  },
  {
    desc: "plain unknown host rejects",
    url: "https://example.com/acme/context",
    kind: "unsupported",
    messageContains: ["github.com"],
  },

  // --- Unsupported: garbage / empty ---
  {
    desc: "non-url garbage rejects",
    url: "not a repository url",
    kind: "unsupported",
  },
  {
    desc: "empty input rejects",
    url: "",
    kind: "unsupported",
  },
];
