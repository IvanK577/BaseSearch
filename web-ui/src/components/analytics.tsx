// Reusable analytics widgets: headline stat cards, the monthly bar chart, the
// grouped ranking table, and the price-metrics table. All numbers arrive
// pre-computed from the backend.

import type {
  AnalyticsFilterAction,
  AnalyticsGroupRow,
  AnalyticsMonthRow,
  AnalyticsPriceMetric,
  AnalyticsSection,
} from "../api/types";
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
  value: string;
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

export function MonthChart({
  months,
  metric = "total_value_usd",
  onSelect,
}: {
  months: AnalyticsMonthRow[];
  metric?: "total_value_usd" | "rows" | "total_net_kg";
  onSelect?: (month: string) => void;
}) {
  if (months.length === 0) {
    return <div className="faint">No monthly data.</div>;
  }
  const values = months.map((m) => m[metric] as number);
  const max = Math.max(...values, 1);
  const peakIdx = values.indexOf(Math.max(...values));
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
            const height = Math.max(2, (values[i] / max) * 100);
            return (
              <div
                key={m.month}
                className="chart-col"
                style={{ cursor: onSelect ? "pointer" : "default" }}
                title={`${formatMonth(m.month)} · ${fmt(values[i])}${onSelect ? " · click to filter" : ""}`}
                onClick={() => onSelect?.(m.month)}
              >
                <div
                  className={`chart-bar ${i === peakIdx ? "peak" : ""}`}
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
}: {
  section: AnalyticsSection;
  onFilter?: (action: AnalyticsFilterAction) => void;
}) {
  if (section.rows.length === 0) {
    return <div className="faint">No data for this group.</div>;
  }
  const maxShare = Math.max(...section.rows.map((r) => r.share_percent), 1);
  return (
    <div className="table-wrap" style={{ maxHeight: "none" }}>
      <table className="grid" style={{ width: "100%" }}>
        <thead>
          <tr>
            <th style={{ minWidth: 220 }}>Name</th>
            <th style={{ width: 160 }}>Share</th>
            <th>Rows</th>
            <th>Value USD</th>
            <th>Net kg</th>
            <th>Value/kg</th>
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
                      style={{ width: `${(row.share_percent / maxShare) * 100}%` }}
                    />
                  </div>
                  <span className="faint">{formatPercent(row.share_percent)}</span>
                </div>
              </td>
              <td>{formatInt(row.rows)}</td>
              <td>{formatMoney(row.total_value_usd)}</td>
              <td>{formatInt(row.total_net_kg)}</td>
              <td>{formatMoney(row.avg_value_per_net_kg)}</td>
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

export function PriceTable({ metrics }: { metrics: AnalyticsPriceMetric[] }) {
  const withData = metrics.filter((m) => m.count > 0);
  if (withData.length === 0) {
    return <div className="faint">No priced rows in this query.</div>;
  }
  return (
    <div className="table-wrap" style={{ maxHeight: "none" }}>
      <table className="grid" style={{ width: "100%" }}>
        <thead>
          <tr>
            <th>Metric</th>
            <th>Samples</th>
            <th>Median</th>
            <th>Average</th>
            <th>Weighted</th>
            <th>P25</th>
            <th>P75</th>
            <th>Min</th>
            <th>Max</th>
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
              <td>{formatMoney(m.minimum)}</td>
              <td>{formatMoney(m.maximum)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
