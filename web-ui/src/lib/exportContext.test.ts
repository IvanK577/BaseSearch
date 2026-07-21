import { describe, expect, it } from "vitest";

import type { FieldDto, Query } from "../api/types";
import type { Translate } from "./i18n";
import {
  describeExportQuery,
  moveFieldId,
  selectAllFieldIds,
} from "./exportContext";

const fields = [
  { id: "a", label: "Alpha" },
  { id: "b", label: "Beta" },
  { id: "c", label: "Gamma" },
] as FieldDto[];

const testMessages: Record<string, string> = {
  exports_query_text: "Text: “{value}”",
  filter_year: "Year",
  exports_query_advanced_one: "1 advanced rule",
  exports_scope_occurrences: "All source occurrences",
};

const t = ((key: string, values?: Record<string, string | number>) => {
  const message = testMessages[key] ?? key;
  return message.replace(/\{(\w+)\}/g, (placeholder, name: string) =>
    values && Object.prototype.hasOwnProperty.call(values, name)
      ? String(values[name])
      : placeholder,
  );
}) as Translate;

describe("contextual export helpers", () => {
  it("selects all fields without discarding the user's current order", () => {
    expect(selectAllFieldIds(fields, ["c", "a"])).toEqual(["c", "a", "b"]);
  });

  it("moves selected fields without crossing list boundaries", () => {
    expect(moveFieldId(["a", "b", "c"], "b", -1)).toEqual(["b", "a", "c"]);
    expect(moveFieldId(["a", "b", "c"], "a", -1)).toEqual(["a", "b", "c"]);
    expect(moveFieldId(["a", "b", "c"], "c", 1)).toEqual(["a", "b", "c"]);
  });

  it("summarizes the applied text, filters, advanced rules and record scope", () => {
    const query: Query = {
      text: "Apple",
      filters: {
        year: "2024",
        product_code: "",
        trademark: "",
        description: "",
        sender: "",
        recipient: "",
        edrpou: "",
        trade_country: "",
        dispatch_country: "",
        origin_country: "",
      },
      advanced: {
        Condition: {
          field: { Column: "recipient" },
          op: "Contains",
          value: { Single: "Retail" },
          negated: false,
        },
      },
      record_scope: "occurrences",
    };

    expect(describeExportQuery(query, t)).toEqual({
      summary: "Text: “Apple” · Year: 2024 · 1 advanced rule",
      scope: "All source occurrences",
    });
  });
});
