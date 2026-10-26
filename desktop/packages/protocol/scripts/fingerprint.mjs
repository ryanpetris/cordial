// Canonical descriptor bytes shared with the Rust build.
import { createHash } from "node:crypto";
import { clone, toBinary, clearField } from "@bufbuild/protobuf";
import { FileDescriptorSetSchema, FileDescriptorProtoSchema } from "@bufbuild/protobuf/wkt";

const digest = bytes => createHash("sha256").update(bytes).digest("hex");

export function fingerprint(descriptors) {
  const normalized = clone(FileDescriptorSetSchema, descriptors);
  const files = new Map(normalized.file.map(file => [file.name, file]));
  const cache = new Map();
  function normalize(name) {
    if (cache.has(name)) return cache.get(name);
    const file = files.get(name);
    if (!file) throw new Error("missing operational dependency");
    clearField(file, FileDescriptorProtoSchema.field.name);
    clearField(file, FileDescriptorProtoSchema.field.sourceCodeInfo);
    file.dependency = file.dependency.map(name => digest(toBinary(FileDescriptorProtoSchema, normalize(name))));
    cache.set(name, file);
    return file;
  }
  normalized.file = [...files.keys()].map(normalize);
  normalized.file.sort((a, b) => Buffer.compare(
    toBinary(FileDescriptorProtoSchema, a), toBinary(FileDescriptorProtoSchema, b),
  ));
  return digest(toBinary(FileDescriptorSetSchema, normalized));
}
