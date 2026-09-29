// Consistency checks on adapter status that the schema can't express,
// matching the Rust client's validate_status.
import type { Capability, Status, Transport } from "./types.ts";
import { HEARTBEAT_INTERVAL_MS, MAX_LINE_BYTES } from "./types.ts";

const HEARTBEAT_TIMEOUT_MS = 15000;
const unique = <T>(items: T[]) => new Set(items).size === items.length;
const within = (n: number, min: number, max: number) => n >= min && n <= max;

/** Null when `status` is consistent with itself and `capabilities`. */
export function statusProblem(status: Status, capabilities: Capability[]): string | null {
  const l = status.limits;
  const c = status.counts;
  const transports = (["classic", "ble"] as Transport[]).filter((t) => capabilities.includes(t));
  const pairing = status.capacity.pairing.map((p) => p.transport);
  const checks: [boolean, string][] = [
    [unique(capabilities), "duplicate capabilities"],
    [status.protocol === 1, "unsupported protocol"],
    [!!status.adapter_id && !!status.boot_id && !!status.session_id, "missing identity"],
    [
      l.max_line_bytes === MAX_LINE_BYTES &&
        within(l.max_pending_requests, 4, 64) &&
        within(l.saved_devices, 1, 65535) &&
        within(l.scan_candidates, 1, 256) &&
        within(l.hidpp_settings, 1, 64) &&
        within(l.hidpp_saved_settings, 1, l.hidpp_settings) &&
        within(l.hidpp_setting_choices, 1, 65536) &&
        within(l.hidpp_features, 1, 256) &&
        l.hidpp_sensors <= 16 &&
        l.hidpp_firmware_entities <= 16,
      "invalid limits",
    ],
    [
      c.saved <= l.saved_devices &&
        c.paired <= c.saved &&
        c.preferred_enabled <= c.saved &&
        c.enabled <= c.preferred_enabled &&
        c.enabled <= c.paired &&
        c.connected <= c.enabled &&
        c.connected <= l.active_connections,
      "inconsistent counts",
    ],
    [
      status.capacity.enabled.every(
        (e) => e.transports.length > 0 && unique(e.transports) && e.transports.every((t) => capabilities.includes(t)) && e.enabled <= e.limit,
      ) &&
        unique(pairing) &&
        pairing.length === transports.length &&
        pairing.every((t) => capabilities.includes(t)) &&
        status.capacity.pairing.every((p) => p.available === (p.reason === null)),
      "capacity inconsistent with capabilities",
    ],
    [status.heartbeat.interval_ms === HEARTBEAT_INTERVAL_MS && status.heartbeat.timeout_ms === HEARTBEAT_TIMEOUT_MS, "unexpected heartbeat timing"],
  ];
  return checks.find(([ok]) => !ok)?.[1] ?? null;
}
