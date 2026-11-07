// SPDX-License-Identifier: MIT
// CVSS base scores for advisory severity: v3.0/v3.1 (the specification's base equations) and
// v4.0 (the algorithm of FIRST's reference calculator, as the RustSec cvss crate implements it: a
// macrovector lookup, then interpolation by severity distance). FIRST's tables are vendored in
// ./vendor/cvss40-tables.mjs under their own licence.

import { LEVELS, LOOKUP_VALUES, MAX_COMPOSED, MAX_SEVERITY } from "./vendor/cvss40-tables.mjs";

const V3 = {
  AV: { N: 0.85, A: 0.62, L: 0.55, P: 0.2 },
  AC: { L: 0.77, H: 0.44 },
  UI: { N: 0.85, R: 0.62 },
  CIA: { H: 0.56, L: 0.22, N: 0 },
};

const metrics = (vector) =>
  Object.fromEntries(
    vector
      .split("/")
      .slice(1)
      .map((kv) => kv.split(":")),
  );

// The CVSS 3.x base score of a vector, or null when it is not a valid 3.x vector.
export function cvss3Score(vector) {
  if (typeof vector !== "string" || !/^CVSS:3\.[01]\//.test(vector)) return null;
  const m = metrics(vector);
  const changed = m.S === "C";
  const pr = { N: 0.85, L: changed ? 0.68 : 0.62, H: changed ? 0.5 : 0.27 }[m.PR];
  const [av, ac, ui] = [V3.AV[m.AV], V3.AC[m.AC], V3.UI[m.UI]];
  const [c, i, a] = [V3.CIA[m.C], V3.CIA[m.I], V3.CIA[m.A]];
  if ([av, ac, ui, pr, c, i, a].includes(undefined) || !["U", "C"].includes(m.S)) return null;
  const iss = 1 - (1 - c) * (1 - i) * (1 - a);
  const impact = changed ? 7.52 * (iss - 0.029) - 3.25 * (iss - 0.02) ** 15 : 6.42 * iss;
  if (impact <= 0) return 0;
  const exploitability = 8.22 * av * ac * pr * ui;
  const roundUp = (x) => {
    const n = Math.round(x * 100000);
    return n % 10000 === 0 ? n / 100000 : (Math.floor(n / 10000) + 1) / 10;
  };
  return roundUp(Math.min((changed ? 1.08 : 1) * (impact + exploitability), 10));
}

const V4_BASE = ["AV", "AC", "AT", "PR", "UI", "VC", "VI", "VA", "SC", "SI", "SA"];
// Unset threat and requirement metrics score as their worst case.
const V4_DEFAULTS = { E: "A", CR: "H", IR: "H", AR: "H" };

const V4_MACROS = [];
for (let a = 0; a < 3; a++)
  for (let b = 0; b < 2; b++)
    for (let c = 0; c < 3; c++)
      for (let d = 0; d < 3; d++)
        for (let e = 0; e < 3; e++)
          for (let f = 0; f < 2; f++)
            if (!(c === 2 && f === 0)) V4_MACROS.push(`${a}${b}${c}${d}${e}${f}`);
const V4_LOOKUP = new Map(V4_MACROS.map((k, n) => [k, LOOKUP_VALUES[n]]));

// The effective metric values of a 4.0 vector (modified metrics override base ones), or null when
// a base metric is missing or a value is unknown.
function v4Metrics(vector) {
  const raw = metrics(vector);
  const m = {};
  for (const k of V4_BASE) {
    const value = raw[`M${k}`] && raw[`M${k}`] !== "X" ? raw[`M${k}`] : raw[k];
    if (LEVELS[k][value] === undefined) return null;
    m[k] = value;
  }
  for (const [k, worst] of Object.entries(V4_DEFAULTS)) {
    const value = raw[k] && raw[k] !== "X" ? raw[k] : worst;
    if (LEVELS[k][value] === undefined) return null;
    m[k] = value;
  }
  return m;
}

// The CVSS 4.0 score of a vector, or null when it is not a valid 4.0 vector.
export function cvss4Score(vector) {
  if (typeof vector !== "string" || !vector.startsWith("CVSS:4.0/")) return null;
  const m = v4Metrics(vector);
  if (!m) return null;
  if (["VC", "VI", "VA", "SC", "SI", "SA"].every((k) => m[k] === "N")) return 0;
  const eq1 =
    m.AV === "N" && m.PR === "N" && m.UI === "N"
      ? 0
      : (m.AV === "N" || m.PR === "N" || m.UI === "N") && m.AV !== "P"
        ? 1
        : 2;
  const eq2 = m.AC === "L" && m.AT === "N" ? 0 : 1;
  const eq3 =
    m.VC === "H" && m.VI === "H" ? 0 : m.VC === "H" || m.VI === "H" || m.VA === "H" ? 1 : 2;
  const eq4 =
    m.SI === "S" || m.SA === "S" ? 0 : m.SC === "H" || m.SI === "H" || m.SA === "H" ? 1 : 2;
  const eq5 = { A: 0, P: 1, U: 2 }[m.E];
  const eq6 =
    (m.CR === "H" && m.VC === "H") ||
    (m.IR === "H" && m.VI === "H") ||
    (m.AR === "H" && m.VA === "H")
      ? 0
      : 1;
  const macro = [eq1, eq2, eq3, eq4, eq5, eq6];
  const look = (i) => V4_LOOKUP.get(macro.map((v, k) => (k === i ? v + 1 : v)).join(""));
  const value = V4_LOOKUP.get(macro.join(""));
  const lower36 =
    eq3 === 0 && eq6 === 0
      ? Math.max(look(5) ?? -Infinity, look(2) ?? -Infinity)
      : eq3 === 1 && eq6 === 0
        ? look(5)
        : look(2);
  const lowers = [look(0), look(1), lower36 === -Infinity ? undefined : lower36, look(3), look(4)];

  const eq36 = `${eq3}${eq6}`;
  const level = (k, v) => LEVELS[k][v];
  let dist = {};
  search: for (const a of MAX_COMPOSED.eq1[eq1])
    for (const b of MAX_COMPOSED.eq2[eq2])
      for (const c of MAX_COMPOSED.eq3eq6[eq36])
        for (const d of MAX_COMPOSED.eq4[eq4])
          for (const e of MAX_COMPOSED.eq5[eq5]) {
            const max = metrics(`x/${a}/${b}/${c}/${d}/${e}`);
            dist = Object.fromEntries(
              Object.keys(LEVELS)
                .filter((k) => k !== "E")
                .map((k) => [k, level(k, m[k]) - level(k, max[k])]),
            );
            if (Object.values(dist).every((x) => x >= 0)) break search;
          }
  const step = 0.1;
  const current = [
    dist.AV + dist.PR + dist.UI,
    dist.AC + dist.AT,
    dist.VC + dist.VI + dist.VA + dist.CR + dist.IR + dist.AR,
    dist.SC + dist.SI + dist.SA,
    0,
  ];
  const maxSeverity = [
    MAX_SEVERITY.eq1[eq1] * step,
    MAX_SEVERITY.eq2[eq2] * step,
    MAX_SEVERITY.eq3eq6[eq36] * step,
    MAX_SEVERITY.eq4[eq4] * step,
    1,
  ];
  let n = 0;
  let sum = 0;
  lowers.forEach((lower, k) => {
    if (lower === undefined) return;
    n += 1;
    sum += (value - lower) * (current[k] / maxSeverity[k]);
  });
  const score = Math.min(Math.max(value - (n ? sum / n : 0), 0), 10);
  return Math.round((score + 10e-6) * 10) / 10;
}

// The base score of a CVSS 3.x or 4.0 vector, or null for anything else.
export const cvssScore = (vector) => cvss3Score(vector) ?? cvss4Score(vector);

export function cvssRating(score) {
  if (score === null) return "unrated";
  if (score >= 9) return "critical";
  if (score >= 7) return "high";
  if (score >= 4) return "medium";
  return score > 0 ? "low" : "none";
}
