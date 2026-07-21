/** @vitest-environment jsdom */

import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Undervaluation } from "../api/types";
import { RiskResults } from "./PriceRisk";

afterEach(cleanup);

describe("RiskResults", () => {
  it("shows the exact cohort, robust threshold, explanation, confidence and limitations", () => {
    const result: Undervaluation = {
      available: true,
      rows: [
        {
          id: 42,
          declaration_date: "2024-02-15",
          declaration_number: "DOC-42",
          recipient: "Buyer A",
          sender: "Supplier A",
          edrpou: "12345678",
          product_code: "SKU-1",
          description: "Widget",
          source_value: 10,
          net_kg: 1,
          price_per_kg: 10,
          code_median: 100,
          code_p25: 90,
          code_p75: 110,
          code_sample_count: 25,
          estimated_gap: 90,
          ratio: 0.1,
          cohort: {
            product_code: "SKU-1",
            period: "2024-Q1",
            currency: "USD",
            weight_unit: "KG",
            brand: "ACME",
            country: "CN",
            dimensions: [
              "product_code",
              "period",
              "currency",
              "weight_unit",
              "brand",
              "country",
            ],
            sample_count: 25,
            median: 100,
            p25: 90,
            p75: 110,
            iqr: 20,
            lower_fence: 60,
            median_ratio_cutoff: 50,
            robust_cutoff: 50,
          },
          deviation_percent: 90,
          confidence: "low",
          reason: "Price is 90.0% below the comparable cohort median.",
          limitations: [
            {
              code: "limited_sample",
              message: "The selected cohort contains only 25 records.",
            },
          ],
        },
      ],
      checked_codes: 1,
      checked_rows: 25,
      flagged_rows: 1,
      flagged_codes: 1,
      flagged_value: 10,
      estimated_gap: 90,
      eligible_rows: 25,
      evaluated_rows: 25,
      checked_cohorts: 1,
      contract: {
        price_basis: "mapped value / mapped net weight",
        period_granularity: "calendar_quarter",
        required_dimensions: ["product_code", "period", "currency", "weight_unit"],
        optional_dimensions: ["brand", "country"],
        min_samples: 20,
        max_median_ratio: 0.5,
        iqr_multiplier: 1.5,
        includes_subject_record: true,
      },
      exclusions: {
        query_rows: 25,
        missing_product_code: 0,
        missing_period: 0,
        missing_currency: 0,
        missing_weight_unit: 0,
        invalid_value: 0,
        invalid_weight: 0,
        insufficient_cohort: 0,
      },
      limitations: [],
      currency_totals: [
        {
          currency: "USD",
          flagged_rows: 1,
          flagged_value: 10,
          estimated_gap: 90,
        },
      ],
    };

    const view = render(
      <RiskResults
        result={result}
        onOpenRecord={vi.fn()}
        onOpenCompany={vi.fn()}
      />,
    );

    expect(view.getByText("2024-Q1 · USD/KG · ACME · CN")).toBeTruthy();
    expect(view.getByText("Low confidence")).toBeTruthy();
    expect(view.getByRole("columnheader", { name: "Robust cutoff" })).toBeTruthy();
    expect(view.getByText("50.00")).toBeTruthy();
    expect(view.getByText(/90.0% below/)).toBeTruthy();
    expect(view.getByText(/only 25 records/)).toBeTruthy();
    expect(view.getByText("USD totals")).toBeTruthy();
  });
});
