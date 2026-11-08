// SPDX-License-Identifier: MIT
const PUBLIC_HOSTS = new Set([
  "codesense.ai",
  "www.codesense.ai",
  "updates.codesense.ai",
  "dl.codesense.ai",
  "share.codesense.ai",
  "docs.codesense.ai",
]);
const ROLE_LOCALS = new Set([
  "security",
  "conduct",
  "support",
  "hello",
  "info",
  "privacy",
  "legal",
  "noreply",
  "no-reply",
  "git",
  "press",
  "sales",
]);
const OK_EMAIL_DOMAINS =
  /(^|\.)(example(\.[a-z.]+)?|codesense\.(ai|local)|openssh\.com|test\.com|b\.io|[a-z-]+\.int|patch|test|invalid|localhost)$/i;
// real domains are allowed only for these exact addresses (fixtures and the GitHub/Anthropic bot addresses in use)
const OK_EMAILS = new Set([
  "me@x.com",
  "mate@x.com",
  "t@t.com",
  "noreply@anthropic.com",
  "123+sam@users.noreply.github.com",
]);
const OK_HOME_NAMES = new Set([
  "t",
  "sam",
  "meg",
  "ada",
  "Jane",
  "owner",
  "account",
  "box",
  "uid",
  "users",
  "other",
  "anybody",
  "y",
  "7",
  "etc",
  "Shared",
  "Public",
  "Default",
  "you",
  "user",
  "username",
  "name",
  "me",
  "runner",
  "example",
  "foo",
  "bar",
  "test",
  "tester",
  "someone",
  "alice",
  "bob",
  "dev",
  "home",
  "x",
  "u",
  "a",
  "b",
  "codesense",
  "ubuntu",
  "root",
  "build",
  "admin",
]);

const UUID = "\\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\\b";
// the base64 of a minisign (Tauri updater) secret key file's first line, at each of the 3 byte alignments it can sit at
// inside a longer base64 run; built at load so this file does not carry the literal
const MINISIGN_SECRET_B64 = [0, 1, 2].map((k) => {
  // Preserve source line boundaries to avoid matching this declaration.
  // prettier-ignore
  const head = Buffer.from([
    "untrusted comment:",
    "minisign encrypted",
    "secret key",
  ].join(" "));
  return Buffer.concat([Buffer.alloc(k), head])
    .toString("base64")
    .slice(Math.ceil((k * 8) / 6), Math.floor(((k + head.length) * 8) / 6));
});
// a type in an annotation or schema (KEY: string, KEY: z.string(), KEY: Option<String>, KEY: process.env.X)
const TYPE_WORD =
  /^(?:(?:z\.|t\.)?(?:string|str|bool|boolean|number|int|integer|any|unknown|null|undefined|true|false|none|optional|required|secret)(?:\(\)?)?(?:\[\]?)?|Option<\w+>|(?:process\.env|import\.meta\.env|env)\.\w+)$/i;
const reEscape = (s) => s.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&");
const PLACEHOLDER_SECRET =
  /^(?:\$\{?\w+\}?|\$\(\w+\)|<[^>]*>|\{\{?[^}]*\}\}?|%[sd]|\*+|x+|\.\.\.|pass(?:word|wd)?|pwd|pw|secret|token)$/i;

// kind: 'show' prints the match; 'secret' never does. `line` gates a rule on the text within NEAR chars of the match.
// Every quantifier is capped (no rule can match more than ~900 chars, under OVERLAP, and none backtracks over a long run).
// session-id rules name `uuidAt`, where the UUID sits in the match; every UUID they flag is then flagged everywhere.
export const GENERIC_RULES = [
  {
    id: "email",
    sev: "error",
    kind: "show",
    re: /[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9-]{1,63}(?:\.[A-Za-z0-9-]{1,63}){0,8}\.[A-Za-z]{2,24}/g,
    ok: (m) => {
      const [l, d] = m.split("@");
      return (
        OK_EMAILS.has(m.toLowerCase()) ||
        OK_EMAIL_DOMAINS.test(d) ||
        (/codesense\.ai$/i.test(d) && ROLE_LOCALS.has(l.toLowerCase())) ||
        (ROLE_LOCALS.has(l.toLowerCase()) &&
          /^(github\.com|gitlab\.com|bitbucket\.org)$/i.test(d)) ||
        /\.(png|jpe?g|gif|svg|webp|mjs|js|ts|tsx|css|json|wasm|node|woff2?)$/i.test(d)
      );
    },
  },
  {
    id: "home-path",
    sev: "error",
    kind: "show",
    re: /(?:\/Users\/|\/home\/|\b[A-Za-z]:(?:\\{1,2}|\/)[Uu]sers(?:\\{1,2}|\/))([A-Za-z0-9._-]{1,64})/g,
    ok: (m, g) => OK_HOME_NAMES.has(g[1]) || /^[._$<{]|\.md$/.test(g[1]),
  },
  // Claude's project dirs encode the home path with dashes (-Users-<name>-repo, C--Users-<name>-repo)
  {
    id: "home-path",
    sev: "error",
    kind: "show",
    re: /\.claude[\\/]{1,2}projects[\\/]{1,2}(?:[A-Za-z]-)?-(?:Users|home)-([A-Za-z0-9_]{1,64})/gi,
    ok: (m, g) => OK_HOME_NAMES.has(g[1]),
  },
  // macOS per-user temp dirs (/var/folders/<2>/<id>/T): placeholder segments like xx/yyyy are fine
  {
    id: "home-path",
    sev: "error",
    kind: "show",
    re: /\/var\/folders\/([A-Za-z0-9_+-]{2})\/([A-Za-z0-9_+-]{1,64})/g,
    ok: (m, g) => /^(.)\1*$/.test(g[1]) && /^(.)\1*$/.test(g[2]),
  },
  {
    id: "internal-host",
    sev: "error",
    kind: "show",
    re: /[A-Za-z0-9-]{1,63}(?:\.[A-Za-z0-9-]{1,63}){0,8}\.codesense\.ai|\bcodesense\.ai\b/g,
    ok: (m) => PUBLIC_HOSTS.has(m.toLowerCase()),
  },
  {
    id: "private-ip",
    sev: "error",
    kind: "show",
    re: /\b(?:10\.\d{1,3}\.\d{1,3}\.\d{1,3}|192\.168\.\d{1,3}\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}|100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d{1,3}\.\d{1,3})\b/g,
  },
  // link-local and IPv6 unique-local: usually examples or SSRF guards, so warnings
  {
    id: "private-ip",
    sev: "warn",
    kind: "show",
    re: /\b169\.254\.\d{1,3}\.\d{1,3}\b|\bf[cd][0-9a-f]{2}:(?=[0-9a-f]{0,4}:)[0-9a-f:]{1,36}/gi,
  },
  {
    id: "token",
    sev: "error",
    kind: "secret",
    re: new RegExp(
      [
        "csp_[A-Za-z0-9]{16,256}",
        "sk-[A-Za-z0-9_-]{20,256}",
        "gh[pousr]_[A-Za-z0-9]{30,256}",
        "github_pat_[A-Za-z0-9_]{20,256}",
        "(?:AKIA|ASIA)[0-9A-Z]{16}",
        "xox[abeoprs]-[A-Za-z0-9-]{10,256}", // Slack
        "xapp-[A-Za-z0-9-]{10,256}", // Slack app-level
        "hooks\\.slack\\.com/(?:services|workflows|triggers)/[A-Za-z0-9_/-]{20,300}", // Slack webhook URL
        "[rs]k_(?:live|test)_[A-Za-z0-9]{10,256}", // Stripe secret and restricted keys, live and test mode
        "whsec_[A-Za-z0-9]{10,256}", // Stripe webhook signing secret
        "AIza[0-9A-Za-z_-]{35}", // Google
        "npm_[A-Za-z0-9]{36}",
        "pypi-[A-Za-z0-9_-]{40,256}",
        "glpat-[A-Za-z0-9_-]{20,256}", // GitLab
        "eyJ[A-Za-z0-9_-]{5,600}\\.eyJ[A-Za-z0-9_-]{10,300}", // JWT: header (kid/jku/x5c can be long) and payload
      ]
        .map((s) => `\\b${s}`)
        .join("|"),
      "g",
    ),
  },
  // AWS secret access key: 40 base64 chars with both cases, near a name for one. `=` may precede it (KEY=value)
  // but not a base64 char, so it never matches inside a longer run.
  {
    id: "token",
    sev: "error",
    kind: "secret",
    line: /aws_?secret|secret_?access_?key|SecretAccessKey/i,
    re: /(?<![A-Za-z0-9/+])(?=[A-Za-z0-9/+]{0,39}[A-Z])(?=[A-Za-z0-9/+]{0,39}[a-z])[A-Za-z0-9/+]{40}(?![A-Za-z0-9/+=])/g,
  },
  {
    id: "credential",
    sev: "error",
    kind: "secret",
    re: /\bbearer[ \t]{1,8}[A-Za-z0-9._~+/-]{20,500}/gi,
  },
  // URL userinfo with a password (the user may be empty: redis://:pw@host); template placeholders are fine
  {
    id: "credential",
    sev: "error",
    kind: "secret",
    re: /\b[a-z][a-z0-9+.-]{1,20}:\/\/([^\s/?#@:"'<>]{0,100}):([^\s/?#@"'<>]{1,200})@[A-Za-z0-9.-]{1,253}/gi,
    ok: (m, g) => PLACEHOLDER_SECRET.test(g[2]),
  },
  // Apple app-specific password (notarization): four lowercase groups of four, near a word for one on the
  // same line or an adjacent one (a `notarytool … \` continuation puts the flag and the value on separate lines)
  {
    id: "credential",
    sev: "error",
    kind: "secret",
    line: /app(?:lication)?.?specific|apple|notar|password|altool/i,
    adjacent: true,
    re: /(?<![\w-])[a-z]{4}-[a-z]{4}-[a-z]{4}-[a-z]{4}(?![\w-])/g,
  },
  // a Tauri updater signing key (or its password) assigned a value: KEY=value, "KEY": "value", KEY: "value", or
  // KEY: value as one unquoted token (YAML). A variable, a ${KEY:-} expansion, a template, a <placeholder>, a
  // path, a type annotation (KEY: string) or prose (KEY: the key from step 1, more words after it) is fine.
  {
    id: "token",
    sev: "error",
    kind: "secret",
    re: /(["']?)(?<![\w{])TAURI_SIGNING_PRIVATE_KEY(?:_PASSWORD)?\1[ \t]{0,4}([=:])[ \t]{0,4}(["']?)([^\s"'`=][^\s"'`]{0,200})\3(?=[ \t]{0,40}(\S?))/g,
    ok: (m, g) =>
      /^(?:\$|<|\{|%|~|\/|\.\.?\/)/.test(g[4]) ||
      PLACEHOLDER_SECRET.test(g[4]) ||
      (g[2] === ":" &&
        !g[3] &&
        ((g[5] !== "" && g[5] !== "#") || TYPE_WORD.test(g[4].replace(/[,;})\]]+$/, "")))),
  },
  {
    id: "private-key",
    sev: "error",
    kind: "secret",
    // prettier-ignore
    re: new RegExp([
      "-----BEGIN [A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----",
      "untrusted comment: " +
        "[^\\n]{0,200}secret key",
    ].join("|"), "g"),
  },
  {
    id: "private-key",
    sev: "error",
    kind: "secret",
    re: new RegExp(MINISIGN_SECRET_B64.map(reEscape).join("|"), "g"),
  },
  {
    id: "apple-team-id",
    sev: "error",
    kind: "secret",
    line: /(?:DEVELOPMENT_TEAM|team[ _-]?id|\bTEAM\b)/i,
    re: /\b(?=[A-Z0-9]{0,9}[A-Z])(?=[A-Z0-9]{0,9}\d)[A-Z0-9]{10}\b/g,
  },
  {
    id: "cloudflare-id",
    sev: "error",
    kind: "secret",
    line: /account|r2|cloudflare|zone/i,
    re: /\b[0-9a-f]{32}\b/g,
  },
  {
    id: "access-aud",
    sev: "error",
    kind: "secret",
    line: /\baud\b|access/i,
    re: /\b[0-9a-f]{64}\b/g,
  },
  // internal text: error in docs, warn in code
  {
    id: "story-id",
    sev: "text",
    kind: "show",
    re: /\b(?:S[VWE]|SLW|PBF|OC|RT|RM)-(?:[A-Z]{1,6}-){0,4}\d{1,6}[a-z]?\b/g,
  },
  // session, inbox and tab ids: shell handles and Claude transcript paths anywhere, a UUID near a word for one
  {
    id: "session-id",
    sev: "error",
    kind: "show",
    re: new RegExp(`\\bshell-(?:${UUID}|mcp-\\d{6,40})`, "gi"),
    uuidAt: (m) => (/^shell-mcp-/i.test(m) ? -1 : 6),
  },
  {
    id: "session-id",
    sev: "error",
    kind: "show",
    re: new RegExp(
      `\\.claude[\\\\/]+projects[\\\\/]+[^\\\\/\\s"'<>\`]{1,200}[\\\\/]+${UUID}`,
      "gi",
    ),
    uuidAt: (m) => m.length - 36,
  },
  {
    id: "session-id",
    sev: "error",
    kind: "show",
    line: /session|inbox|agent|tab(?:s|_?id|\b)/i,
    re: new RegExp(UUID, "gi"),
    uuidAt: () => 0,
  },
].map((rule, index) => ({
  ...rule,
  order: [0, 1, 2, 3, 4, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 25, 26, 27][index],
}));
