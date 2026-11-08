// SPDX-License-Identifier: MIT
// Print only scanner locators; raw credential fields never enter the output.
import fs from "node:fs";

const [tool, file, statusText, label] = process.argv.slice(2);
try {
  const status = Number(statusText);
  const text = fs.readFileSync(file, "utf8");
  let locators;
  if (tool === "gitleaks") {
    const findings = JSON.parse(text);
    if (!Array.isArray(findings)) throw new Error();
    locators = findings.map(({ File, StartLine, RuleID }) => [File, StartLine, RuleID]);
  } else if (tool === "trufflehog") {
    locators = text
      .split("\n")
      .filter((line) => line.trim())
      .map((line) => {
        const finding = JSON.parse(line);
        const source = finding.SourceMetadata?.Data?.Filesystem;
        return [source?.file, source?.line ?? 0, finding.DetectorName];
      });
  } else throw new Error();
  // Validate all entries before emitting anything, including on malformed reports.
  for (const [file, line, rule] of locators) {
    if (typeof file !== "string" || typeof rule !== "string" || !/^[0-9]+$/.test(String(line)))
      throw new Error();
  }
  const escape = (value) => JSON.stringify(value).slice(1, -1);
  for (const [file, line, rule] of locators)
    console.log(`  ${escape(file)}:${line} ${escape(rule)}`);
  console.log(`secret-scan: ${tool} ${label}: ${locators.length} findings`);
  const findingsExit = tool === "gitleaks" ? 1 : 183;
  if (status === 0 && locators.length === 0) {
    console.log(`secret-scan: ${tool} ${label} clean`);
  } else if (status === findingsExit && locators.length > 0) {
    console.error(`secret-scan: ${tool} ${label} findings detected`);
    process.exitCode = 1;
  } else throw new Error();
} catch {
  console.error(`secret-scan: ${tool} ${label} scan error (details withheld)`);
  process.exitCode = 1;
}
