import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { api, ApiError } from "../api/client";
import type {
  Analytics,
  AnalyticsFilterAction,
  AnalyticsGroupRow,
  AnalyticsScope,
  AnalyticsSection,
  Filters,
  PivotDim,
  PivotMetric,
  PivotResult,
  Query,
  SchemaResponse,
} from "../api/types";
import { MonthChart, PriceTable, StatCard } from "../components/analytics";
import { Icon } from "../components/Icon";
import { Banner, EmptyState, Loading } from "../components/ui";
import { useI18n, type MessageKey } from "../lib/i18n";
import { fieldRefOf } from "../lib/advanced";
import { copyText } from "../lib/clipboard";
import { downloadCsv } from "../lib/csv";
import { buildReportHtml, buildReportText, queryLabel } from "../lib/report";
import {
  formatCompact,
  formatInt,
  formatMoney,
  formatMonth,
  formatPercent,
  monthBounds,
} from "../lib/format";
import { navigate } from "../lib/router";
import { useQueryStore } from "../state/query";
import { useStore } from "../state/store";

type Tab =
  | "overview"
  | "months"
  | "companies"
  | "products"
  | "countries"
  | "prices"
  | "pivot"
  | "report"
  | "compare";

const TABS: { id: Tab; key: MessageKey }[] = [
  { id: "overview", key: "analytics_overview" },
  { id: "months", key: "analytics_months" },
  { id: "companies", key: "analytics_companies" },
  { id: "products", key: "analytics_products" },
  { id: "countries", key: "analytics_countries" },
  { id: "prices", key: "analytics_prices" },
  { id: "pivot", key: "analytics_pivot" },
  { id: "report", key: "analytics_report" },
  { id: "compare", key: "analytics_compare" },
];

// Tabs that self-fetch (their own state) rather than going through the shared
// scoped loader.
const SELF_FETCH_TABS: Tab[] = ["pivot", "report", "compare"];

const SCOPE_FOR_TAB: Record<Tab, AnalyticsScope | null> = {
  overview: null,
  months: null,
  companies: "companies",
  products: "products",
  countries: "countries",
  prices: "prices",
  pivot: null,
  report: null,
  compare: null,
};

export function AnalyticsPage() {
  const { t } = useI18n();
  const {
    query,
    isEmpty,
    applyText,
    applyFilter,
    applyAdvanced,
    applyDrilldown,
    undo,
    canUndo,
    reset,
  } = useQueryStore();
  const { openCompany, toast } = useStore();
  const [tab, setTab] = useState<Tab>("overview");
  const [forceAll, setForceAll] = useState(false);
  const [analytics, setAnalytics] = useState<Analytics | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hsLevel, setHsLevel] = useState(10);
  // How many rows each ranked section pulls: 50, 200, or 500 ("see all").
  const [sectionLimit, setSectionLimit] = useState(200);
  const [schema, setSchema] = useState<SchemaResponse | null>(null);

  useEffect(() => {
    api.schema().then(setSchema).catch(() => {});
  }, []);

  const shouldRun = !isEmpty || forceAll;

  // Per-query cache so switching tabs is instant and never re-runs an identical
  // aggregation. Cleared when the query changes or the database grows (an
  // import finished), so analytics never serve stale totals.
  const { status } = useStore();
  const queryKey = useMemo(
    () => `${JSON.stringify(query)}|rows:${status?.total_rows ?? 0}`,
    [query, status?.total_rows],
  );
  const cacheRef = useRef<{ key: string; map: Map<string, Analytics> }>({
    key: "",
    map: new Map(),
  });
  // Monotonic request id: a slower earlier response can never overwrite a
  // newer one when the user switches tabs quickly.
  const reqRef = useRef(0);

  const load = useCallback(
    async (scope: AnalyticsScope | null) => {
      const cacheKey = `${scope ?? "_overview"}:${hsLevel}:${sectionLimit}`;
      const store = cacheRef.current;
      if (store.key !== queryKey) {
        store.key = queryKey;
        store.map.clear();
      }
      const cached = store.map.get(cacheKey);
      if (cached) {
        // Invalidate any slower in-flight request so it cannot overwrite the
        // cached data we are about to show, and drop a stuck spinner.
        reqRef.current += 1;
        setAnalytics(cached);
        setError(null);
        setLoading(false);
        return;
      }
      const id = ++reqRef.current;
      setLoading(true);
      setError(null);
      try {
        const res = await api.analytics(query, scope, hsLevel, sectionLimit);
        if (id !== reqRef.current) return;
        store.map.set(cacheKey, res.data);
        setAnalytics(res.data);
      } catch (err) {
        if (id !== reqRef.current) return;
        setError((err as ApiError)?.message ?? "Analytics failed");
      } finally {
        if (id === reqRef.current) setLoading(false);
      }
    },
    [query, queryKey, hsLevel, sectionLimit],
  );

  useEffect(() => {
    if (SELF_FETCH_TABS.includes(tab) || !shouldRun) return;
    load(SCOPE_FOR_TAB[tab]);
  }, [tab, load, shouldRun]);

  // Clicking a month filters the query to that month via a date range. Date
  // fields only accept Equals/Range, so a range over the whole month is the
  // correct (and valid) filter — `StartsWith` on a date is rejected by the core.
  const drillMonth = (month: string) => {
    const dateField = schema?.search_fields.find((f) => f.kind === "date");
    const bounds = monthBounds(month);
    if (!dateField || !bounds) {
      toast(t("analytics_no_date_column"), "error");
      return;
    }
    applyDrilldown({
      Group: {
        op: "And",
        negated: false,
        children: [
          {
            Condition: {
              field: fieldRefOf(dateField),
              op: "Range",
              value: { Range: { from: bounds.from, to: bounds.to } },
              negated: false,
            },
          },
        ],
      },
    });
    toast(`${t("analytics_filtered_to")} ${formatMonth(month)}`, "success");
  };

  const onAction = (action: AnalyticsFilterAction) => {
    applyFilter(action.field as keyof Filters, action.value);
  };

  if (!shouldRun) {
    return (
      <EmptyState
        icon="analytics"
        title={t("analytics_need_query")}
        action={
          <div className="row" style={{ gap: 10 }}>
            <button className="btn" onClick={() => navigate("search")}>
              {t("nav_search")}
            </button>
            <button className="btn btn-primary" onClick={() => setForceAll(true)}>
              {t("analytics_whole_db")}
            </button>
          </div>
        }
      />
    );
  }

  return (
    <div className="stack">
      <QueryChips
        query={query}
        onClearText={() => applyText("")}
        onClearFilter={(key) => applyFilter(key, "")}
        onClearAdvanced={() => applyAdvanced(null)}
        onClearAll={() => {
          // Back to the consent screen: an unfiltered aggregation over the
          // whole database must always be an explicit choice, never a side
          // effect of clearing chips.
          reset();
          setForceAll(false);
        }}
        onUndo={undo}
        canUndo={canUndo}
      />
      <div className="tabs">
        {TABS.map((tabDef) => (
          <button
            key={tabDef.id}
            className={`tab ${tab === tabDef.id ? "active" : ""}`}
            onClick={() => setTab(tabDef.id)}
          >
            {t(tabDef.key)}
          </button>
        ))}
      </div>

      {error ? <Banner>{error}</Banner> : null}

      {tab === "pivot" ? (
        <PivotPanel />
      ) : tab === "report" ? (
        <ReportPanel />
      ) : tab === "compare" ? (
        <ComparePanel />
      ) : loading ? (
        <Loading label={t("common_loading")} />
      ) : analytics ? (
        <>
          {tab === "overview" ? (
            <OverviewPanel analytics={analytics} onMonth={drillMonth} />
          ) : null}
          {tab === "months" ? <MonthsPanel analytics={analytics} onMonth={drillMonth} /> : null}

          {tab === "companies" ? (
            <SectionGroupPanel
              sections={analytics.company_sections}
              limit={sectionLimit}
              onLimit={setSectionLimit}
              onRow={(section, action) =>
                section.kind === "edrpou" ? openCompany(action.value) : onAction(action)
              }
              hintFor={(section) =>
                section.kind === "edrpou" ? t("analytics_open_company_hint") : undefined
              }
            />
          ) : null}

          {tab === "products" ? (
            <SectionGroupPanel
              sections={analytics.product_sections}
              limit={sectionLimit}
              onLimit={setSectionLimit}
              onRow={(_, action) => onAction(action)}
              extraControls={
                <div className="row" style={{ gap: 6 }}>
                  <span className="field-label" style={{ margin: 0 }}>
                    {t("analytics_hs_grouping")}:
                  </span>
                  {[2, 4, 6, 10].map((lvl) => (
                    <button
                      key={lvl}
                      className={`btn btn-sm ${hsLevel === lvl ? "" : "btn-ghost"}`}
                      onClick={() => setHsLevel(lvl)}
                    >
                      {lvl === 10 ? t("analytics_hs_full") : lvl}
                    </button>
                  ))}
                </div>
              }
            />
          ) : null}

          {tab === "countries" ? (
            <SectionGroupPanel
              sections={analytics.country_sections}
              limit={sectionLimit}
              onLimit={setSectionLimit}
              onRow={(_, action) => onAction(action)}
            />
          ) : null}

          {tab === "prices" ? (
            <div className="panel panel-pad">
              <div className="section-title">{t("analytics_prices")}</div>
              <PriceTable metrics={analytics.price_sections} />
            </div>
          ) : null}
        </>
      ) : null}
    </div>
  );
}

function OverviewPanel({
  analytics,
  onMonth,
}: {
  analytics: Analytics;
  onMonth: (month: string) => void;
}) {
  const { t } = useI18n();
  const o = analytics.overview;
  return (
    <div className="stack">
      <div className="stat-grid">
        <StatCard label={t("common_rows")} value={formatInt(o.row_count)} />
        {/* Datasets without document numbers get no permanent "0" card. */}
        {o.declaration_count > 0 ? (
          <StatCard
            label={t("common_declarations")}
            value={formatInt(o.declaration_count)}
          />
        ) : null}
        <StatCard
          label={`${t("common_value")} USD`}
          value={formatCompact(o.total_value_usd)}
          hint={formatMoney(o.total_value_usd)}
        />
        <StatCard label={t("common_net_kg")} value={formatCompact(o.total_net_kg)} />
        <StatCard label={t("analytics_value_per_kg")} value={formatMoney(o.avg_value_per_net_kg)} />
        <StatCard label={t("analytics_companies")} value={formatInt(o.distinct_edrpou)} />
        <StatCard label={t("analytics_product_codes")} value={formatInt(o.distinct_product_codes)} />
        <StatCard
          label={t("analytics_origin_countries")}
          value={formatInt(o.distinct_origin_countries)}
        />
      </div>

      {analytics.months.length > 0 ? (
        <div className="panel panel-pad">
          <div className="row" style={{ justifyContent: "space-between" }}>
            <div className="section-title" style={{ margin: 0 }}>
              {t("analytics_months")}
            </div>
            <div className="faint">
              {formatMonth(analytics.months[0].month)} –{" "}
              {formatMonth(analytics.months[analytics.months.length - 1].month)}
            </div>
          </div>
          <MonthChart months={analytics.months} metric="total_value_usd" onSelect={onMonth} />
        </div>
      ) : null}
    </div>
  );
}

type MonthMetric = "total_value_usd" | "total_net_kg" | "rows";

type MonthSortField = "month" | "value" | "net_kg" | "rows" | "docs" | "mom";

function MonthsPanel({
  analytics,
  onMonth,
}: {
  analytics: Analytics;
  onMonth: (month: string) => void;
}) {
  const { t } = useI18n();
  const [metric, setMetric] = useState<MonthMetric>("total_value_usd");
  const [sortField, setSortField] = useState<MonthSortField>("month");
  const [dir, setDir] = useState<"asc" | "desc">("asc");
  const months = analytics.months;

  // Month-over-month is always computed against the chronological previous
  // month, so it stays correct no matter how the table is sorted.
  const enriched = useMemo(
    () =>
      months.map((m, i) => {
        const prev = i > 0 ? months[i - 1].total_value_usd : null;
        const mom = prev && prev > 0 ? ((m.total_value_usd - prev) / prev) * 100 : null;
        return { ...m, mom };
      }),
    [months],
  );

  const sorted = useMemo(() => {
    const key = (r: (typeof enriched)[number]): number | string => {
      switch (sortField) {
        case "month":
          return r.month;
        case "value":
          return r.total_value_usd;
        case "net_kg":
          return r.total_net_kg;
        case "rows":
          return r.rows;
        case "docs":
          return r.declarations;
        case "mom":
          return r.mom ?? -Infinity;
      }
    };
    return [...enriched].sort((a, b) => {
      const ka = key(a);
      const kb = key(b);
      const cmp =
        typeof ka === "string" ? ka.localeCompare(kb as string) : (ka as number) - (kb as number);
      return dir === "asc" ? cmp : -cmp;
    });
  }, [enriched, sortField, dir]);

  if (months.length === 0) {
    return <EmptyState icon="analytics" title={t("analytics_no_group_data")} />;
  }

  const totalValue = months.reduce((s, m) => s + m.total_value_usd, 0);
  const totalNet = months.reduce((s, m) => s + m.total_net_kg, 0);
  const peak = months.reduce((a, b) => (b.total_value_usd > a.total_value_usd ? b : a));
  const first = months[0].total_value_usd;
  const last = months[months.length - 1].total_value_usd;
  const overall = first > 0 ? ((last - first) / first) * 100 : 0;

  const metricOptions: { id: MonthMetric; label: string }[] = [
    { id: "total_value_usd", label: `${t("common_value")} USD` },
    { id: "total_net_kg", label: t("common_net_kg") },
    { id: "rows", label: t("common_rows") },
  ];

  const setSort = (field: MonthSortField) => {
    if (field === sortField) {
      setDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortField(field);
      setDir(field === "month" ? "asc" : "desc");
    }
  };
  const arrow = (field: MonthSortField) =>
    sortField === field ? (dir === "asc" ? " ▲" : " ▼") : "";

  return (
    <div className="stack">
      {/* One compact context line instead of a second wall of stat cards:
          the chart and the table below already carry the same numbers. */}
      <div className="panel panel-pad row wrap faint" style={{ gap: 14, fontSize: 12 }}>
        <span>
          {formatInt(months.length)} {t("common_months").toLowerCase()} ·{" "}
          {formatMonth(months[0].month)} → {formatMonth(months[months.length - 1].month)}
        </span>
        <span>
          {t("analytics_peak_month")}: <strong>{formatMonth(peak.month)}</strong> (
          {formatCompact(peak.total_value_usd)})
        </span>
        <span>
          {t("analytics_first_last")}:{" "}
          <strong>{`${overall >= 0 ? "+" : ""}${formatPercent(overall, 0)}`}</strong>
        </span>
        <span>
          {t("common_value")}: <strong>{formatCompact(totalValue)}</strong> ·{" "}
          {t("common_net_kg")}: <strong>{formatCompact(totalNet)}</strong>
        </span>
      </div>

      <div className="panel panel-pad">
        <div className="row wrap" style={{ justifyContent: "space-between", gap: 8 }}>
          <div className="section-title" style={{ margin: 0 }}>
            {t("analytics_months")}
          </div>
          <div className="row" style={{ gap: 6 }}>
            <span className="field-label" style={{ margin: 0 }}>{t("chart_metric")}:</span>
            {metricOptions.map((m) => (
              <button
                key={m.id}
                className={`btn btn-sm ${metric === m.id ? "" : "btn-ghost"}`}
                onClick={() => setMetric(m.id)}
              >
                {m.label}
              </button>
            ))}
          </div>
        </div>
        <MonthChart months={months} metric={metric} onSelect={onMonth} />
      </div>

      <div className="panel panel-pad">
        <div className="section-title">{t("analytics_month_breakdown")}</div>
        <div className="table-wrap" style={{ maxHeight: "none" }}>
          <table className="grid" style={{ width: "100%" }}>
            <thead>
              <tr>
                <SortTh label={t("analytics_month")} onClick={() => setSort("month")} arrow={arrow("month")} />
                <SortTh label={`${t("common_value")} USD`} onClick={() => setSort("value")} arrow={arrow("value")} />
                <SortTh label={t("common_net_kg")} onClick={() => setSort("net_kg")} arrow={arrow("net_kg")} />
                <SortTh label={t("common_rows")} onClick={() => setSort("rows")} arrow={arrow("rows")} />
                <SortTh label={t("common_declarations")} onClick={() => setSort("docs")} arrow={arrow("docs")} />
                <SortTh label={t("analytics_mom")} onClick={() => setSort("mom")} arrow={arrow("mom")} />
              </tr>
            </thead>
            <tbody>
              {sorted.map((m) => (
                <tr key={m.month} onClick={() => onMonth(m.month)} style={{ cursor: "pointer" }}>
                  <td>{formatMonth(m.month)}</td>
                  <td>{formatMoney(m.total_value_usd)}</td>
                  <td>{formatInt(m.total_net_kg)}</td>
                  <td>{formatInt(m.rows)}</td>
                  <td>{formatInt(m.declarations)}</td>
                  <td
                    style={{
                      color:
                        m.mom === null
                          ? "var(--text-faint)"
                          : m.mom >= 0
                            ? "var(--flame-amber)"
                            : "var(--flame-red)",
                    }}
                  >
                    {m.mom === null ? "—" : `${m.mom >= 0 ? "+" : ""}${formatPercent(m.mom, 0)}`}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}

// One ranked table at a time with a compact section switcher, instead of a
// tall stack of huge tables: the screen never sprawls, whatever the database
// size, and the visible table gets the full height.
function SectionGroupPanel({
  sections,
  limit,
  onLimit,
  onRow,
  hintFor,
  extraControls,
}: {
  sections: AnalyticsSection[];
  limit: number;
  onLimit: (n: number) => void;
  onRow: (section: AnalyticsSection, action: AnalyticsFilterAction) => void;
  hintFor?: (section: AnalyticsSection) => string | undefined;
  extraControls?: ReactNode;
}) {
  const { t } = useI18n();
  const withData = sections.filter((section) => section.rows.length > 0);
  const list = withData.length > 0 ? withData : sections.slice(0, 1);
  const [activeKind, setActiveKind] = useState(list[0]?.kind);
  const active = list.find((section) => section.kind === activeKind) ?? list[0];
  if (!active) {
    return <EmptyState icon="analytics" title={t("analytics_no_group_data")} />;
  }
  return (
    <div className="stack" style={{ gap: 10 }}>
      <div className="row wrap" style={{ gap: 8, justifyContent: "space-between" }}>
        <div className="row wrap" style={{ gap: 6 }}>
          {list.map((section) => (
            <button
              key={section.kind}
              className={`btn btn-sm ${active.kind === section.kind ? "" : "btn-ghost"}`}
              onClick={() => setActiveKind(section.kind)}
            >
              {sectionTitle(section.kind, t)}
              <span className="faint" style={{ marginLeft: 4 }}>
                {formatInt(section.rows.length)}
              </span>
            </button>
          ))}
        </div>
        <div className="row wrap" style={{ gap: 10 }}>
          {extraControls}
          <SectionLimitBar value={limit} onChange={onLimit} />
        </div>
      </div>
      <SectionTable
        title={sectionTitle(active.kind, t)}
        section={active}
        limit={limit}
        onRow={(action) => onRow(active, action)}
        hint={hintFor?.(active)}
        fullHeight
      />
    </div>
  );
}

type SortField = "name" | "share" | "rows" | "value" | "net_kg" | "vpk";

// A full, sortable, filterable, exportable ranking table — the "see all"
// replacement for the old top-N teaser. Sort/filter run client-side over the
// rows already fetched (up to `limit`).
function SectionTable({
  title,
  section,
  limit,
  onRow,
  hint,
  fullHeight,
}: {
  title: string;
  section: AnalyticsSection;
  limit: number;
  onRow?: (action: AnalyticsFilterAction) => void;
  hint?: string;
  fullHeight?: boolean;
}) {
  const { t } = useI18n();
  const [sortField, setSortField] = useState<SortField>("value");
  const [dir, setDir] = useState<"asc" | "desc">("desc");
  const [filter, setFilter] = useState("");

  const rows = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const list = needle
      ? section.rows.filter((r) => r.label.toLowerCase().includes(needle))
      : section.rows.slice();
    const key = (r: AnalyticsGroupRow): number | string => {
      switch (sortField) {
        case "name":
          return r.label.toLowerCase();
        case "share":
          return r.share_percent;
        case "rows":
          return r.rows;
        case "value":
          return r.total_value_usd;
        case "net_kg":
          return r.total_net_kg;
        case "vpk":
          return r.avg_value_per_net_kg;
      }
    };
    list.sort((a, b) => {
      const ka = key(a);
      const kb = key(b);
      const cmp =
        typeof ka === "string"
          ? ka.localeCompare(kb as string)
          : (ka as number) - (kb as number);
      return dir === "asc" ? cmp : -cmp;
    });
    return list;
  }, [section.rows, filter, sortField, dir]);

  const setSort = (field: SortField) => {
    if (field === sortField) {
      setDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortField(field);
      setDir(field === "name" ? "asc" : "desc");
    }
  };
  const arrow = (field: SortField) =>
    sortField === field ? (dir === "asc" ? " ▲" : " ▼") : "";

  const exportCsv = () => {
    const headers = ["Name", "Share %", "Rows", "Value USD", "Net kg", "Value/kg"];
    const data = rows.map((r) => [
      r.label,
      r.share_percent.toFixed(2),
      r.rows,
      r.total_value_usd.toFixed(2),
      r.total_net_kg.toFixed(3),
      r.avg_value_per_net_kg.toFixed(2),
    ]);
    downloadCsv(title.replace(/[^\w]+/g, "_").toLowerCase() || "section", headers, data);
  };

  const capped = section.rows.length >= limit;

  return (
    <div className="panel panel-pad">
      <div className="row wrap" style={{ justifyContent: "space-between", gap: 10 }}>
        <div className="section-title" style={{ margin: 0 }}>{title}</div>
        <div className="row" style={{ gap: 8 }}>
          <input
            className="input"
            style={{ width: 180, padding: "6px 10px" }}
            placeholder={t("sec_filter")}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          <button className="btn btn-sm btn-ghost" onClick={exportCsv} disabled={rows.length === 0}>
            <Icon name="download" size={14} /> {t("sec_export")}
          </button>
        </div>
      </div>
      {hint ? (
        <div className="faint" style={{ fontSize: 12, marginTop: 4 }}>{hint}</div>
      ) : null}
      {section.rows.length === 0 ? (
        <div className="faint" style={{ marginTop: 10 }}>{t("analytics_no_group_data")}</div>
      ) : (
        <>
          <div
            className="table-wrap"
            style={{
              maxHeight: fullHeight ? "calc(100vh - 330px)" : 460,
              marginTop: 10,
            }}
          >
            <table className="grid" style={{ width: "100%" }}>
              <thead>
                <tr>
                  <SortTh label={t("col_name")} onClick={() => setSort("name")} arrow={arrow("name")} />
                  <SortTh label={t("col_share")} onClick={() => setSort("share")} arrow={arrow("share")} />
                  <SortTh label={t("common_rows")} onClick={() => setSort("rows")} arrow={arrow("rows")} />
                  <SortTh label={`${t("common_value")} USD`} onClick={() => setSort("value")} arrow={arrow("value")} />
                  <SortTh label={t("common_net_kg")} onClick={() => setSort("net_kg")} arrow={arrow("net_kg")} />
                  <SortTh label={t("analytics_value_per_kg")} onClick={() => setSort("vpk")} arrow={arrow("vpk")} />
                </tr>
              </thead>
              <tbody>
                {rows.map((row, i) => (
                  <tr
                    key={`${row.label}-${i}`}
                    onClick={() => row.filter_action && onRow?.(row.filter_action)}
                    style={{ cursor: row.filter_action && onRow ? "pointer" : "default" }}
                  >
                    <td title={row.label} style={{ maxWidth: 320 }}>{row.label || "—"}</td>
                    <td>{formatPercent(row.share_percent)}</td>
                    <td>{formatInt(row.rows)}</td>
                    <td>{formatMoney(row.total_value_usd)}</td>
                    <td>{formatInt(row.total_net_kg)}</td>
                    <td>{formatMoney(row.avg_value_per_net_kg)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="faint" style={{ fontSize: 12, marginTop: 8 }}>
            {t("sec_shown")}: {formatInt(rows.length)}
            {capped ? ` · ${t("sec_capped")}` : ""}
          </div>
        </>
      )}
    </div>
  );
}

function SortTh({ label, onClick, arrow }: { label: string; onClick: () => void; arrow: string }) {
  return (
    <th onClick={onClick} style={{ cursor: "pointer", userSelect: "none" }} title="Sort">
      {label}
      {arrow}
    </th>
  );
}

function SectionLimitBar({ value, onChange }: { value: number; onChange: (n: number) => void }) {
  const { t } = useI18n();
  return (
    <div className="row" style={{ gap: 8 }}>
      <span className="field-label" style={{ margin: 0 }}>{t("sec_show_top")}:</span>
      {[50, 200, 500].map((n) => (
        <button
          key={n}
          className={`btn btn-sm ${value === n ? "" : "btn-ghost"}`}
          onClick={() => onChange(n)}
        >
          {n}
        </button>
      ))}
    </div>
  );
}

// The constraints currently narrowing the analytics, as removable chips — so a
// month/company drill is always visible and reversible.
function QueryChips({
  query,
  onClearText,
  onClearFilter,
  onClearAdvanced,
  onClearAll,
  onUndo,
  canUndo,
}: {
  query: Query;
  onClearText: () => void;
  onClearFilter: (key: keyof Filters) => void;
  onClearAdvanced: () => void;
  onClearAll: () => void;
  onUndo: () => void;
  canUndo: boolean;
}) {
  const { t } = useI18n();
  const filterEntries = (Object.entries(query.filters) as [keyof Filters, string][]).filter(
    ([, value]) => value.trim(),
  );
  const hasAny = !!query.text.trim() || filterEntries.length > 0 || !!query.advanced;
  if (!hasAny) return null;

  const label = (key: keyof Filters): string => {
    const map: Partial<Record<keyof Filters, string>> = {
      year: t("common_year"),
      edrpou: "EDRPOU",
      product_code: "HS",
      recipient: t("sec_recipients"),
      sender: t("sec_senders"),
      trademark: t("sec_trademarks"),
      origin_country: t("analytics_origin_countries"),
      dispatch_country: t("sec_dispatch_countries"),
      trade_country: t("sec_trade_countries"),
    };
    return map[key] ?? String(key);
  };

  return (
    <div className="panel panel-pad row wrap" style={{ gap: 8, alignItems: "center" }}>
      <span className="field-label" style={{ margin: 0 }}>{t("filter_active")}:</span>
      {query.text.trim() ? (
        <span className="chip">
          “{query.text.trim()}” <button onClick={onClearText}>×</button>
        </span>
      ) : null}
      {filterEntries.map(([key, value]) => (
        <span key={key} className="chip">
          {label(key)}: {value} <button onClick={() => onClearFilter(key)}>×</button>
        </span>
      ))}
      {query.advanced ? (
        <span className="chip">
          {t("search_advanced")} <button onClick={onClearAdvanced}>×</button>
        </span>
      ) : null}
      <div className="grow" />
      {canUndo ? (
        <button className="btn btn-sm btn-ghost" onClick={onUndo}>{t("common_undo")}</button>
      ) : null}
      <button className="btn btn-sm btn-ghost" onClick={onClearAll}>{t("filter_clear_all")}</button>
    </div>
  );
}

const SECTION_TITLE_KEYS: Record<string, MessageKey> = {
  recipients: "sec_recipients",
  senders: "sec_senders",
  edrpou: "sec_edrpou",
  product_codes: "sec_product_codes",
  trademarks: "sec_trademarks",
  product_groups: "sec_product_groups",
  origin_countries: "sec_origin_countries",
  dispatch_countries: "sec_dispatch_countries",
  trade_countries: "sec_trade_countries",
};

function sectionTitle(kind: string, t: (key: MessageKey) => string): string {
  const key = SECTION_TITLE_KEYS[kind];
  return key ? t(key) : kind;
}

const DIMS: PivotDim[] = [
  "recipient",
  "sender",
  "edrpou",
  "product_code",
  "trademark",
  "origin_country",
  "dispatch_country",
  "trade_country",
  "month",
  "year",
];

const METRIC_IDS: PivotMetric[] = ["value", "rows", "net_kg"];

function pivotDimLabel(d: PivotDim, t: (key: MessageKey) => string): string {
  const map: Record<PivotDim, string> = {
    recipient: t("sec_recipients"),
    sender: t("sec_senders"),
    edrpou: "EDRPOU",
    product_code: "HS",
    trademark: t("sec_trademarks"),
    origin_country: t("analytics_origin_countries"),
    dispatch_country: t("sec_dispatch_countries"),
    trade_country: t("sec_trade_countries"),
    month: t("analytics_month"),
    year: t("common_year"),
  };
  return map[d];
}

function pivotMetricLabel(m: PivotMetric, t: (key: MessageKey) => string): string {
  return m === "value"
    ? `${t("common_value")} USD`
    : m === "rows"
      ? t("common_rows")
      : t("common_net_kg");
}

function PivotPanel() {
  const { t } = useI18n();
  const { query } = useQueryStore();
  const [rowDim, setRowDim] = useState<PivotDim>("recipient");
  const [colDim, setColDim] = useState<PivotDim>("origin_country");
  const [metric, setMetric] = useState<PivotMetric>("value");
  const [result, setResult] = useState<PivotResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setResult(await api.pivot(query, rowDim, colDim, metric, 20, 12));
    } catch (err) {
      setError((err as ApiError)?.message ?? "Pivot failed");
    } finally {
      setLoading(false);
    }
  }, [query, rowDim, colDim, metric]);

  // A pivot must never present numbers from a query the user already changed:
  // drop the stale table the moment the query, dimensions, or metric differ.
  const builtFor = useRef<string | null>(null);
  const buildKey = `${JSON.stringify(query)}|${rowDim}|${colDim}|${metric}`;
  useEffect(() => {
    if (result && builtFor.current !== buildKey) {
      setResult(null);
    }
  }, [buildKey, result]);
  const runAndRemember = () => {
    builtFor.current = buildKey;
    run();
  };

  const fmt = (v: number) => (metric === "value" ? formatMoney(v) : formatInt(v));

  return (
    <div className="stack">
      <div className="panel panel-pad toolbar" style={{ alignItems: "flex-end" }}>
        <div>
          <label className="field-label">{t("pivot_rows")}</label>
          <select className="select" style={{ width: 170 }} value={rowDim} onChange={(e) => setRowDim(e.target.value as PivotDim)}>
            {DIMS.map((d) => (
              <option key={d} value={d}>{pivotDimLabel(d, t)}</option>
            ))}
          </select>
        </div>
        <div>
          <label className="field-label">{t("pivot_cols")}</label>
          <select className="select" style={{ width: 170 }} value={colDim} onChange={(e) => setColDim(e.target.value as PivotDim)}>
            {DIMS.map((d) => (
              <option key={d} value={d}>{pivotDimLabel(d, t)}</option>
            ))}
          </select>
        </div>
        <div>
          <label className="field-label">{t("analytics_metric")}</label>
          <select className="select" style={{ width: 150 }} value={metric} onChange={(e) => setMetric(e.target.value as PivotMetric)}>
            {METRIC_IDS.map((m) => (
              <option key={m} value={m}>{pivotMetricLabel(m, t)}</option>
            ))}
          </select>
        </div>
        <button className="btn btn-primary btn-sm" onClick={runAndRemember} disabled={loading}>
          {t("pivot_build")}
        </button>
      </div>

      {error ? <Banner>{error}</Banner> : null}
      {loading ? <Loading /> : null}

      {result && !result.cells ? (
        <Banner variant="warn">{t("pivot_mixed_units")}</Banner>
      ) : null}
      {result && result.cells ? (
        <div className="table-wrap">
          <table className="grid">
            <thead>
              <tr>
                <th style={{ position: "sticky", left: 0, zIndex: 3 }}>
                  {pivotDimLabel(rowDim, t)} \ {pivotDimLabel(colDim, t)}
                </th>
                {result.col_labels.map((c) => (
                  <th key={c} title={c}>{c || "—"}</th>
                ))}
                <th>{t("common_total")}</th>
              </tr>
            </thead>
            <tbody>
              {result.row_labels.map((rowLabel, ri) => (
                <tr key={rowLabel} style={{ cursor: "default" }}>
                  <td style={{ position: "sticky", left: 0, background: "var(--panel-solid)" }} title={rowLabel}>
                    {rowLabel || "—"}
                  </td>
                  {(result.cells?.[ri] ?? []).map((cell, ci) => (
                    <td key={ci}>{cell ? fmt(cell) : ""}</td>
                  ))}
                  <td><strong>{fmt(result.row_totals?.[ri] ?? 0)}</strong></td>
                </tr>
              ))}
              <tr style={{ cursor: "default" }}>
                <td style={{ position: "sticky", left: 0, background: "var(--panel-solid)" }}>
                  <strong>{t("common_total")}</strong>
                </td>
                {(result.col_totals ?? []).map((c, ci) => (
                  <td key={ci}><strong>{fmt(c)}</strong></td>
                ))}
                <td><strong>{fmt(result.grand_total ?? 0)}</strong></td>
              </tr>
            </tbody>
          </table>
        </div>
      ) : null}
    </div>
  );
}

// A clean working summary of the current query: headline numbers, top
// companies/goods/countries/prices, and a printable (Save as PDF) HTML report.
function ReportPanel() {
  const { t } = useI18n();
  const { query } = useQueryStore();
  const { toast } = useStore();
  const [analytics, setAnalytics] = useState<Analytics | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const queryKey = useMemo(() => JSON.stringify(query), [query]);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError(null);
    api
      .analytics(query, null, 10, 12, "sqlite", true)
      .then((res) => alive && setAnalytics(res.data))
      .catch((err) => alive && setError((err as ApiError)?.message ?? "Report failed"))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
    // Re-run only when the query changes.
  }, [queryKey]); // eslint-disable-line react-hooks/exhaustive-deps

  const titleOf = (kind: string) => sectionTitle(kind, t);

  const printReport = () => {
    if (!analytics) return;
    const win = window.open("", "_blank");
    if (!win) {
      toast(t("report_popup_blocked"), "error");
      return;
    }
    win.document.write(buildReportHtml(analytics, query, titleOf));
    win.document.close();
    win.focus();
    setTimeout(() => win.print(), 300);
  };

  const copyReport = async () => {
    if (!analytics) return;
    const ok = await copyText(buildReportText(analytics, query));
    toast(ok ? t("report_copied") : t("report_copy_failed"), ok ? "success" : "error");
  };

  if (loading && !analytics) return <Loading label={t("common_loading")} />;
  if (error) return <Banner>{error}</Banner>;
  if (!analytics) return null;
  const o = analytics.overview;

  return (
    <div className="stack">
      <div className="panel panel-pad row wrap" style={{ justifyContent: "space-between", gap: 10 }}>
        <div>
          <div className="section-title" style={{ margin: 0 }}>{t("report_title")}</div>
          <div className="faint" style={{ fontSize: 12, marginTop: 4 }}>{queryLabel(query)}</div>
        </div>
        <div className="row" style={{ gap: 8 }}>
          <button className="btn btn-sm" onClick={copyReport}>
            {t("report_copy")}
          </button>
          <button className="btn btn-sm btn-primary" onClick={printReport}>
            <Icon name="export" size={14} /> {t("report_print")}
          </button>
        </div>
      </div>

      <div className="stat-grid">
        <StatCard label={t("common_rows")} value={formatInt(o.row_count)} />
        {/* Datasets without document numbers get no permanent "0" card. */}
        {o.declaration_count > 0 ? (
          <StatCard
            label={t("common_declarations")}
            value={formatInt(o.declaration_count)}
          />
        ) : null}
        <StatCard
          label={`${t("common_value")} USD`}
          value={formatCompact(o.total_value_usd)}
          hint={formatMoney(o.total_value_usd)}
        />
        <StatCard label={t("common_net_kg")} value={formatCompact(o.total_net_kg)} />
        <StatCard label={t("analytics_value_per_kg")} value={formatMoney(o.avg_value_per_net_kg)} />
        <StatCard label={t("analytics_companies")} value={formatInt(o.distinct_edrpou)} />
      </div>

      <div className="grid-2">
        <ReportSection
          title={t("analytics_companies")}
          sections={analytics.company_sections}
          titleOf={titleOf}
        />
        <ReportSection
          title={t("analytics_products")}
          sections={analytics.product_sections}
          titleOf={titleOf}
        />
      </div>
      <div className="grid-2">
        <ReportSection
          title={t("analytics_countries")}
          sections={analytics.country_sections}
          titleOf={titleOf}
        />
        <div className="panel panel-pad">
          <div className="section-title">{t("analytics_prices")}</div>
          <PriceTable metrics={analytics.price_sections} />
        </div>
      </div>
    </div>
  );
}

function ReportSection({
  title,
  sections,
  titleOf,
}: {
  title: string;
  sections: AnalyticsSection[];
  titleOf: (kind: string) => string;
}) {
  const { t } = useI18n();
  const withRows = sections.filter((s) => s.rows.length > 0);
  return (
    <div className="panel panel-pad">
      <div className="section-title">{title}</div>
      {withRows.length === 0 ? (
        <div className="faint">{t("analytics_no_group_data")}</div>
      ) : (
        withRows.slice(0, 2).map((section) => (
          <div key={section.kind} style={{ marginBottom: 10 }}>
            <div className="faint" style={{ fontSize: 12, marginBottom: 6 }}>
              {titleOf(section.kind)}
            </div>
            {section.rows.slice(0, 5).map((row, i) => (
              <div
                key={`${row.label}-${i}`}
                className="row"
                style={{ justifyContent: "space-between", padding: "3px 0", gap: 12 }}
              >
                <span
                  title={row.label}
                  style={{
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                    maxWidth: "58%",
                  }}
                >
                  {row.label || "—"}
                </span>
                <span className="faint">
                  {formatCompact(row.total_value_usd)} · {formatPercent(row.share_percent)}
                </span>
              </div>
            ))}
          </div>
        ))
      )}
    </div>
  );
}

// Compares the current query with another product, company, or the previous
// year, side by side, with a signed difference table.
function ComparePanel() {
  const { t } = useI18n();
  const { query } = useQueryStore();
  const [withText, setWithText] = useState("");
  const [prevYear, setPrevYear] = useState(false);
  const [left, setLeft] = useState<Analytics | null>(null);
  const [right, setRight] = useState<Analytics | null>(null);
  const [leftQuery, setLeftQuery] = useState<Query | null>(null);
  const [rightQuery, setRightQuery] = useState<Query | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const hasYear = !!query.filters.year?.trim();

  // A comparison snapshot must never masquerade as the live query: when the
  // active query changes after a run, the old result disappears instead of
  // being relabeled "Current".
  const queryKey = JSON.stringify(query);
  useEffect(() => {
    if (leftQuery && JSON.stringify(leftQuery) !== queryKey) {
      setLeft(null);
      setRight(null);
      setLeftQuery(null);
      setRightQuery(null);
    }
  }, [queryKey, leftQuery]);

  const run = async () => {
    const other: Query = { ...query, filters: { ...query.filters } };
    if (withText.trim()) other.text = withText.trim();
    if (prevYear && hasYear) {
      const year = parseInt(query.filters.year.trim(), 10);
      if (Number.isNaN(year)) {
        setError(t("compare_bad_year"));
        return;
      }
      other.filters = { ...other.filters, year: String(year - 1) };
    }
    if (!withText.trim() && !(prevYear && hasYear)) {
      setError(t("compare_empty"));
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const [l, r] = await Promise.all([
        api.analytics(query, null, 10, 12, "auto"),
        api.analytics(other, null, 10, 12, "auto"),
      ]);
      setLeft(l.data);
      setRight(r.data);
      setLeftQuery(query);
      setRightQuery(other);
    } catch (err) {
      setError((err as ApiError)?.message ?? "Compare failed");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="stack">
      <div className="panel panel-pad stack" style={{ gap: 12 }}>
        <div className="faint" style={{ fontSize: 12 }}>{t("compare_hint")}</div>
        <div className="row wrap" style={{ gap: 10 }}>
          <input
            className="input"
            style={{ maxWidth: 280 }}
            placeholder={t("compare_with")}
            value={withText}
            onChange={(e) => setWithText(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && run()}
          />
          {hasYear ? (
            <label className="row" style={{ gap: 6, cursor: "pointer" }}>
              <input
                type="checkbox"
                checked={prevYear}
                onChange={(e) => setPrevYear(e.target.checked)}
              />
              <span className="faint">{t("compare_prev_year")}</span>
            </label>
          ) : null}
          <button className="btn btn-primary btn-sm" onClick={run} disabled={loading}>
            {t("compare_run")}
          </button>
        </div>
      </div>

      {error ? <Banner>{error}</Banner> : null}
      {loading ? <Loading /> : null}

      {left && right && leftQuery && rightQuery ? (
        <>
          <div className="grid-2">
            <CompareCard title={t("compare_current")} label={queryLabel(leftQuery)} data={left} />
            <CompareCard title={t("compare_other")} label={queryLabel(rightQuery)} data={right} />
          </div>
          <div className="panel panel-pad">
            <div className="section-title">{t("compare_difference")}</div>
            <div className="table-wrap" style={{ maxHeight: "none" }}>
              <table className="grid" style={{ width: "100%" }}>
                <thead>
                  <tr>
                    <th>{t("analytics_metric")}</th>
                    <th>{t("compare_current")}</th>
                    <th>{t("compare_other")}</th>
                    <th>Δ</th>
                  </tr>
                </thead>
                <tbody>
                  <CompareRow label={t("common_rows")} a={left.overview.row_count} b={right.overview.row_count} kind="int" />
                  <CompareRow label={t("common_declarations")} a={left.overview.declaration_count} b={right.overview.declaration_count} kind="int" />
                  <CompareRow label={`${t("common_value")} USD`} a={left.overview.total_value_usd} b={right.overview.total_value_usd} kind="money" />
                  <CompareRow label={t("common_net_kg")} a={left.overview.total_net_kg} b={right.overview.total_net_kg} kind="int" />
                  <CompareRow label={t("analytics_value_per_kg")} a={left.overview.avg_value_per_net_kg} b={right.overview.avg_value_per_net_kg} kind="money" />
                  <CompareRow label={t("analytics_companies")} a={left.overview.distinct_edrpou} b={right.overview.distinct_edrpou} kind="int" />
                </tbody>
              </table>
            </div>
          </div>
        </>
      ) : null}
    </div>
  );
}

function CompareCard({ title, label, data }: { title: string; label: string; data: Analytics }) {
  const { t } = useI18n();
  const o = data.overview;
  const line = (name: string, value: string) => (
    <div className="row" style={{ justifyContent: "space-between", padding: "3px 0" }}>
      <span className="faint">{name}</span>
      <strong>{value}</strong>
    </div>
  );
  return (
    <div className="panel panel-pad">
      <div className="section-title" style={{ marginBottom: 4 }}>{title}</div>
      <div className="faint" style={{ fontSize: 12, marginBottom: 10 }} title={label}>
        {label}
      </div>
      {line(t("common_rows"), formatInt(o.row_count))}
      {line(`${t("common_value")} USD`, formatCompact(o.total_value_usd))}
      {line(t("common_net_kg"), formatCompact(o.total_net_kg))}
      {line(t("analytics_value_per_kg"), formatMoney(o.avg_value_per_net_kg))}
    </div>
  );
}

function CompareRow({
  label,
  a,
  b,
  kind,
}: {
  label: string;
  a: number;
  b: number;
  kind: "int" | "money";
}) {
  const fmt = kind === "money" ? formatMoney : formatInt;
  const delta = b - a;
  const pct = Math.abs(a) > 1e-9 ? (delta / a) * 100 : null;
  const color =
    delta === 0 ? "var(--text-faint)" : delta > 0 ? "var(--flame-amber)" : "var(--flame-red)";
  return (
    <tr style={{ cursor: "default" }}>
      <td>{label}</td>
      <td>{fmt(a)}</td>
      <td>{fmt(b)}</td>
      <td style={{ color }}>
        {delta >= 0 ? "+" : ""}
        {fmt(delta)}
        {pct !== null ? ` (${delta >= 0 ? "+" : ""}${formatPercent(pct, 0)})` : ""}
      </td>
    </tr>
  );
}
