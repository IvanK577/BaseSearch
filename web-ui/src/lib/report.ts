// Builds a clean, self-contained working report from the analytics of the
// current query — a printable HTML document (Save as PDF from the browser
// print dialog) and a plain-text version for copying. Mirrors the desktop's
// report so the two interfaces produce the same deliverable.

import type { Analytics, AnalyticsSection, Query } from "../api/types";
import { formatInt, formatMoney, formatPercent } from "./format";

/** A short human label for the query the report was run on. */
export function queryLabel(query: Query): string {
  const parts: string[] = [];
  if (query.text.trim()) parts.push(`"${query.text.trim()}"`);
  const f = query.filters;
  for (const [name, value] of Object.entries(f)) {
    if (value && value.trim()) parts.push(`${name}: ${value.trim()}`);
  }
  if (query.advanced) parts.push("advanced filter");
  return parts.length ? parts.join(" · ") : "Whole database";
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function sectionRowsHtml(sections: AnalyticsSection[], titleOf: (kind: string) => string): string {
  let out = "";
  for (const section of sections.filter((s) => s.rows.length > 0).slice(0, 3)) {
    out += `<h3>${escapeHtml(titleOf(section.kind))}</h3>`;
    out += `<table><thead><tr><th>Name</th><th>Value USD</th><th>Net kg</th><th>Rows</th><th>Share</th></tr></thead><tbody>`;
    for (const row of section.rows.slice(0, 10)) {
      out += `<tr><td>${escapeHtml(row.label || "—")}</td><td>${formatMoney(
        row.total_value_usd,
      )}</td><td>${formatInt(row.total_net_kg)}</td><td>${formatInt(
        row.rows,
      )}</td><td>${formatPercent(row.share_percent)}</td></tr>`;
    }
    out += `</tbody></table>`;
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

/** A full standalone HTML document ready to print or save as PDF. */
export function buildReportHtml(
  analytics: Analytics,
  query: Query,
  titleOf: (kind: string) => string,
): string {
  const o = analytics.overview;
  const kpis: [string, string][] = [
    ["Rows", formatInt(o.row_count)],
    ["Documents", formatInt(o.declaration_count)],
    ["Value USD", formatMoney(o.total_value_usd)],
    ["Net kg", formatInt(o.total_net_kg)],
    ["Value / kg", formatMoney(o.avg_value_per_net_kg)],
    ["Companies", formatInt(o.distinct_edrpou)],
  ];
  let body = `<h1>Base Search Report</h1><p class="query">${escapeHtml(queryLabel(query))}</p>`;
  body += `<section class="kpis">`;
  for (const [label, value] of kpis) {
    body += `<article><span>${escapeHtml(label)}</span><strong>${escapeHtml(value)}</strong></article>`;
  }
  body += `</section>`;
  body += `<section><h2>Companies</h2>${sectionRowsHtml(analytics.company_sections, titleOf)}</section>`;
  body += `<section><h2>Goods</h2>${sectionRowsHtml(analytics.product_sections, titleOf)}</section>`;
  body += `<section><h2>Countries</h2>${sectionRowsHtml(analytics.country_sections, titleOf)}</section>`;
  return `<!doctype html><html><head><meta charset="utf-8"><title>Base Search Report</title><style>${REPORT_CSS}</style></head><body>${body}</body></html>`;
}

/** A plain-text (markdown-ish) version for copying to the clipboard. */
export function buildReportText(analytics: Analytics, query: Query): string {
  const o = analytics.overview;
  const lines: string[] = [
    "Base Search Report",
    `Query: ${queryLabel(query)}`,
    "",
    `Rows: ${formatInt(o.row_count)}`,
    `Documents: ${formatInt(o.declaration_count)}`,
    `Value USD: ${formatMoney(o.total_value_usd)}`,
    `Net kg: ${formatInt(o.total_net_kg)}`,
    `Value / kg: ${formatMoney(o.avg_value_per_net_kg)}`,
    `Companies: ${formatInt(o.distinct_edrpou)}`,
  ];
  const appendSections = (title: string, sections: AnalyticsSection[]) => {
    const withRows = sections.filter((s) => s.rows.length > 0).slice(0, 2);
    if (withRows.length === 0) return;
    lines.push("", title);
    for (const section of withRows) {
      for (const row of section.rows.slice(0, 5)) {
        lines.push(
          `  ${row.label || "—"} — ${formatMoney(row.total_value_usd)} (${formatPercent(row.share_percent)})`,
        );
      }
    }
  };
  appendSections("Companies", analytics.company_sections);
  appendSections("Goods", analytics.product_sections);
  appendSections("Countries", analytics.country_sections);
  return lines.join("\n");
}
