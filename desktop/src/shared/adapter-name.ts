/** Canonical device-owned name, or null when invalid. */
export function adapterName(value: string): string | null {
  if (/\p{Cc}/u.test(value) || !value.isWellFormed()) return null;
  const name = value.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
  return name && new TextEncoder().encode(name).length <= 64 ? name : null;
}
