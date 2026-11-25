import type {
  Analytics,
  AnalyticsGroupRow,
  AnalyticsOverview,
  AnalyticsSection,
  Query,
} from "../api/types";
import {
  compatibleCurrencyTotal,
  currencyLabel,
  safeRowShare,
  unitLabel,
} from "./analyticsMeasures";
import { formatInt, formatMoney, formatPercent } from "./format";
import type { Translate } from "./i18n";

type MeasureCarrier = AnalyticsOverview | AnalyticsGroupRow;

export function queryLabel(query: Query, t?: Translate): string {
  const parts: string[] = [];
  if (query.text.trim()) parts.push(`"${query.text.trim()}"`);
  for (const [name, value] of Object.entries(query.filters)) {
    if (value?.trim()) parts.push(`${name}: ${value.trim()}`);
  }
  if (query.advanced) parts.push(t ? t("search_advanced") : "Advanced filter");
  return parts.length ? parts.join(" / ") : t ? t("analytics_whole_db") : "Whole database";
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function currencyText(value: MeasureCarrier, t: Translate): string {
  const totals =
    value.measures.currency_totals.length > 0
      ? value.measures.currency_totals
      : compatibleCurrencyTotal(value)
        ? [compatibleCurrencyTotal(value)!]
        : [];
  return (
    totals
      .map(
        (total) =>
          `${formatMoney(total.total_value)} ${currencyLabel(
            total.currency,
            t("analytics_unknown_currency"),
          )}`,
      )
      .join(" / ") || "-"
  );
}

function weightText(value: MeasureCarrier, t: Translate): string {
  return (
    value.measures.net_weight_totals
      .map((total) => {
        const sourceUnit = unitLabel(total.source_unit, t("analytics_unknown_unit"));
        if (total.known && total.normalized_unit === "kg" && total.total_kg !== null) {
          return total.source_unit.toLowerCase() === "kg"
            ? `${formatInt(total.total_kg)} kg`
            : `${formatInt(total.total_source_weight)} ${sourceUnit} -> ${formatInt(
                total.total_kg,
              )} kg`;
        }
        return `${formatInt(total.total_source_weight)} ${sourceUnit}`;
      })
      .join(" / ") || "-"
  );
}

function ratioText(value: MeasureCarrier, t: Translate): string {
  return (
    value.measures.value_per_net_weight
      .filter((ratio) => ratio.value_per_weight !== null)
      .map(
        (ratio) =>
          `${formatMoney(ratio.value_per_weight ?? 0)} ${currencyLabel(
            ratio.currency,
            t("analytics_unknown_currency"),
          )}/${unitLabel(ratio.normalized_weight_unit, t("analytics_unknown_unit"))}`,
      )
      .join(" / ") || "-"
  );
}

function sectionRowsHtml(
  sections: AnalyticsSection[],
  overview: AnalyticsOverview,
  titleOf: (kind: string) => string,
  t: Translate,
): string {
  let out = "";
  const valueShareIsCompatible = compatibleCurrencyTotal(overview) !== null;
  for (const section of sections.filter((item) => item.rows.length > 0).slice(0, 3)) {
    out += `<h3>${escapeHtml(titleOf(section.kind))}</h3>`;
    out += `<table><thead><tr><th>${escapeHtml(t("col_name"))}</th><th>${escapeHtml(
      t("analytics_value_by_currency"),
    )}</th><th>${escapeHtml(t("analytics_net_weight"))}</th><th>${escapeHtml(
      t("common_rows"),
    )}</th><th>${escapeHtml(t("analytics_rows_share"))}</th></tr></thead><tbody>`;
    for (const row of section.rows.slice(0, 10)) {
      out += `<tr><td>${escapeHtml(row.label || "-")}</td><td>${escapeHtml(
        currencyText(row, t),
      )}</td><td>${escapeHtml(weightText(row, t))}</td><td>${formatInt(
        row.rows,
      )}</td><td>${formatPercent(
        safeRowShare(row, overview.row_count, valueShareIsCompatible),
      )}</td></tr>`;
    }
    out += "</tbody></table>";
  }
  return out;
}

const REPORT_CSS = `
  :root { color-scheme: light; font-family: "Segoe UI", Arial, sans-serif; color: #1b2430; }
  body { margin: 36px; background: #fff; font-size: 13px; line-height: 1.45; }
  h1 { margin: 0 0 4px; font-size: 26px; }
  h2 { margin: 26px 0 8px; font-size: 18px; border-bottom: 1px solid #d7dde5; padding-bottom: 4px; }
  h3 { margin: 16px 0 6px; font-size: 14px; color: #34404e; }
  .query { margin: 0 0 18px; color: #6a7682; }
  .kpis { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; margin: 18px 0 20px; }
  .kpis article { border: 1px solid #d7dde5; border-radius: 6px; padding: 10px 12px; }
  .kpis span { display: block; color: #6a7682; font-size: 11px; }
  .kpis strong { display: block; margin-top: 4px; font-size: 18px; font-family: Consolas, monospace; }
  table { width: 100%; border-collapse: collapse; margin-bottom: 8px; }
  th, td { border-bottom: 1px solid #e4e8ee; padding: 6px 7px; text-align: left; vertical-align: top; }
  th { background: #f3f6f9; color: #34404e; font-size: 11px; text-transform: uppercase; }
  td:not(:first-child), th:not(:first-child) { text-align: right; font-family: Consolas, monospace; }
  @media print { body { margin: 18mm; } h2 { break-after: avoid; } table { break-inside: avoid; } }
`;

export function buildReportHtml(
  analytics: Analytics,
  query: Query,
  titleOf: (kind: string) => string,
  t: Translate,
): string {
  const overview = analytics.overview;
  const kpis: [string, string][] = [
    [t("common_rows"), formatInt(overview.row_count)],
    [t("common_declarations"), formatInt(overview.declaration_count)],
    [t("analytics_value_by_currency"), currencyText(overview, t)],
    [t("analytics_net_weight"), weightText(overview, t)],
    [t("analytics_value_per_weight"), ratioText(overview, t)],
    [t("analytics_companies"), formatInt(overview.distinct_edrpou)],
  ];
  let body = `<h1>${escapeHtml(t("report_title"))}</h1><p class="query">${escapeHtml(
    queryLabel(query, t),
  )}</p><section class="kpis">`;
  for (const [label, value] of kpis) {
    body += `<article><span>${escapeHtml(label)}</span><strong>${escapeHtml(value)}</strong></article>`;
  }
  body += "</section>";
  body += `<section><h2>${escapeHtml(t("analytics_companies"))}</h2>${sectionRowsHtml(
    analytics.company_sections,
    overview,
    titleOf,
    t,
  )}</section>`;
  body += `<section><h2>${escapeHtml(t("analytics_products"))}</h2>${sectionRowsHtml(
    analytics.product_sections,
    overview,
    titleOf,
    t,
  )}</section>`;
  body += `<section><h2>${escapeHtml(t("analytics_countries"))}</h2>${sectionRowsHtml(
    analytics.country_sections,
    overview,
    titleOf,
    t,
  )}</section>`;
  return `<!doctype html><html><head><meta charset="utf-8"><title>${escapeHtml(
    t("report_title"),
  )}</title><style>${REPORT_CSS}</style></head><body>${body}</body></html>`;
}

export function buildReportText(
  analytics: Analytics,
  query: Query,
  t: Translate,
): string {
  const overview = analytics.overview;
  const lines: string[] = [
    t("report_title"),
    queryLabel(query, t),
    "",
    `${t("common_rows")}: ${formatInt(overview.row_count)}`,
    `${t("common_declarations")}: ${formatInt(overview.declaration_count)}`,
    `${t("analytics_value_by_currency")}: ${currencyText(overview, t)}`,
    `${t("analytics_net_weight")}: ${weightText(overview, t)}`,
    `${t("analytics_value_per_weight")}: ${ratioText(overview, t)}`,
    `${t("analytics_companies")}: ${formatInt(overview.distinct_edrpou)}`,
  ];
  const valueShareIsCompatible = compatibleCurrencyTotal(overview) !== null;
  const appendSections = (title: string, sections: AnalyticsSection[]) => {
    const withRows = sections.filter((section) => section.rows.length > 0).slice(0, 2);
    if (withRows.length === 0) return;
    lines.push("", title);
    for (const section of withRows) {
      for (const row of section.rows.slice(0, 5)) {
        lines.push(
          `  ${row.label || "-"}: ${currencyText(row, t)} (${formatPercent(
            safeRowShare(row, overview.row_count, valueShareIsCompatible),
          )})`,
        );
      }
    }
  };
  appendSections(t("analytics_companies"), analytics.company_sections);
  appendSections(t("analytics_products"), analytics.product_sections);
  appendSections(t("analytics_countries"), analytics.country_sections);
  return lines.join("\n");
}
