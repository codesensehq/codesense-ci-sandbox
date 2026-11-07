// SPDX-License-Identifier: MIT
const SHELL_META = /[ '"\\\t()$`!&|;*?[\]{}<>~#]/g;

export function shellEscape(path: string): string {
  return path.replace(SHELL_META, (ch) => `\\${ch}`);
}

const C0_START = String.fromCharCode(0x00);
const C0_END = String.fromCharCode(0x1f);
const DEL = String.fromCharCode(0x7f);
const CONTROL_CHARS = new RegExp(`[${C0_START}-${C0_END}${DEL}]`, "g");

export function stripControlChars(s: string): string {
  return s.replace(CONTROL_CHARS, "");
}
