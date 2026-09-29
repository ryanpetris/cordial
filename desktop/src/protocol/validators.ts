// Schema validators compiled from the checked-in wire schema at run time.
// The web build replaces this module with validators compiled at build time
// (scripts/web-validators.mjs): its Content Security Policy forbids the code
// generation Ajv uses here.
import { Ajv2020 } from "ajv/dist/2020.js";
import type { ValidateFunction } from "ajv";
import schema from "../../../schema/wire.schema.json" with { type: "json" };

const ajv = new Ajv2020({ strict: false, validateFormats: false, allErrors: false });
ajv.addSchema(schema);
const cache = new Map<string, ValidateFunction>();

/** The validator of a definition in the schema's `$defs`. */
export function validator(definition: string): ValidateFunction {
  let found = cache.get(definition);
  if (!found) {
    const compiled = ajv.getSchema(`urn:cordial:wire:1#/$defs/${definition}`);
    if (!compiled) throw new Error(`schema has no definition ${definition}`);
    found = compiled;
    cache.set(definition, found);
  }
  return found;
}
