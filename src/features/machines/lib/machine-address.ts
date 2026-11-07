// SPDX-License-Identifier: MIT
/** An SSH target as typed in the Add-machine "Where" step: `user@host[:port]`. */
interface MachineAddress {
  username: string | null;
  host: string;
  port: number;
}

const DEFAULT_SSH_PORT = 22;

function parsePort(raw: string): number | null {
  if (!/^\d+$/.test(raw)) return null;
  const port = Number(raw);
  return port >= 1 && port <= 65535 ? port : null;
}

/** A bare IPv6 address: up to eight 1–4-digit hex groups, compressed by at most
 *  one `::`, with an optional zone id (`fe80::1%en0`). */
const IPV6_GROUPS = /^[0-9a-f]{0,4}(:[0-9a-f]{0,4}){2,7}$/i;
function isBareIpv6(raw: string): boolean {
  const [addr = "", zone] = raw.split("%");
  if (zone !== undefined && !/^[\w.]+$/.test(zone)) return false;
  if (!IPV6_GROUPS.test(addr) || addr.includes(":::")) return false;
  const compressions = addr.split("::").length - 1;
  if (compressions > 1) return false;
  const groups = addr.split(":").filter(Boolean).length;
  return compressions === 1 ? groups >= 1 && groups <= 7 : groups === 8;
}

/** A value that would read as an ssh/scp OPTION if it reached an argv. */
const LEADING_DASH = /^-/;

/** Split `host[:port]` — `[v6]:port` bracketed, and a bare IPv6 (`fe80::1`) as a
 *  host on the default port. */
function splitHostPort(rest: string): { host: string; port: number } | null {
  if (rest.startsWith("[")) {
    const end = rest.indexOf("]");
    if (end < 0) return null;
    const after = rest.slice(end + 1);
    const port =
      after === "" ? DEFAULT_SSH_PORT : after.startsWith(":") ? parsePort(after.slice(1)) : null;
    return port === null ? null : { host: rest.slice(1, end), port };
  }
  const colons = rest.split(":").length - 1;
  if (colons === 0) return { host: rest, port: DEFAULT_SSH_PORT };
  // Two or more colons is only a bare IPv6 address — anything else is malformed.
  if (colons > 1) return isBareIpv6(rest) ? { host: rest, port: DEFAULT_SSH_PORT } : null;
  const [host, rawPort] = rest.split(":");
  const port = parsePort(rawPort ?? "");
  return port === null ? null : { host: host ?? "", port };
}

/** Parse `user@host[:port]` (user and port optional). Returns null when there is no
 *  host or the port isn't a valid TCP port. */
export function parseMachineAddress(input: string): MachineAddress | null {
  const trimmed = input.trim();
  if (!trimmed) return null;
  const at = trimmed.lastIndexOf("@");
  const username = at > 0 ? trimmed.slice(0, at) : null;
  if (username !== null && (/\s|@/.test(username) || LEADING_DASH.test(username))) return null;
  const split = splitHostPort(at >= 0 ? trimmed.slice(at + 1) : trimmed);
  if (!split) return null;
  const host = split.host.trim();
  if (!host || /\s|\//.test(host) || LEADING_DASH.test(host)) return null;
  return { username, host, port: split.port };
}

/** The inverse of {@link parseMachineAddress}; the port shows only when non-default. */
export function formatMachineAddress({ username, host, port }: MachineAddress): string {
  const user = username ? `${username}@` : "";
  if (port === DEFAULT_SSH_PORT) return `${user}${host}`;
  return `${user}${host.includes(":") ? `[${host}]` : host}:${port}`;
}
