import type { FieldDto, Query, QueryExpr, ResultSort } from "../api/types";
import type { MessageKey, Translate } from "./i18n";

export const RESULT_SORT_STORAGE_KEY = "base-search.result-sort.v1";

const FILTER_KEYS: Record<keyof Query["filters"], MessageKey> = {
  year: "filter_year",
  product_code: "filter_product_code",
  trademark: "filter_trademark",
  description: "filter_description",
  sender: "filter_sender",
  recipient: "filter_recipient",
  edrpou: "filter_company_code",
  trade_country: "filter_trade_country",
  dispatch_country: "filter_dispatch_country",
  origin_country: "filter_origin_country",
};

export function selectAllFieldIds(fields: FieldDto[], current: string[]): string[] {
  const available = new Set(fields.map((field) => field.id));
  const ordered = current.filter((id) => available.has(id));
  const selected = new Set(ordered);
  for (const field of fields) {
    if (!selected.has(field.id)) ordered.push(field.id);
  }
  return ordered;
}

export function moveFieldId(ids: string[], id: string, offset: -1 | 1): string[] {
  const index = ids.indexOf(id);
  const destination = index + offset;
  if (index < 0 || destination < 0 || destination >= ids.length) return ids;
  const next = [...ids];
  [next[index], next[destination]] = [next[destination], next[index]];
  return next;
}

export function describeExportQuery(
  query: Query,
  t: Translate,
): { summary: string; scope: string } {
  const parts: string[] = [];
  const text = query.text.trim();
  if (text) parts.push(t("exports_query_text", { value: text }));
  for (const [key, value] of Object.entries(query.filters) as [
    keyof Query["filters"],
    string,
  ][]) {
    const trimmed = value.trim();
    if (trimmed) parts.push(`${t(FILTER_KEYS[key])}: ${trimmed}`);
  }
  const advanced = countAdvancedRules(query.advanced ?? null);
  if (advanced === 1) parts.push(t("exports_query_advanced_one"));
  if (advanced > 1) parts.push(t("exports_query_advanced_many", { count: advanced }));

  const visible = parts.slice(0, 3);
  if (parts.length > visible.length) {
    visible.push(t("exports_query_more", { count: parts.length - visible.length }));
  }
  return {
    summary: visible.length > 0 ? visible.join(" · ") : t("exports_query_all"),
    scope:
      query.record_scope === "occurrences"
        ? t("exports_scope_occurrences")
        : t("exports_scope_canonical"),
  };
}

export function readActiveResultSort(): ResultSort | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(RESULT_SORT_STORAGE_KEY);
    if (!raw) return null;
    const value = JSON.parse(raw) as Partial<ResultSort>;
    return typeof value.field === "string" && typeof value.descending === "boolean"
      ? { field: value.field, descending: value.descending }
      : null;
  } catch {
    return null;
  }
}

export function writeActiveResultSort(sort: ResultSort | null): void {
  if (typeof window === "undefined") return;
  try {
    if (sort) window.localStorage.setItem(RESULT_SORT_STORAGE_KEY, JSON.stringify(sort));
    else window.localStorage.removeItem(RESULT_SORT_STORAGE_KEY);
  } catch {
    // A private or hardened browser may disable storage. Search still works for
    // the current page; export simply falls back to the default result order.
  }
}

function countAdvancedRules(expr: QueryExpr | null): number {
  if (!expr) return 0;
  if ("Condition" in expr) return 1;
  return expr.Group.children.reduce((total, child) => total + countAdvancedRules(child), 0);
}
