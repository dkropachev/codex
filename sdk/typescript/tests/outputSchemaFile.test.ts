import { describe, expect, it } from "@jest/globals";

import { createOutputSchemaFile } from "../src/outputSchemaFile";

describe("createOutputSchemaFile", () => {
  it("rejects object schemas that omit required properties", async () => {
    const schema = {
      type: "object",
      properties: {
        summary: { type: "string" },
        testPath: { type: "string" },
        testCommand: { type: "string" },
      },
      required: ["testPath", "testCommand"],
      additionalProperties: false,
    } as const;

    await expect(createOutputSchemaFile(schema)).rejects.toThrow(
      "outputSchema at $ must list every property in required (missing required keys: summary)",
    );
  });

  it("rejects nested object schemas that omit required properties", async () => {
    const schema = {
      type: "object",
      properties: {
        repro: {
          type: "object",
          properties: {
            testPath: { type: "string" },
            testCommand: { type: "string" },
          },
          required: ["testPath"],
          additionalProperties: false,
        },
      },
      required: ["repro"],
      additionalProperties: false,
    } as const;

    await expect(createOutputSchemaFile(schema)).rejects.toThrow(
      "outputSchema at $.properties.repro must list every property in required (missing required keys: testCommand)",
    );
  });
});
