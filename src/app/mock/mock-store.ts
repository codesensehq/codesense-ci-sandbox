// SPDX-License-Identifier: MIT
/**
 * Derive a deterministic id from a display name so the mock's create → edit flow
 * can re-find a just-created record by name alone. When `taken` is supplied, a colliding
 * slug is disambiguated with a numeric suffix (`-2`, `-3`, …).
 */
function slugify(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/-{2,}/g, "-")
    .replace(/^-+|-+$/g, "");
}

export function mintId(name: string, taken?: ReadonlySet<string>, fallback = "item"): string {
  const base = slugify(name) || fallback;
  if (!taken || !taken.has(base)) return base;
  let n = 2;
  while (taken.has(`${base}-${n}`)) n++;
  return `${base}-${n}`;
}

/** An insertion-ordered, in-memory collection of id-keyed records — the storage
 *  primitive behind every mock collection surface. */
export interface Collection<T extends { id: string }> {
  list(): T[];
  get(id: string): T | null;
  /** Insert a new record or overwrite an existing one in place (edit). */
  put(record: T): T;
  remove(id: string): boolean;
  has(id: string): boolean;
  /** The set of ids currently in use — feed to {@link mintId} to avoid clashes. */
  ids(): Set<string>;
}

export function createCollection<T extends { id: string }>(seed: readonly T[] = []): Collection<T> {
  const records = new Map<string, T>();
  for (const record of seed) records.set(record.id, record);

  return {
    list: () => [...records.values()],
    get: (id) => records.get(id) ?? null,
    put(record) {
      records.set(record.id, record);
      return record;
    },
    remove: (id) => records.delete(id),
    has: (id) => records.has(id),
    ids: () => new Set(records.keys()),
  };
}
