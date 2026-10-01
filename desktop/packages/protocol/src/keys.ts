// Information and setting keys from proto/keys.toml. A key containing `{n}`
// is a template for a part of the device that repeats; `lookup` matches a
// concrete key such as `pointer.sensor.1.dpi` to its template and index.
import { KEYS } from "./gen/keys.ts";

export * from "./gen/keys.ts";

export type KeyType = "bool" | "integer" | "enum" | "text" | "color";

/** One catalog entry. */
export interface KeyEntry {
  /** The key, or its template with `{n}` in place of an index. */
  key: string;
  /** Whether the key can appear in `Status.info`. */
  adapter: boolean;
  /** Whether the key can appear in `Device.info`. */
  device: boolean;
  /** Whether the key can be a setting. */
  setting: boolean;
  type: KeyType;
  unit: string | null;
  /** Enum values in display order. */
  values: readonly string[];
}

/** The catalog entry for `key`, and the index it carries when the entry is a template. */
export function lookup(key: string): { entry: KeyEntry; index: number | null } | null {
  const levels = key.split(".");
  for (const entry of KEYS) {
    const patterns = entry.key.split(".");
    if (patterns.length !== levels.length) continue;
    let index: number | null = null;
    const matches = patterns.every((pattern, i) => {
      const level = levels[i]!;
      if (pattern !== "{n}") return pattern === level;
      if (!/^[0-9]+$/.test(level)) return false;
      index = Number(level);
      return Number.isSafeInteger(index);
    });
    if (matches) return { entry, index };
  }
  return null;
}

/** The concrete key for index `n` of a template such as `pointer.sensor.{n}.dpi`. */
export const indexed = (template: string, n: number) => template.replace("{n}", String(n));
