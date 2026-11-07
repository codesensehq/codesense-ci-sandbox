// SPDX-License-Identifier: MIT
/** A word `joinArgs` writes as it is: nothing in it a reader of `splitArgs` would treat
 *  specially. */
const PLAIN_WORD = /^[A-Za-z0-9_@%+=:,./~^-]+$/;

/** Quote `word` so `splitArgs` reads it back unchanged: as it is when plain, else in single
 *  quotes, with each `'` written as `'\''`. */
function quoteArg(word: string): string {
  if (PLAIN_WORD.test(word)) return word;
  return `'${word.replace(/'/g, `'\\''`)}'`;
}

/** argv words as one editable line that `splitArgs` reads back to exactly the same words. */
export function joinArgs(words: readonly string[]): string {
  return words.map(quoteArg).join(" ");
}

/**
 * Split a typed args line into argv words, shell style: whitespace separates words; `'…'`
 * keeps everything inside literally; `"…"` keeps everything inside except that `\"` and `\\`
 * are a quote and a backslash; outside quotes a backslash escapes whitespace, a quote or a
 * backslash and is otherwise kept (so `C:\dir` stays as typed). Quotes join with what touches
 * them (`--x="a b"` is one word, `--x=a b`), `''` is an empty word, and an unclosed quote runs
 * to the end of the line.
 */
export function splitArgs(input: string): string[] {
  const words: string[] = [];
  let word = "";
  let inWord = false;
  let quote: "'" | '"' | null = null;
  for (let i = 0; i < input.length; i += 1) {
    const c = input[i];
    if (quote === "'") {
      if (c === "'") quote = null;
      else word += c;
      continue;
    }
    if (quote === '"') {
      if (c === '"') quote = null;
      else if (c === "\\" && (input[i + 1] === '"' || input[i + 1] === "\\")) word += input[++i];
      else word += c;
      continue;
    }
    if (/\s/.test(c)) {
      if (inWord) words.push(word);
      word = "";
      inWord = false;
      continue;
    }
    inWord = true;
    if (c === "'" || c === '"') quote = c;
    else if (c === "\\" && i + 1 < input.length && /[\s'"\\]/.test(input[i + 1]))
      word += input[++i];
    else word += c;
  }
  if (inWord) words.push(word);
  return words;
}
