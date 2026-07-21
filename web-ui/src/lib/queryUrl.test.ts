import { describe, expect, it } from "vitest";

import { emptyQuery, type Query } from "../api/types";
import { decodeQuery, encodeQuery } from "./queryUrl";

describe("query URL contract", () => {
  it("round-trips a nested universal query without changing it", () => {
    const query: Query = {
      ...emptyQuery(),
      text: "Apple Україна",
      filters: { ...emptyQuery().filters, year: "2024", origin_country: "CN" },
      advanced: {
        Group: {
          op: "Or",
          negated: true,
          children: [
            {
              Condition: {
                field: { Extra: "Contract value" },
                op: "Range",
                value: { Range: { from: "1000", to: "5000" } },
                negated: false,
              },
            },
          ],
        },
      },
      record_scope: "occurrences",
    };

    expect(decodeQuery(encodeQuery(query))).toEqual(query);
  });

  it("rejects malformed and oversized query state", () => {
    expect(decodeQuery("not json")).toBeNull();
    expect(decodeQuery(JSON.stringify({ text: 42, filters: {} }))).toBeNull();
    expect(decodeQuery("x".repeat(40_000))).toBeNull();
  });
});
