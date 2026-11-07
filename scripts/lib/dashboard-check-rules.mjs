// SPDX-License-Identifier: MIT
// Dashboard SOURCE rules: findFalseZeros, findDetailInLoop and the code/string masks, plus the
// F1, F8, F10 and K6+ rules. One module with no
// dependencies, so the starter template's check and the later publish_dashboard port both import it unchanged.
//
// The rules are lexical: they catch the known shapes and say which line, rather than prove a program. Code only: comments are
// dropped and string contents blanked (same length), so prose and copy never match. Each finding is
// {rule, severity: "refuse" | "warn", line, text, message}. F1 refuses (the page never starts on the share host); the rest warn
// first and become refusals once the shipped dashboards pass.

const SEVERITY = {
  F1: "refuse",
  F4: "warn",
  F5: "warn",
  F8: "warn",
  F10: "warn",
  "K6+list": "warn",
  "K6+section": "warn",
  "K6+cache": "warn",
};

const codeOf = (text) =>
  text
    .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "))
    .replace(/(^|[^:\\'"`])\/\/.*$/gm, (m, lead) => lead + " ".repeat(m.length - lead.length));
// A template literal's text is blanked but its `${ ... }` expressions are kept (a `?? 0` in one is still a `?? 0`); newlines stay.
function maskTemplate(t) {
  let out = "`";
  let depth = 0;
  for (let i = 1; i < t.length - 1; i++) {
    const c = t[i];
    if (depth === 0) {
      if (c === "$" && t[i + 1] === "{") {
        out += "${";
        depth = 1;
        i++;
      } else out += c === "\n" ? "\n" : " ";
      continue;
    }
    if (c === "{") depth++;
    else if (c === "}") depth--;
    out += c;
  }
  return out + "`";
}
const maskStrings = (text) =>
  text.replace(/'(?:[^'\\\n]|\\.)*'|"(?:[^"\\\n]|\\.)*"|`(?:[^`\\]|\\.)*`/g, (m) =>
    m[0] === "`" ? maskTemplate(m) : m[0] + " ".repeat(m.length - 2) + m.at(-1),
  );
const lineOf = (text, index) => text.slice(0, index).split("\n").length;
function closing(masked, open) {
  let depth = 0;
  for (let i = open; i < masked.length; i++) {
    const c = masked[i];
    if (c === "(" || c === "{" || c === "[") depth++;
    else if (c === ")" || c === "}" || c === "]") {
      depth--;
      if (depth === 0) return i;
    }
  }
  return masked.length;
}
const finding = (rule, code, index, text, message) => ({
  rule,
  severity: SEVERITY[rule],
  line: lineOf(code, index),
  text: text.trim(),
  message,
});

// F1: a hosted page must boot on what is present (CS.boot), never on DOM shape: the share host's inliner strips every script[src].
const DOM_SHAPE_BOOT = /script\s*\[\s*src|\bcurrentScript\b|document\.scripts\b/g;
// The selector is a string (querySelector("script[src]")), so strings are kept here; comments are still dropped.
export function findDomShapeBoot(source) {
  const code = codeOf(source);
  return [...code.matchAll(DOM_SHAPE_BOOT)].map((m) =>
    finding(
      "F1",
      code,
      m.index,
      m[0],
      "boot on CS.boot, not on script[src] or currentScript: the share host removes every script src, so the page would stay blank",
    ),
  );
}

// F5: the nullable fields of storyTotals and story_changes totals. null means unknown, so `x ?? 0` and `x || 0` make a false zero.
export const NULLABLE_FIELDS = [
  "costUsd",
  "lines",
  "commits",
  "files",
  "reviewRounds",
  "sessions",
  "wallMs",
  "activeMs",
  "idleMs",
  "cost",
  "rounds",
];
const FALSE_ZERO = new RegExp(
  `\\b(?:${NULLABLE_FIELDS.join("|")})\\b[^;{}]{0,40}?(?:\\?\\?|\\|\\|)\\s*0(?![.\\w])`,
  "g",
);
export function findFalseZeros(source) {
  const code = maskStrings(codeOf(source));
  // A count of an array (`row.sessions.length || 0`) is a number, not a nullable total.
  return [...code.matchAll(FALSE_ZERO)]
    .filter((m) => !/\.(?:length|size)\b/.test(m[0]))
    .map((m) =>
      finding(
        "F5",
        code,
        m.index,
        m[0],
        "a null total is Unknown, never a 0: check for null and use CS.value",
      ),
    );
}

// F4: a detail query (one story's changes, a why-line) belongs to a detail view or a section, on demand. Inside a loop over a list's rows
// it is the 770-reads-at-once flood: a list reads totals once (CS.visibleTotals).
const LOOP_OPEN =
  /\b(?:for|while)\s*(?:await\s*)?\(|\.(?:map|forEach|flatMap|filter|reduce|some|every|find|findIndex)\s*\(/g;
function loopRanges(masked) {
  const ranges = [];
  for (const m of masked.matchAll(LOOP_OPEN)) {
    const open = m.index + m[0].length - 1;
    let end = closing(masked, open);
    // for/while: the body is the block (or statement) after the parentheses.
    if (!m[0].startsWith(".")) {
      const rest = masked.slice(end + 1);
      const skip = rest.match(/^\s*/)[0].length;
      end =
        rest[skip] === "{"
          ? closing(masked, end + 1 + skip)
          : end + 1 + skip + Math.max(0, rest.slice(skip).search(/;/));
    }
    ranges.push([m.index, end]);
  }
  return ranges;
}
function inLoop(source, pattern, rule, message) {
  const code = codeOf(source);
  const ranges = loopRanges(maskStrings(code));
  return [...code.matchAll(pattern)]
    .filter((c) => ranges.some(([from, to]) => c.index > from && c.index < to))
    .map((c) => finding(rule, code, c.index, c[0], message));
}
const DETAIL_CALL =
  /(?:\.|\b)(?:call|query|queryAll|subscribe)\s*\(\s*['"`](?:storyChanges|story_changes|whyLine|why_line)['"`]/g;
export const findDetailInLoop = (source) =>
  inLoop(
    source,
    DETAIL_CALL,
    "F4",
    "a detail query inside a loop over rows: read the list's totals with CS.visibleTotals and the detail in a CS.section",
  );
// K6+: a section read is one bounded read per opened region, never one per row.
export const findSectionInLoop = (source) =>
  inLoop(
    source,
    /(?:\.|\b)section\s*\(/g,
    "K6+section",
    "CS.section inside a loop over rows: one section per region, loaded when it opens",
  );

// F8: a browser test waits on the harness clock, never on real time (WebKit throttles background timers, so runs hang).
export function findRealTimeWaits(source) {
  const code = maskStrings(codeOf(source));
  return [
    ...code.matchAll(
      /\bwaitForTimeout\s*\(|\bwait\s*\(\s*\d{2,}\s*\)|new Promise\(\s*\(?\w*\)?\s*=>\s*setTimeout\(/g,
    ),
  ].map((m) =>
    finding(
      "F8",
      code,
      m.index,
      m[0],
      "advance the harness clock (page.clock) instead of waiting in real time",
    ),
  );
}

// Calls to `name(` whose argument text (up to the matching parenthesis) satisfies `bad`.
function callsWith(source, name, bad, rule, message) {
  const code = codeOf(source);
  const masked = maskStrings(code);
  const out = [];
  for (const m of masked.matchAll(new RegExp(`(?:\\.|\\b)${name}\\s*\\(`, "g"))) {
    const open = m.index + m[0].length - 1;
    const args = code.slice(open, closing(masked, open) + 1);
    if (bad(args)) out.push(finding(rule, code, m.index, m[0], message));
  }
  return out;
}
// F10: a query names a limit. The engine's default limits are its maximum, and an unbounded answer can be 1 MB.
const LIMITED = new Set([
  "queryContext",
  "storyChanges",
  "sessionRead",
  "inboxList",
  "listWorkspaces",
]);
export function findQueryWithoutLimit(source) {
  const code = codeOf(source);
  const masked = maskStrings(code);
  const out = [];
  for (const m of code.matchAll(
    /(?:\.|\b)(?:call|query|queryAll|cachedQuery|read)\s*\(\s*['"`](\w+)['"`]/g,
  )) {
    if (!LIMITED.has(m[1])) continue;
    const open = masked.indexOf("(", m.index);
    const args = code.slice(open, closing(masked, open) + 1);
    if (!/\blimit\b/.test(args))
      out.push(
        finding("F10", code, m.index, m[0], `${m[1]} without a limit: pass an explicit limit`),
      );
  }
  return out;
}
// A boot's `queries: {alias: {name, args, maxAgeMs}}` follow the same rules as the calls: a limit for the queries that need one (F10) and a
// maxAgeMs for each (K6+; boot's default of a minute is not a decision the page made).
export function findBootQueries(source) {
  const code = codeOf(source);
  const masked = maskStrings(code);
  const out = [];
  for (const block of masked.matchAll(/\bqueries\s*:\s*\{/g)) {
    const open = block.index + block[0].length - 1;
    const end = closing(masked, open);
    for (const m of code.slice(open, end).matchAll(/\bname\s*:\s*['"`](\w+)['"`]/g)) {
      const at = open + m.index;
      let from = at;
      for (let depth = 0; from > open; from--) {
        if (masked[from] === "}") depth++;
        else if (masked[from] === "{" && depth-- === 0) break;
      }
      const query = code.slice(from, closing(masked, from) + 1);
      if (!/\bmaxAgeMs\b/.test(query))
        out.push(
          finding(
            "K6+cache",
            code,
            at,
            m[0],
            "a boot query needs maxAgeMs: how old an answer may be before it reads again",
          ),
        );
      if (LIMITED.has(m[1]) && !/\blimit\b/.test(query))
        out.push(finding("F10", code, at, m[0], `${m[1]} without a limit: pass an explicit limit`));
    }
  }
  return out;
}
// K6+: every cachedQuery names maxAgeMs.
export const findCachedQueryWithoutMaxAge = (source) =>
  callsWith(
    source,
    "cachedQuery",
    (args) => !/\bmaxAgeMs\b/.test(args),
    "K6+cache",
    "cachedQuery needs maxAgeMs: how old an answer may be before it reads again",
  );

// K6+, a page rule over all of a dashboard's modules: a list that reads totals uses visibleTotals (visible rows only) and a budget.
export function findListPageGaps(sources) {
  const all = Object.values(sources)
    .map((s) => maskStrings(codeOf(s)))
    .join("\n");
  const out = [];
  if (!/\b(?:storyTotals|batchTotals)\b/.test(all)) return out;
  const gap = (text, message) => ({
    rule: "K6+list",
    severity: SEVERITY["K6+list"],
    line: 0,
    text,
    message,
  });
  if (!/\bvisibleTotals\b/.test(all))
    out.push(
      gap(
        "visibleTotals",
        "a list page reads totals through CS.visibleTotals, so only the rows on screen are asked",
      ),
    );
  if (!/\b(?:CS|kit)\.budget\s*\(/.test(all))
    out.push(
      gap(
        "budget",
        "a list page sets its read budget with CS.budget (the default of 2 in flight applies if you accept it explicitly)",
      ),
    );
  return out;
}

const PER_FILE = [
  findDomShapeBoot,
  findFalseZeros,
  findDetailInLoop,
  findSectionInLoop,
  findQueryWithoutLimit,
  findCachedQueryWithoutMaxAge,
  findBootQueries,
];
const TEST_FILE = /\.(?:browser\.)?(?:test|spec)\.m?[jt]sx?$/;
// Every rule over a dashboard's own modules: {fileName: source}. Test files get F8 as well. Returns {refusals, warnings}, each
// finding carrying its file.
export function checkDashboardSource(sources) {
  const findings = [];
  for (const [file, source] of Object.entries(sources)) {
    const rules = TEST_FILE.test(file) ? [findRealTimeWaits] : PER_FILE;
    for (const rule of rules) for (const f of rule(source)) findings.push({ ...f, file });
  }
  const dashboardFiles = Object.fromEntries(
    Object.entries(sources).filter(([file]) => !TEST_FILE.test(file)),
  );
  for (const f of findListPageGaps(dashboardFiles)) findings.push({ ...f, file: "(page)" });
  return {
    refusals: findings.filter((f) => f.severity === "refuse"),
    warnings: findings.filter((f) => f.severity === "warn"),
  };
}
export const describeFinding = (f) =>
  `${f.file}${f.line ? `: line ${f.line}` : ""}: ${f.rule} "${f.text}": ${f.message}`;
