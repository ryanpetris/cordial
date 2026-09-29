// Schema validators compiled at build time for the web build, whose Content
// Security Policy forbids the run-time code generation Ajv uses. A Vite plugin
// substitutes them for src/protocol/validators.ts; nothing is checked in.
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { Ajv2020 } = require("ajv/dist/2020.js");
const standaloneCode = require("ajv/dist/standalone").default;
const schemaPath = new URL("../../schema/wire.schema.json", import.meta.url);
const ID = "\0cordial-validators";

/** An ES module exporting `validator(definition)` like src/protocol/validators.ts. */
export function validatorsSource() {
  const schema = JSON.parse(readFileSync(schemaPath, "utf8"));
  const ajv = new Ajv2020({ strict: false, validateFormats: false, allErrors: false, code: { source: true, esm: true } });
  ajv.addSchema(schema);
  // The definitions src/protocol/validate.ts checks messages against.
  const definitions = Object.keys(schema.$defs).filter((d) => d === "Event" || d.endsWith(".request") || d.endsWith(".response"));
  const exports = Object.fromEntries(definitions.map((d, i) => [`v${i}`, `${schema.$id}#/$defs/${d}`]));
  const imports = [];
  // Even ES module output requires Ajv's run-time helpers. Their default
  // export is the helper under a bundler and the CommonJS exports under Node.
  const code = standaloneCode(ajv, exports)
    .replace(/^"use strict";/, "")
    .replace(/require\("([^"]+)"\)\.default/g, (_, specifier) => {
      const name = `runtime${imports.length}`;
      imports.push(`import ${name} from ${JSON.stringify(`${specifier}.js`)};`);
      return `(${name}.default ?? ${name})`;
    });
  const table = definitions.map((d, i) => `${JSON.stringify(d)}: v${i}`).join(",\n  ");
  return `${imports.join("\n")}
${code}
const table = {
  ${table},
};
export function validator(definition) {
  const found = table[definition];
  if (!found) throw new Error(\`schema has no definition \${definition}\`);
  return found;
}
`;
}

/** Vite plugin replacing the run-time validators with precompiled ones. */
export function precompiledValidators() {
  return {
    name: "cordial-precompiled-validators",
    // Ahead of Vite's resolver, which would find the run-time module.
    enforce: "pre",
    resolveId(source, importer) {
      if (source === "./validators.ts" && importer?.replaceAll("\\", "/").endsWith("/src/protocol/validate.ts")) return ID;
      return null;
    },
    load(id) {
      return id === ID ? validatorsSource() : null;
    },
  };
}
