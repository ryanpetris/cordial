import { mkdirSync, writeFileSync } from "node:fs";
import { expect, it } from "vitest";
import type { ValidateFunction } from "ajv";
import { validator } from "../src/protocol/validators.ts";
import { openSession, until } from "./helpers.ts";

type Json = Record<string, unknown>;

it("precompiled web validators agree with the run-time ones", async () => {
  // A variable specifier keeps TypeScript from resolving the build script.
  const script = "../scripts/web-validators.mjs";
  const { validatorsSource } = (await import(script)) as { validatorsSource: () => string };
  const directory = new URL("../node_modules/.cache/cordial-tests/", import.meta.url);
  mkdirSync(directory, { recursive: true });
  const file = new URL("validators.mjs", directory);
  writeFileSync(file, validatorsSource());
  const precompiled = (await import(/* @vite-ignore */ file.href)) as { validator: (definition: string) => ValidateFunction };

  // Real traffic: a session syncing the simulated adapter's devices and settings.
  const lines: Json[] = [];
  const { fake, session } = await openSession();
  fake.onData((chunk) => lines.push(...new TextDecoder().decode(chunk).split("\n").filter(Boolean).map((l) => JSON.parse(l) as Json)));
  await until(() => session.readiness.state === "ready" && session.view.devices.size === 4);
  session.watchSettings(["d_1"]);
  await until(() => lines.some((m) => m.type === "response" && fake.received.find((r) => r.id === m.id)?.cmd === "hidpp.setting.list"));
  const commands = new Map(fake.received.map((r) => [r.id, r.cmd as string]));
  const cases: [string, unknown][] = [
    ...fake.received.map((r): [string, unknown] => [`${r.cmd as string}.request`, r]),
    ...lines.map((m): [string, unknown] => [m.type === "response" ? `${commands.get(m.id as number)}.response` : "Event", m]),
  ];
  expect(new Set(cases.map(([d]) => d)).size).toBeGreaterThan(5);
  for (const [definition, message] of cases) {
    const broken = { ...(message as Json), v: 2 };
    for (const m of [message, broken])
      expect([definition, precompiled.validator(definition)(m)]).toEqual([definition, validator(definition)(m)]);
    expect(validator(definition)(message)).toBe(true);
  }
  expect(() => precompiled.validator("nope")).toThrow("schema has no definition nope");
  await session.close();
});
