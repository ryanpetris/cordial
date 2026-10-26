/** Maps an unversioned historical schema to its existing storage format without changing fields. */
export function versionStorageSchema(text) {
  const identity = text.match(/message Identity\s*\{([\s\S]*?)\n\}/)?.[1];
  if (!identity) throw new Error("Historical storage schema has no Identity record");
  let version;
  if (/\buint32 format = 1;/.test(identity) && /\bbytes address = 2;/.test(identity)) version = "v2";
  else if (!/\bformat\s*=/.test(identity) && /\bbytes address = 1;/.test(identity)) version = "v1";
  else throw new Error("Historical storage schema has an unknown format");
  if (!text.includes("package cordial.storage;")) throw new Error("Unexpected historical storage package");
  return {
    path: `storage/${version}/storage.proto`,
    text: text.replace("package cordial.storage;", `package cordial.storage.${version};`),
  };
}
