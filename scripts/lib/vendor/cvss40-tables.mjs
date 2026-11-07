// SPDX-License-Identifier: BSD-2-Clause
// The CVSS v4.0 scoring tables of FIRST's reference calculator, cvss-v4-calculator
// (https://github.com/FIRSTdotorg/cvss-v4-calculator, commit
// c5b0d409ae9f57c44264c6ce5f27d89298e1d32a: cvss_lookup.js, max_composed.js, max_severity.js and the
// metric levels of cvss_score.js). Data only; scripts/lib/cvss.mjs does the scoring.
//
// Copyright (c) 2023 FIRST.ORG, Inc., Red Hat, and contributors
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this
//    list of conditions and the following disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice,
//    this list of conditions and the following disclaimer in the documentation
//    and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
// FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
// DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
// CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
// OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
// OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

// Severity levels (distance from the most severe value) of the metrics the score uses.
export const LEVELS = {
  AV: { N: 0, A: 0.1, L: 0.2, P: 0.3 },
  AC: { L: 0, H: 0.1 },
  AT: { N: 0, P: 0.1 },
  PR: { N: 0, L: 0.1, H: 0.2 },
  UI: { N: 0, P: 0.1, A: 0.2 },
  VC: { H: 0, L: 0.1, N: 0.2 },
  VI: { H: 0, L: 0.1, N: 0.2 },
  VA: { H: 0, L: 0.1, N: 0.2 },
  SC: { H: 0.1, L: 0.2, N: 0.3 },
  SI: { S: 0, H: 0.1, L: 0.2, N: 0.3 },
  SA: { S: 0, H: 0.1, L: 0.2, N: 0.3 },
  E: { A: 0, P: 0.1, U: 0.2 },
  CR: { H: 0, M: 0.1, L: 0.2 },
  IR: { H: 0, M: 0.1, L: 0.2 },
  AR: { H: 0, M: 0.1, L: 0.2 },
};

// cvss_lookup.js: the score of each macrovector eq1..eq6, in order (eq1 0-2, eq2 0-1, eq3 0-2,
// eq4 0-2, eq5 0-2, eq6 0-1, without eq3 2 with eq6 0).
// prettier-ignore
export const LOOKUP_VALUES = [10.0, 9.9, 9.8, 9.5, 9.5, 9.2, 10.0, 9.6, 9.3, 8.7, 9.1, 8.1, 9.3, 9.0, 8.9, 8.0, 8.1, 6.8, 9.8, 9.5, 9.5, 9.2, 9.0, 8.4, 9.3, 9.2, 8.9, 8.1, 8.1, 6.5, 8.8, 8.0, 7.8, 7.0, 6.9, 4.8, 9.2, 8.2, 7.2, 7.9, 6.9, 5.0, 6.9, 5.5, 2.7, 9.9, 9.7, 9.5, 9.2, 9.2, 8.5, 9.5, 9.1, 9.0, 8.3, 8.4, 7.1, 9.2, 8.1, 8.2, 7.1, 7.2, 5.3, 9.5, 9.3, 9.2, 8.5, 8.5, 7.3, 9.2, 8.2, 8.0, 7.2, 7.0, 5.9, 8.4, 7.0, 7.1, 5.2, 5.0, 3.0, 8.6, 7.5, 5.2, 7.1, 5.2, 2.9, 6.3, 2.9, 1.7, 9.8, 9.5, 9.4, 8.7, 9.1, 8.1, 9.4, 8.9, 8.6, 7.4, 7.7, 6.4, 8.7, 7.5, 7.4, 6.3, 6.3, 4.9, 9.4, 8.9, 8.8, 7.7, 7.6, 6.7, 8.6, 7.6, 7.4, 5.8, 5.9, 5.0, 7.2, 5.7, 5.7, 5.2, 5.2, 2.5, 8.3, 7.0, 5.4, 6.5, 5.8, 2.6, 5.3, 2.1, 1.3, 9.5, 9.0, 8.8, 7.6, 7.6, 7.0, 9.0, 7.7, 7.5, 6.2, 6.1, 5.3, 7.7, 6.6, 6.8, 5.9, 5.2, 3.0, 8.9, 7.8, 7.6, 6.7, 6.2, 5.8, 7.4, 5.9, 5.7, 5.7, 4.7, 2.3, 6.1, 5.2, 5.7, 2.9, 2.4, 1.6, 7.1, 5.9, 3.0, 5.8, 2.6, 1.5, 2.3, 1.3, 0.6, 9.3, 8.7, 8.6, 7.2, 7.5, 5.8, 8.6, 7.4, 7.4, 6.1, 5.6, 3.4, 7.0, 5.4, 5.2, 4.0, 4.0, 2.2, 8.5, 7.5, 7.4, 5.5, 6.2, 5.1, 7.2, 5.7, 5.5, 4.1, 4.6, 1.9, 5.3, 3.6, 3.4, 1.9, 1.9, 0.8, 6.4, 5.1, 2.0, 4.7, 2.1, 1.1, 2.4, 0.9, 0.4, 8.8, 7.5, 7.3, 5.3, 6.0, 5.0, 7.3, 5.5, 5.9, 4.0, 4.1, 2.0, 5.4, 4.3, 4.5, 2.2, 2.0, 1.1, 7.5, 5.5, 5.8, 4.5, 4.0, 2.1, 6.1, 5.1, 4.8, 1.8, 2.0, 0.9, 4.6, 1.8, 1.7, 0.7, 0.8, 0.2, 5.3, 2.4, 1.4, 2.4, 1.2, 0.5, 1.0, 0.3, 0.1];

// max_composed.js: the highest-severity vectors of each equivalence class.
export const MAX_COMPOSED = {
  eq1: [
    ["AV:N/PR:N/UI:N"],
    ["AV:A/PR:N/UI:N", "AV:N/PR:L/UI:N", "AV:N/PR:N/UI:P"],
    ["AV:P/PR:N/UI:N", "AV:A/PR:L/UI:P"],
  ],
  eq2: [["AC:L/AT:N"], ["AC:H/AT:N", "AC:L/AT:P"]],
  eq3eq6: {
    "00": ["VC:H/VI:H/VA:H/CR:H/IR:H/AR:H"],
    "01": ["VC:H/VI:H/VA:L/CR:M/IR:M/AR:H", "VC:H/VI:H/VA:H/CR:M/IR:M/AR:M"],
    10: ["VC:L/VI:H/VA:H/CR:H/IR:H/AR:H", "VC:H/VI:L/VA:H/CR:H/IR:H/AR:H"],
    11: [
      "VC:L/VI:H/VA:L/CR:H/IR:M/AR:H",
      "VC:L/VI:H/VA:H/CR:H/IR:M/AR:M",
      "VC:H/VI:L/VA:H/CR:M/IR:H/AR:M",
      "VC:H/VI:L/VA:L/CR:M/IR:H/AR:H",
      "VC:L/VI:L/VA:H/CR:H/IR:H/AR:M",
    ],
    21: ["VC:L/VI:L/VA:L/CR:H/IR:H/AR:H"],
  },
  eq4: [["SC:H/SI:S/SA:S"], ["SC:H/SI:H/SA:H"], ["SC:L/SI:L/SA:L"]],
  eq5: [["E:A"], ["E:P"], ["E:U"]],
};

// max_severity.js: the depth of each equivalence class, in tenths.
export const MAX_SEVERITY = {
  eq1: [1, 4, 5],
  eq2: [1, 2],
  eq3eq6: { "00": 7, "01": 6, 10: 8, 11: 8, 21: 10 },
  eq4: [6, 5, 4],
};
