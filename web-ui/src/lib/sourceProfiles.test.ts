import { describe, expect, it } from "vitest";

import type { ColumnPeek, SourceMappingProfile } from "../api/types";
import {
  effectiveFixedValue,
  effectiveMapping,
  mappingSelection,
} from "./sourceProfiles";

const columns: ColumnPeek[] = [
  {
    index: 0,
    id: "alpha",
    header: "Alpha",
    sample: "ACME",
    role: "Text",
    semantic: "Sender",
  },
  {
    index: 1,
    id: "beta",
    header: "Beta",
    sample: "1250",
    role: "Number",
    semantic: "Value",
  },
];

const profile: SourceMappingProfile = {
  id: 7,
  name: "Reusable source",
  signature: `smp1:2:${"a".repeat(64)}`,
  mapping: ["Recipient", null],
  fixed_values: { Currency: "USD", WeightUnit: "kg" },
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

describe("source mapping profile composition", () => {
  it("lets explicit per-column choices win over a selected profile", () => {
    const overrides = { 0: "Description" as const };
    expect(mappingSelection(0, overrides, profile)).toBe("Description");
    expect(mappingSelection(1, overrides, profile)).toBe(null);
    expect(effectiveMapping(columns, overrides, profile)).toEqual([
      "Description",
      null,
    ]);
  });

  it("saves auto-detected semantics when no profile or override is active", () => {
    expect(effectiveMapping(columns, {}, null)).toEqual(["Sender", "Value"]);
  });

  it("lets an explicit fixed value override the profile default", () => {
    expect(effectiveFixedValue("Currency", {}, profile)).toBe("USD");
    expect(
      effectiveFixedValue("Currency", { Currency: "EUR" }, profile),
    ).toBe("EUR");
  });
});
