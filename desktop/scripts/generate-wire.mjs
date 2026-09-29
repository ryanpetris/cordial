// Generates TypeScript wire types from the checked-in protocol schema.
// `--check` fails when the checked-in output differs from regeneration.
import { compile } from "json-schema-to-typescript";
import { readFileSync, writeFileSync } from "node:fs";

const root = new URL("../", import.meta.url);
const schema = JSON.parse(readFileSync(new URL("../schema/wire.schema.json", root), "utf8"));
const output = new URL("src/protocol/wire.ts", root);

// The conditional per-key constraints of InfoField don't translate into
// useful types; the full definition still validates every received message.
schema.$defs.InfoField = {
  type: "object",
  additionalProperties: false,
  required: ["key", "instance", "value", "available", "fresh"],
  properties: {
    key: { $ref: "#/$defs/InfoKey" },
    instance: { type: "integer" },
    value: { $ref: "#/$defs/SettingValue" },
    available: { type: "boolean" },
    fresh: { type: "boolean" },
  },
};

const pascal = (name) =>
  name.split(/[._]/).map((part) => part[0].toUpperCase() + part.slice(1)).join("");
const commands = Object.keys(
  JSON.parse(readFileSync(new URL("../schema/commands.json", root), "utf8")).commands,
).sort();

const text =
  (await compile(schema, "Wire", {
  bannerComment:
    "// Generated from schema/wire.schema.json by scripts/generate-wire.mjs. Do not edit.\n/* eslint-disable */",
  additionalProperties: false,
  unreachableDefinitions: true,
  strictIndexSignatures: true,
  format: true,
})) +
  "\n/** Every command's response union, by command name. */\nexport interface ResponseMap {\n" +
  commands.map((c) => `  ${JSON.stringify(c)}: ${pascal(c)}Response;\n`).join("") +
  "}\n";

if (process.argv.includes("--check")) {
  const current = readFileSync(output, "utf8");
  if (current !== text) {
    console.error("src/protocol/wire.ts is out of date; run npm run generate");
    process.exit(1);
  }
} else {
  writeFileSync(output, text);
}
