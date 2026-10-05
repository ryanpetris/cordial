import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { DeviceWarningSchema, DeviceWarningsSchema, ReportType, WarningCode } from "@cordial/protocol";
import { describe, expect, it } from "vitest";
import { warning } from "../src/core/convert.ts";
import type { DeviceWarning } from "../src/shared/state.ts";
import { warningFact } from "../src/shared/text.ts";
import { openSession, until } from "./helpers.ts";

const field: DeviceWarning = {
  code: "numeric_selector_unsupported", service: 1, reportId: 3, reportType: "input", bitOffset: 16, usagePage: 0x0c, usage: 0x238,
};

describe("device warnings", () => {
  it("reads every device's list and follows its warnings events", async () => {
    const { fake, session } = await openSession();
    await until(() => session.listed && session.warnings.size === fake.devices.length);
    expect(session.warnings.get(1)).toEqual([]);
    const selectors = [field, { ...field, usage: 0x239 }, { ...field, usagePage: 1 }];
    fake.changeWarnings(1, selectors);
    await until(() => session.warnings.get(1)!.length === 3);
    // In the adapter's order: service, report type, report ID, bit offset, usage page, usage, code.
    expect(session.warnings.get(1)).toEqual([selectors[2], selectors[0], selectors[1]]);
    // Events carry only what was added and removed.
    fake.changeWarnings(1, [selectors[0]!, { ...field, bitOffset: 8 }]);
    await until(() => session.warnings.get(1)!.length === 2);
    expect(session.warnings.get(1)).toEqual([{ ...field, bitOffset: 8 }, field]);
    const event = fake.events.findLast((e) => e.case === "warningsChanged");
    expect(event?.case === "warningsChanged" && [event.value.added?.length, event.value.removed?.length]).toEqual([1, 2]);
    fake.changeWarnings(1, []);
    await until(() => session.warnings.get(1)!.length === 0);
    // A disconnected device has none.
    fake.changeWarnings(1, selectors);
    await until(() => session.warnings.get(1)!.length === 3);
    fake.changeDevice(1, { state: "disconnected" });
    await until(() => session.warnings.get(1)!.length === 0);
    await session.close();
  });

  it("reads every page of a device's warnings", async () => {
    const { fake, session } = await openSession({ pageSize: 2 });
    await until(() => session.listed && session.warnings.size === fake.devices.length);
    const many = [1, 2, 3, 4, 5].map((bit) => ({ ...field, bitOffset: bit }));
    fake.changeWarnings(1, many);
    await until(() => session.warnings.get(1)!.length === 5);
    const sent = fake.received.length;
    session.reload(1);
    await until(() => fake.received.slice(sent).some((r) => r.command.case === "listSettings"));
    expect(session.warnings.get(1)).toEqual(many);
    const reads = fake.received.slice(sent).filter((r) => r.command.case === "listWarnings");
    expect(reads.map((r) => r.command.case === "listWarnings" && r.command.value.after?.bitOffset)).toEqual([undefined, 2, 4]);
    await session.close();
  });

    it("reads a list sent without warnings as having none", async () => {
    const bytes = toBinary(DeviceWarningsSchema, create(DeviceWarningsSchema, { device: 1 }));
    expect(fromBinary(DeviceWarningsSchema, bytes).warnings.map(warning)).toEqual([]);
    const { fake, session } = await openSession();
    await until(() => session.listed && session.warnings.size === fake.devices.length);
    fake.changeWarnings(1, [field]);
    await until(() => session.warnings.get(1)!.length === 1);
    fake.changeWarnings(1, []);
    await until(() => session.warnings.get(1)!.length === 0);
    await session.close();
  });

  it("reads optional fields as missing and unknown codes as unknown", () => {
    const wire = create(DeviceWarningSchema, { code: 999 as WarningCode, service: 2, reportType: 77 as ReportType });
    expect(warning(wire)).toEqual({ code: "unknown", service: 2, reportType: null, reportId: null, bitOffset: null, usagePage: null, usage: null });
    // A zero report ID is present, not missing.
    const zero = create(DeviceWarningSchema, { code: WarningCode.INDICATOR_WRITE_FAILED, reportId: 0 });
    expect(toBinary(DeviceWarningSchema, zero).length).toBeGreaterThan(2);
    expect(warning(zero).reportId).toBe(0);
  });

  it("names the field with its service, report, bit and usage", () => {
    expect(warningFact(field)).toEqual({
      label: "Input Field",
      text: "The adapter can't derive a value from this numeric selector field.",
      context: "Service 1, Input Report 3, Bit 16, Usage 000C:0238",
    });
  });

  it("names the report by type and ID, by type alone, or by ID alone", () => {
    const lights: DeviceWarning = {
      code: "indicator_write_failed", service: 1, reportId: 3, reportType: "output", bitOffset: 16, usagePage: 8, usage: 2,
    };
    expect(warningFact(lights).context).toBe("Service 1, Output Report 3, Bit 16, Usage 0008:0002");
    expect(warningFact({ ...lights, reportId: null }).context).toBe("Service 1, Output Report, Bit 16, Usage 0008:0002");
    expect(warningFact({ ...lights, reportType: "feature" }).context).toBe("Service 1, Feature Report 3, Bit 16, Usage 0008:0002");
    expect(warningFact({ ...lights, reportType: null }).context).toBe("Service 1, Report 3, Bit 16, Usage 0008:0002");
    expect(warningFact({ ...lights, reportType: null, reportId: null }).context).toBe("Service 1, Bit 16, Usage 0008:0002");
  });

  it("names a report-level indicator problem without field details", () => {
    const report: DeviceWarning = {
      code: "indicator_write_failed", service: 0, reportId: 0, reportType: null, bitOffset: null, usagePage: null, usage: null,
    };
    expect(warningFact(report)).toEqual({
      label: "Lock Indicators",
      text: "The adapter couldn't update the indicator lights.",
      context: "Service 0, Report 0",
    });
  });

  it("labels a warning this app doesn't know as a device warning", () => {
    const w: DeviceWarning = { ...field, code: "unknown" };
    expect(warningFact(w).label).toBe("Device Warning");
    expect(warningFact(w).text).toBe("The adapter can't use part of this device.");
  });

  it("keeps a usage page reported without a usage", () => {
    const w: DeviceWarning = {
      ...field, code: "indicator_range_unsupported", reportId: null, reportType: null, bitOffset: null, usage: null, usagePage: 0x08,
    };
    expect(warningFact(w)).toEqual({
      label: "Lock Indicators",
      text: "The indicator's value range can't represent both states.",
      context: "Service 1, Usage 0008",
    });
  });
});
