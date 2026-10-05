// The orders the Dongle lists entries in, so a client can merge pages and changes into what it has
// listed and keep it in the same order.
import type { DeviceWarning, Feature, FeatureRef, ProfileRule, Setting, SettingRef, Usage } from "@cordial/protocol";

const encoder = new TextEncoder();

/** Compares two strings by their UTF-8 bytes. */
export function compareBytes(a: string, b: string): number {
  if (a === b) return 0;
  const x = encoder.encode(a);
  const y = encoder.encode(b);
  const n = Math.min(x.length, y.length);
  for (let i = 0; i < n; i++) if (x[i] !== y[i]) return x[i]! - y[i]!;
  return x.length - y.length;
}

const compareNumbers = (a: number, b: number) => (a < b ? -1 : a > b ? 1 : 0);
/** A missing value orders before any value. */
const compareOptional = (a: number | undefined, b: number | undefined) =>
  a === undefined ? (b === undefined ? 0 : -1) : b === undefined ? 1 : compareNumbers(a, b);

/** The key a setting is listed by. */
export const settingRef = (s: Pick<Setting, "integration" | "key">): Pick<SettingRef, "integration" | "key"> => ({ integration: s.integration, key: s.key });

/** Settings order: integration, then key compared bytewise. */
export const compareSettingRefs = (a: Pick<SettingRef, "integration" | "key">, b: Pick<SettingRef, "integration" | "key">) =>
  compareNumbers(a.integration, b.integration) || compareBytes(a.key, b.key);

type WarningKey = Pick<DeviceWarning, "code" | "service" | "reportType" | "reportId" | "bitOffset" | "usagePage" | "usage">;

/** Warnings order: service, report type, report ID, bit offset, usage page, usage, then code. */
export const compareWarnings = (a: WarningKey, b: WarningKey) =>
  compareNumbers(a.service, b.service)
  || compareNumbers(a.reportType, b.reportType)
  || compareOptional(a.reportId, b.reportId)
  || compareOptional(a.bitOffset, b.bitOffset)
  || compareOptional(a.usagePage, b.usagePage)
  || compareOptional(a.usage, b.usage)
  || compareNumbers(a.code, b.code);

/** Rules order: usage page, then usage of the input. */
export const compareUsages = (a: Pick<Usage, "usagePage" | "usage">, b: Pick<Usage, "usagePage" | "usage">) =>
  compareNumbers(a.usagePage, b.usagePage) || compareNumbers(a.usage, b.usage);

/** The input a rule is listed by; a rule without one orders first. */
export const ruleInput = (r: Pick<ProfileRule, "input">): Pick<Usage, "usagePage" | "usage"> => ({ usagePage: r.input?.usagePage ?? 0, usage: r.input?.usage ?? 0 });

/** The key a feature is listed by. */
export const featureRef = (f: Feature): Pick<FeatureRef, "integration" | "index"> => ({
  integration: f.integration,
  index: f.detail.case === "hidpp" ? f.detail.value.index : 0,
});

/** Features order: integration, then index within the integration's table. */
export const compareFeatureRefs = (a: Pick<FeatureRef, "integration" | "index">, b: Pick<FeatureRef, "integration" | "index">) =>
  compareNumbers(a.integration, b.integration) || compareNumbers(a.index, b.index);
