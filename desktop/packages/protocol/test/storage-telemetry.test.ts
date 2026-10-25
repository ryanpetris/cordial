import { create, fromBinary, fromJson, toBinary, toJson } from "@bufbuild/protobuf";
import { expect, it } from "vitest";
import { ResourceType, StatusSchema } from "../src/index.ts";

it("preserves independent storage presence and explicit zero in binary and raw JSON", () => {
  for (const storageFormat of [undefined, 0, 1]) {
    for (const count of [undefined, 0, 4096]) {
      for (const limit of [undefined, 0, 1 << 20]) {
        const status = create(StatusSchema, {
          information: { storageFormat },
          resources: [{ type: ResourceType.STORAGE, count, limit }],
        });
        expect(fromBinary(StatusSchema, toBinary(StatusSchema, status))).toEqual(status);
        const json = toJson(StatusSchema, status, { useProtoFieldName: true });
        expect(json).toEqual({
          information: storageFormat === undefined ? {} : { storage_format: storageFormat },
          resources: [{
            type: "RESOURCE_TYPE_STORAGE",
            ...(count === undefined ? {} : { count }),
            ...(limit === undefined ? {} : { limit }),
          }],
        });
        expect(fromJson(StatusSchema, json)).toEqual(status);
      }
    }
  }
});
