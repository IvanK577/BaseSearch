/** @vitest-environment jsdom */

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { AnalyticsMeasures, AnalyticsMonthRow } from "../api/types";
import { CurrencySummary, MonthChart, WeightSummary } from "./analytics";

const exclusions = {
  value_without_known_currency: 0,
  net_weight_without_known_unit: 0,
  gross_weight_without_known_unit: 0,
  ratio_without_known_currency: 0,
  ratio_without_known_weight_unit: 0,
  ratio_with_zero_or_missing_weight: 0,
};

function measures(overrides: Partial<AnalyticsMeasures> = {}): AnalyticsMeasures {
  return {
    currency_totals: [],
    net_weight_totals: [],
    gross_weight_totals: [],
    value_per_net_weight: [],
    compatible_value_total: null,
    compatible_value_per_net_weight: null,
    exclusions,
    ...overrides,
  };
}

afterEach(cleanup);

describe("analytics measures", () => {
  it("renders mixed currencies separately and normalizes gram weights without calling them kg", () => {
    const view = render(
      <>
        <CurrencySummary
          measures={measures({
            currency_totals: [
              { currency: "USD", known: true, valued_rows: 2, total_value: 100 },
              { currency: "EUR", known: true, valued_rows: 1, total_value: 200 },
            ],
          })}
        />
        <WeightSummary
          totals={[
            {
              source_unit: "g",
              known: true,
              normalized_unit: "kg",
              factor_to_kg: 0.001,
              weighted_rows: 3,
              total_source_weight: 100_000,
              total_kg: 100,
            },
          ]}
        />
      </>,
    );

    expect(view.getByText("USD").previousSibling?.textContent).toBe("100");
    expect(view.getByText("EUR").previousSibling?.textContent).toBe("200");
    expect(view.container.textContent).toContain("100.0K g");
    expect(view.container.textContent).toContain("100 kg");
    expect(view.container.textContent).not.toContain("100,000 kg");
  });

  it("declines to chart incompatible month currencies", () => {
    const months: AnalyticsMonthRow[] = [
      {
        month: "2025-01",
        rows: 2,
        declarations: 1,
        total_net_kg: 0,
        measures: measures({
          compatible_value_total: {
            currency: "USD",
            known: true,
            valued_rows: 2,
            total_value: 100,
          },
        }),
      },
      {
        month: "2025-02",
        rows: 3,
        declarations: 1,
        total_net_kg: 0,
        measures: measures({
          compatible_value_total: {
            currency: "EUR",
            known: true,
            valued_rows: 3,
            total_value: 120,
          },
        }),
      },
    ];

    const view = render(<MonthChart months={months} metric="value" />);
    expect(view.getByText("Not comparable across currencies or units.")).toBeTruthy();
  });
});
