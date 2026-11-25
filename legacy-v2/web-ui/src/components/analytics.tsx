// Reusable analytics widgets: headline stat cards, the monthly bar chart, the
// grouped ranking table, and the price-metrics table. All numbers arrive
// pre-computed from the backend.

import type { CSSProperties, ReactNode } from "react";

import type {
  AnalyticsFilterAction,
  AnalyticsGroupRow,
  AnalyticsMeasures,
  AnalyticsMonthRow,
  AnalyticsPriceMetric,
  AnalyticsSection,
  AnalyticsWeightTotal,
} from "../api/types";
import { useI18n } from "../lib/i18n";
import {
  commonCurrency,
  compatibleCurrencyTotal,
  currencyLabel,
  safeNetWeightKg,
  unitLabel,
} from "../lib/analyticsMeasures";
import {
  formatCompact,
  formatInt,
  formatMoney,
  formatMonth,
  formatPercent,
} from "../lib/format";

export function StatCard({
  label,
  value,
  hint,
}: {
  label: string;
  value: ReactNode;
  hint?: string;
}) {
  return (
    <div className="panel stat-card">
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      {hint ? <div className="stat-hint">{hint}</div> : null}
    </div>
  );
}

export function CurrencySummary({
  measures,
  legacyUsd,
}: {
  measures: AnalyticsMeasures;
  legacyUsd?: number;
}) {
  const { t } = useI18n();
  const totals =
    measures.currency_totals.length > 0
      ? measures.currency_totals
      : typeof legacyUsd === "number" && Number.isFinite(legacyUsd)
        ? [{ currency: "USD", known: true, valued_rows: 0, total_value: legacyUsd }]
        : [];
  if (totals.length === 0) return <span className="faint">—</span>;
  return (
    <div className="measure-list">
      {totals.map((total, index) => (
        <span className="measure-item" key={`${total.currency}-${index}`}>
          <strong>{formatCompact(total.total_value)}</strong>
          <span>{currencyLabel(total.currency, t("analytics_unknown_currency"))}</span>
        </span>
      ))}
    </div>
  );
}

export function WeightSummary({ totals }: { totals: AnalyticsWeightTotal[] }) {
  const { t } = useI18n();
  if (totals.length === 0) return <span className="faint">—</span>;
  return (
    <div className="measure-list">
      {totals.map((total, index) => {
        const sourceUnit = unitLabel(total.source_unit, t("analytics_unknown_unit"));
        const normalized =
          total.known && total.normalized_unit === "kg" && total.total_kg !== null;
        const label = normalized
          ? total.source_unit.toLowerCase() === "kg"
            ? `${formatCompact(total.total_kg ?? 0)} kg`
            : `${formatCompact(total.total_source_weight)} ${sourceUnit} → ${formatCompact(total.total_kg ?? 0)} kg`
          : `${formatCompact(total.total_source_weight)} ${sourceUnit}`;
        return (
          <span
            className={`measure-item ${normalized ? "" : "unknown"}`}
            key={`${sourceUnit}-${index}`}
          >
            {label}
          </span>
        );
      })}
    </div>
  );
}

export function ValuePerWeightSummary({
  measures,
  legacyUsdPerKg,
}: {
  measures: AnalyticsMeasures;
  legacyUsdPerKg?: number;
}) {
  const { t } = useI18n();
  const ratios = measures.value_per_net_weight.filter(
    (ratio) => ratio.value_per_weight !== null && Number.isFinite(ratio.value_per_weight),
  );
  if (
    ratios.length === 0 &&
    typeof legacyUsdPerKg === "number" &&
    Number.isFinite(legacyUsdPerKg)
  ) {
    return <span>{formatMoney(legacyUsdPerKg)} USD/kg</span>;
  }
  if (ratios.length === 0) return <span className="faint">—</span>;
  return (
    <div className="measure-list">
      {ratios.map((ratio, index) => (
        <span className="measure-item" key={`${ratio.currency}-${ratio.normalized_weight_unit}-${index}`}>
          <strong>{formatMoney(ratio.value_per_weight ?? 0)}</strong>
          <span>
            {currencyLabel(ratio.currency, t("analytics_unknown_currency"))}/
            {unitLabel(ratio.normalized_weight_unit, t("analytics_unknown_unit"))}
          </span>
        </span>
      ))}
    </div>
  );
}

export function MonthChart({
  months,
  metric = "value",
  onSelect,
  allowLegacyRawKg = false,
}: {
  months: AnalyticsMonthRow[];
  metric?: "value" | "rows" | "net_weight";
  onSelect?: (month: string) => void;
  allowLegacyRawKg?: boolean;
}) {
  const { t } = useI18n();
  if (months.length === 0) {
    return <div className="faint">{t("analytics_no_group_data")}</div>;
  }
  const valueCurrency = commonCurrency(months);
  const values: Array<number | null> = months.map((month) => {
    if (metric === "rows") return month.rows;
    if (metric === "net_weight") return safeNetWeightKg(month, allowLegacyRawKg);
    if (!valueCurrency) return null;
    return compatibleCurrencyTotal(month)?.total_value ?? null;
  });
  const comparable = values.filter((value): value is number => value !== null);
  if (comparable.length === 0) {
    return <div className="faint chart-unavailable">{t("analytics_not_comparable")}</div>;
  }
  const max = Math.max(...comparable, 1);
  const peakValue = Math.max(...comparable);
  const peakIdx = values.indexOf(peakValue);
  const fmt = metric === "rows" ? formatInt : formatCompact;
  // Thin out x labels so they never collide on long ranges.
  const step = months.length <= 14 ? 1 : Math.ceil(months.length / 12);
  return (
    <div className="month-chart">
      <div className="chart-axis">
        <span>{fmt(max)}</span>
        <span>{fmt(max / 2)}</span>
        <span>0</span>
      </div>
      <div className="chart-plot">
        <div className="chart-bars">
          {months.map((m, i) => {
            const value = values[i];
            const height = value === null ? 0 : Math.max(2, (value / max) * 100);
            return (
              <div
                key={m.month}
                className="chart-col"
                style={
                  { cursor: onSelect ? "pointer" : "default", "--col-i": i } as CSSProperties
                }
                title={`${formatMonth(m.month)} · ${value === null ? t("analytics_not_comparable") : fmt(value)}`}
                onClick={() => onSelect?.(m.month)}
              >
                <div
                  className={`chart-bar ${i === peakIdx ? "peak" : ""} ${value === null ? "unavailable" : ""}`}
                  style={{ height: `${height}%` }}
                />
              </div>
            );
          })}
        </div>
        <div className="chart-xrow">
          {months.map((m, i) => (
            <div key={m.month} className="chart-x">
              {i % step === 0 ? formatMonth(m.month).split(" ")[0] : ""}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

export function GroupTable({
  section,
  onFilter,
  totalRowCount,
}: {
  section: AnalyticsSection;
  onFilter?: (action: AnalyticsFilterAction) => void;
  /** True cohort size for the share column; visible rows are only a top-N slice. */
  totalRowCount?: number;
}) {
  const { t } = useI18n();
  if (section.rows.length === 0) {
    return <div className="faint">{t("analytics_no_group_data")}</div>;
  }
  const visibleRows = section.rows.reduce((sum, row) => sum + row.rows, 0);
  const totalRows =
    typeof totalRowCount === "number" && totalRowCount >= visibleRows
      ? totalRowCount
      : visibleRows;
  const shares = section.rows.map((row) =>
    totalRows > 0 ? (row.rows / totalRows) * 100 : 0,
  );
  const maxShare = Math.max(...shares, 1);
  return (
    <div className="table-wrap" style={{ maxHeight: "none" }}>
      <table className="grid" style={{ width: "100%" }}>
        <thead>
          <tr>
            <th style={{ minWidth: 220 }}>{t("col_name")}</th>
            <th style={{ width: 160 }}>{t("analytics_rows_share")}</th>
            <th>{t("common_rows")}</th>
            <th>{t("analytics_value_by_currency")}</th>
            <th>{t("analytics_net_weight")}</th>
            <th>{t("analytics_value_per_weight")}</th>
          </tr>
        </thead>
        <tbody>
          {section.rows.map((row: AnalyticsGroupRow, i) => (
            <tr
              key={`${row.label}-${i}`}
              onClick={() => row.filter_action && onFilter?.(row.filter_action)}
              style={{ cursor: row.filter_action ? "pointer" : "default" }}
            >
              <td title={row.label} style={{ maxWidth: 320 }}>
                {row.label || "—"}
              </td>
              <td>
                <div className="row" style={{ gap: 8 }}>
                  <div className="bar-track" style={{ width: 90 }}>
                    <div
                      className="bar-fill"
                      style={{ width: `${(shares[i] / maxShare) * 100}%` }}
                    />
                  </div>
                  <span className="faint">{formatPercent(shares[i])}</span>
                </div>
              </td>
              <td>{formatInt(row.rows)}</td>
              <td>
                <CurrencySummary measures={row.measures} legacyUsd={row.total_value_usd} />
              </td>
              <td>
                <WeightSummary totals={row.measures.net_weight_totals} />
              </td>
              <td>
                <ValuePerWeightSummary measures={row.measures} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const PRICE_LABELS: Record<string, string> = {
  value_per_net_kg: "Value / net kg",
  rfv_usd_kg: "RFV $/kg",
  rmv_net_usd_kg: "RMV net $/kg",
  rmv_usd_extra_unit: "RMV extra unit",
  rmv_gross_usd_kg: "RMV gross $/kg",
  min_base_usd_kg: "Min base $/kg",
};

// A compact box-plot with robust (Tukey) scaling: the whisker spans the
// interquartile fence so the P25–P75 box and the median line stay readable even
// when a few extreme outliers stretch the true min–max by orders of magnitude.
// A dot on the right edge flags that outliers extend beyond the fence.
function PriceRangeBar({ metric }: { metric: AnalyticsPriceMetric }) {
  const { minimum, p25, median, p75, maximum } = metric;
  if (!(maximum > minimum)) return <span className="faint">—</span>;
  const iqr = Math.max(0, p75 - p25);
  const lo = iqr > 0 ? Math.max(minimum, p25 - 1.5 * iqr) : minimum;
  const hi = iqr > 0 ? Math.min(maximum, p75 + 1.5 * iqr) : maximum;
  const span = hi - lo || 1;
  const pct = (value: number) => Math.min(100, Math.max(0, ((value - lo) / span) * 100));
  const boxLeft = pct(p25);
  const boxWidth = Math.max(2, pct(p75) - boxLeft);
  const hasHighOutliers = maximum > hi;
  const hasLowOutliers = minimum < lo;
  return (
    <div
      className="price-range"
      title={`min ${formatMoney(minimum)} · P25 ${formatMoney(p25)} · median ${formatMoney(median)} · P75 ${formatMoney(p75)} · max ${formatMoney(maximum)}`}
    >
      <div className="price-range-whisker" />
      <div className="price-range-box" style={{ left: `${boxLeft}%`, width: `${boxWidth}%` }} />
      <div className="price-range-median" style={{ left: `${pct(median)}%` }} />
      {hasLowOutliers ? <div className="price-range-outlier" style={{ left: 0 }} /> : null}
      {hasHighOutliers ? <div className="price-range-outlier" style={{ left: "100%" }} /> : null}
    </div>
  );
}

export function PriceTable({ metrics }: { metrics: AnalyticsPriceMetric[] }) {
  const { t } = useI18n();
  const withData = metrics.filter((m) => m.count > 0);
  if (withData.length === 0) {
    return <div className="faint">{t("price_no_rows")}</div>;
  }
  return (
    <div className="table-wrap" style={{ maxHeight: "none" }}>
      <table className="grid" style={{ width: "100%" }}>
        <thead>
          <tr>
            <th>{t("price_col_metric")}</th>
            <th>{t("price_col_samples")}</th>
            <th>{t("price_col_median")}</th>
            <th>{t("price_col_average")}</th>
            <th>{t("price_col_weighted")}</th>
            <th>P25</th>
            <th>P75</th>
            <th>{t("price_col_iqr")}</th>
            <th>{t("price_col_min")}</th>
            <th>{t("price_col_max")}</th>
            <th style={{ minWidth: 130 }}>{t("price_col_distribution")}</th>
          </tr>
        </thead>
        <tbody>
          {withData.map((m) => (
            <tr key={m.kind} style={{ cursor: "default" }}>
              <td>{PRICE_LABELS[m.kind] ?? m.kind}</td>
              <td>{formatInt(m.count)}</td>
              <td>{formatMoney(m.median)}</td>
              <td>{formatMoney(m.average)}</td>
              <td>{formatMoney(m.weighted_average)}</td>
              <td>{formatMoney(m.p25)}</td>
              <td>{formatMoney(m.p75)}</td>
              <td className="faint">{formatMoney(Math.max(0, m.p75 - m.p25))}</td>
              <td>{formatMoney(m.minimum)}</td>
              <td>{formatMoney(m.maximum)}</td>
              <td>
                <PriceRangeBar metric={m} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
