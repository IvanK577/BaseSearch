import type {
  AnalyticsCurrencyTotal,
  AnalyticsGroupRow,
  AnalyticsMeasures,
  AnalyticsMonthRow,
  AnalyticsOverview,
  AnalyticsValuePerWeight,
  AnalyticsWeightTotal,
} from "../api/types";

type ValueCarrier =
  | AnalyticsOverview
  | AnalyticsGroupRow
  | AnalyticsMonthRow;

// The core buckets an unrecognized currency or unit under a sentinel key so it
// is never confused with a real code. Plain "__unknown__" means the source
// cell was empty; "__unknown__:XYZ" keeps the original, non-standard code.
// Neither must ever reach the screen verbatim.
const UNKNOWN_KEY = "__unknown__";

export function isUnknownMeasureKey(raw: string): boolean {
  return raw === UNKNOWN_KEY || raw.startsWith(`${UNKNOWN_KEY}:`);
}

/** Human label for a currency bucket key, given the localized "unknown" word. */
export function currencyLabel(raw: string, unknownLabel: string): string {
  if (raw === UNKNOWN_KEY) return unknownLabel;
  if (raw.startsWith(`${UNKNOWN_KEY}:`)) return raw.slice(UNKNOWN_KEY.length + 1);
  return raw;
}

/** Human label for a weight-unit bucket key, given the localized "unknown" word. */
export function unitLabel(raw: string, unknownLabel: string): string {
  if (!raw || raw === UNKNOWN_KEY) return unknownLabel;
  if (raw.startsWith(`${UNKNOWN_KEY}:`)) return raw.slice(UNKNOWN_KEY.length + 1);
  return raw;
}

export function compatibleCurrencyTotal(
  value: ValueCarrier,
): AnalyticsCurrencyTotal | null {
  const total = value.measures.compatible_value_total;
  if (total?.known && Number.isFinite(total.total_value)) return total;
  if (typeof value.total_value_usd === "number" && Number.isFinite(value.total_value_usd)) {
    return {
      currency: "USD",
      known: true,
      valued_rows: 0,
      total_value: value.total_value_usd,
    };
  }
  return null;
}

export function compatibleValuePerWeight(
  measures: AnalyticsMeasures,
): AnalyticsValuePerWeight | null {
  const ratio = measures.compatible_value_per_net_weight;
  return ratio && ratio.value_per_weight !== null && Number.isFinite(ratio.value_per_weight)
    ? ratio
    : null;
}

export function normalizedWeightTotal(
  totals: AnalyticsWeightTotal[],
): number | null {
  if (
    totals.length === 0 ||
    totals.some(
      (total) =>
        !total.known ||
        total.normalized_unit !== "kg" ||
        total.total_kg === null ||
        !Number.isFinite(total.total_kg),
    )
  ) {
    return null;
  }
  return totals.reduce((sum, total) => sum + (total.total_kg ?? 0), 0);
}

export function safeNetWeightKg(
  value: ValueCarrier,
  allowLegacyRawKg = false,
): number | null {
  const normalized = normalizedWeightTotal(value.measures.net_weight_totals);
  if (normalized !== null) return normalized;
  return allowLegacyRawKg && Number.isFinite(value.total_net_kg)
    ? value.total_net_kg
    : null;
}

export function safeValuePerNetWeight(
  value: AnalyticsOverview | AnalyticsGroupRow,
  allowLegacyRawKg = false,
): AnalyticsValuePerWeight | null {
  const measured = compatibleValuePerWeight(value.measures);
  if (measured) return measured;
  const currency = compatibleCurrencyTotal(value)?.currency;
  if (
    allowLegacyRawKg &&
    currency === "USD" &&
    typeof value.avg_value_per_net_kg === "number" &&
    Number.isFinite(value.avg_value_per_net_kg)
  ) {
    return {
      currency: "USD",
      normalized_weight_unit: "kg",
      source_weight_units: ["kg"],
      paired_rows: 0,
      total_value: 0,
      total_weight: 0,
      value_per_weight: value.avg_value_per_net_kg,
    };
  }
  return null;
}

export function rawNetWeightIsKg(measures: AnalyticsMeasures): boolean {
  return (
    measures.net_weight_totals.length > 0 &&
    measures.net_weight_totals.every(
      (total) =>
        total.known &&
        total.source_unit.toLowerCase() === "kg" &&
        total.normalized_unit === "kg" &&
        total.total_kg !== null,
    )
  );
}

export function commonCurrency(values: ValueCarrier[]): string | null {
  if (values.length === 0) return null;
  const totals = values.map(compatibleCurrencyTotal);
  const currency = totals[0]?.currency ?? null;
  return currency && totals.every((total) => total?.currency === currency)
    ? currency
    : null;
}

export function safeRowShare(
  row: AnalyticsGroupRow,
  totalRows: number,
  valueShareIsCompatible: boolean,
): number {
  if (valueShareIsCompatible && Number.isFinite(row.share_percent)) {
    return row.share_percent;
  }
  return totalRows > 0 ? (row.rows / totalRows) * 100 : 0;
}
